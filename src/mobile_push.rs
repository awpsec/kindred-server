//! Native mobile push. Registrations live in each profile database and are bound
//! to the account session that created them. One gateway worker turns the
//! profile's existing notification feed into a bounded outbox and delivers generic
//! alerts directly to APNs or FCM. Push payloads never carry chat content or the
//! server address; native clients map `account_id` to their saved backend.
use crate::db::{Db, now};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

pub const MAX_DEVICES_PER_PROFILE: i64 = 32;
/// Events older than this when first observed are never pushed, so a restart,
/// a late configuration change or a stale cursor cannot replay history.
pub const MAX_EVENT_AGE: i64 = 15 * 60;
/// An undelivered alert is discarded after this long or this many attempts.
pub const MAX_ATTEMPTS: i64 = 8;
pub const MAX_OUTBOX_AGE: i64 = 60 * 60;
const MAX_OUTBOX_ROWS: i64 = 512;
const FEED_PAGES_PER_TICK: usize = 5;
const SENDS_PER_TICK: i64 = 32;
const PARALLEL_SENDS: usize = 8;
pub const ALERT_TITLE: &str = "Kindred";
pub const ALERT_BODY: &str = "You have a new update.";

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS mobile_devices(installation TEXT PRIMARY KEY,platform TEXT NOT NULL,token TEXT NOT NULL,environment TEXT NOT NULL,account_id TEXT NOT NULL,session_digest BLOB NOT NULL,created INTEGER NOT NULL,updated INTEGER NOT NULL,UNIQUE(platform,token));
      CREATE INDEX IF NOT EXISTS mobile_device_session ON mobile_devices(session_digest);
      CREATE TABLE IF NOT EXISTS mobile_push_cursor(id INTEGER PRIMARY KEY CHECK(id=1),cursor INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS mobile_push_outbox(installation TEXT NOT NULL REFERENCES mobile_devices(installation) ON DELETE CASCADE,event_id INTEGER NOT NULL,chat_id TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,next_attempt INTEGER NOT NULL,created INTEGER NOT NULL,PRIMARY KEY(installation,event_id));
      CREATE INDEX IF NOT EXISTS mobile_push_due ON mobile_push_outbox(next_attempt);")?;
    let since: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('mobile_devices') WHERE name='since_event')", [], |r| r.get(0))?;
    if !since {
        // Earlier development rows have no join point; start them at the present.
        c.execute("ALTER TABLE mobile_devices ADD COLUMN since_event INTEGER NOT NULL DEFAULT 0", [])?;
        c.execute("UPDATE mobile_devices SET since_event=(SELECT COALESCE(MAX(seq),0) FROM events)", [])?;
    }
    let profile: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('mobile_devices') WHERE name='profile_id')", [], |r| r.get(0))?;
    if !profile {
        c.execute("ALTER TABLE mobile_devices ADD COLUMN profile_id TEXT NOT NULL DEFAULT ''", [])?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    pub installation: String,
    pub platform: String,
    pub token: String,
    pub environment: String,
    pub account_id: String,
}

impl Registration {
    /// Validates the native client contract. `account` is the authenticated
    /// account; a client cannot register a device for another account.
    pub fn parse(installation: &str, body: &Value, account: &str) -> Result<Self> {
        let installation = uuid::Uuid::parse_str(installation)
            .ok()
            .filter(|_| installation.len() == 36)
            .context("Invalid installation ID")?
            .hyphenated()
            .to_string();
        let object = body.as_object().context("Invalid device registration")?;
        ensure!(
            object
                .keys()
                .all(|k| matches!(k.as_str(), "platform" | "token" | "environment" | "account_id")),
            "Unknown device registration field"
        );
        let text = |key: &str| body[key].as_str().unwrap_or("");
        let platform = text("platform");
        ensure!(matches!(platform, "android" | "ios"), "Invalid push platform");
        let environment = text("environment");
        ensure!(
            matches!(environment, "production" | "sandbox"),
            "Invalid push environment"
        );
        let token = text("token");
        let valid = if platform == "ios" {
            (64..=200).contains(&token.len()) && token.len() % 2 == 0 && token.bytes().all(|b| b.is_ascii_hexdigit())
        } else {
            (16..=4096).contains(&token.len())
                && token.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_:.".contains(&b))
        };
        ensure!(valid, "Invalid push token");
        let account_id = uuid::Uuid::parse_str(text("account_id"))
            .ok()
            .filter(|_| text("account_id").len() == 36)
            .context("Invalid account ID")?
            .hyphenated()
            .to_string();
        ensure!(
            !account.is_empty() && account_id == account,
            "This device registration belongs to a different account"
        );
        Ok(Self {
            installation,
            platform: platform.into(),
            token: if platform == "ios" { token.to_ascii_lowercase() } else { token.into() },
            environment: environment.into(),
            account_id,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Device {
    pub installation: String,
    pub platform: String,
    pub account_id: String,
    pub session_digest: Vec<u8>,
    /// Latest event when this installation joined; it never receives earlier events.
    pub since_event: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Platforms {
    pub ios: bool,
    pub android: bool,
}
impl Platforms {
    pub fn supports(&self, platform: &str) -> bool {
        match platform {
            "ios" => self.ios,
            "android" => self.android,
            _ => false,
        }
    }
    pub fn any(&self) -> bool {
        self.ios || self.android
    }
}

fn latest_event(c: &Connection) -> rusqlite::Result<i64> {
    c.query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| r.get(0))
}

impl Db {
    /// `profile` is the ID of the profile owning this database; it is returned in
    /// pushes so the app can switch to that profile before opening the chat.
    pub fn register_mobile_device(&self, r: &Registration, session_digest: &[u8], profile: &str) -> Result<()> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let existing: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM mobile_devices WHERE installation=?)",
            [&r.installation],
            |row| row.get(0),
        )?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM mobile_devices", [], |row| row.get(0))?;
        ensure!(
            existing || count < MAX_DEVICES_PER_PROFILE,
            "Too many mobile devices are registered for this profile. Remove an old device first."
        );
        // A token moves with the app installation that currently owns it.
        tx.execute(
            "DELETE FROM mobile_devices WHERE platform=? AND token=? AND installation!=?",
            params![r.platform, r.token, r.installation],
        )?;
        // A new installation joins at the present, even when other devices already
        // share the profile cursor. Refreshing an existing one keeps its join point.
        let latest = latest_event(&tx)?;
        tx.execute("INSERT INTO mobile_devices(installation,platform,token,environment,account_id,session_digest,created,updated,since_event,profile_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?7,?8,?9) ON CONFLICT(installation) DO UPDATE SET platform=excluded.platform,token=excluded.token,environment=excluded.environment,account_id=excluded.account_id,session_digest=excluded.session_digest,updated=excluded.updated,profile_id=excluded.profile_id",
            params![r.installation, r.platform, r.token, r.environment, r.account_id, session_digest, now(), latest, profile])?;
        tx.execute("INSERT OR IGNORE INTO mobile_push_cursor VALUES(1,?)", [latest])?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove_mobile_device(&self, installation: &str) -> Result<bool> {
        let c = self.0.lock().unwrap();
        let removed = c.execute("DELETE FROM mobile_devices WHERE installation=?", [installation])? > 0;
        reset_when_empty(&c)?;
        Ok(removed)
    }

    /// Removes registrations created by any of these sessions (sign-out).
    pub fn remove_mobile_sessions(&self, digests: &[Vec<u8>]) -> Result<usize> {
        let c = self.0.lock().unwrap();
        let mut removed = 0;
        for digest in digests {
            removed += c.execute("DELETE FROM mobile_devices WHERE session_digest=?", [digest])?;
        }
        reset_when_empty(&c)?;
        Ok(removed)
    }

    pub fn remove_mobile_account(&self, account: &str) -> Result<usize> {
        let c = self.0.lock().unwrap();
        let removed = c.execute("DELETE FROM mobile_devices WHERE account_id=?", [account])?;
        reset_when_empty(&c)?;
        Ok(removed)
    }

    /// Keeps a device registered when its own session is rotated in place.
    pub fn rebind_mobile_session(&self, old: &[u8], new: &[u8]) -> Result<usize> {
        Ok(self.0.lock().unwrap().execute(
            "UPDATE mobile_devices SET session_digest=? WHERE session_digest=?",
            params![new, old],
        )?)
    }

    pub fn mobile_devices(&self) -> Result<Vec<Device>> {
        Ok(self.0.lock().unwrap()
            .prepare("SELECT installation,platform,account_id,session_digest,since_event FROM mobile_devices ORDER BY created,installation")?
            .query_map([], |r| Ok(Device { installation: r.get(0)?, platform: r.get(1)?, account_id: r.get(2)?, session_digest: r.get(3)?, since_event: r.get(4)? }))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Registrations of one account with their session binding, for copying
    /// into a profile the account creates later.
    pub fn mobile_registrations(&self, account: &str) -> Result<Vec<(Registration, Vec<u8>)>> {
        Ok(self.0.lock().unwrap()
            .prepare("SELECT installation,platform,token,environment,account_id,session_digest FROM mobile_devices WHERE account_id=? ORDER BY created,installation")?
            .query_map([account], |r| Ok((Registration { installation: r.get(0)?, platform: r.get(1)?, token: r.get(2)?, environment: r.get(3)?, account_id: r.get(4)? }, r.get(5)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn mobile_device_registered(&self, installation: &str) -> Result<bool> {
        Ok(self.0.lock().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM mobile_devices WHERE installation=?)",
            [installation],
            |r| r.get(0),
        )?)
    }
}

/// With no devices left, the next registration starts from the present again.
fn reset_when_empty(c: &Connection) -> Result<()> {
    let empty: bool = c.query_row("SELECT NOT EXISTS(SELECT 1 FROM mobile_devices)", [], |r| r.get(0))?;
    if empty {
        c.execute("DELETE FROM mobile_push_cursor", [])?;
        c.execute("DELETE FROM mobile_push_outbox", [])?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Push {
    pub installation: String,
    pub platform: String,
    pub token: String,
    pub environment: String,
    pub account_id: String,
    pub profile_id: String,
    pub chat_id: String,
    pub event_id: i64,
}
impl Push {
    pub fn data(&self) -> Value {
        json!({"account_id":self.account_id,"profile_id":self.profile_id,"installation_uuid":self.installation,"chat_id":self.chat_id,"event_id":self.event_id.to_string()})
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    /// Transient provider, network or credential failure.
    Retry { after: Option<i64> },
    /// The provider says this device token can never receive pushes again.
    InvalidToken,
    /// The provider rejected this message permanently.
    Rejected,
}

pub type SendFuture<'a> = Pin<Box<dyn Future<Output = Outcome> + Send + 'a>>;
pub trait Transport: Send + Sync {
    fn platforms(&self) -> Platforms;
    fn send<'a>(&'a self, push: &'a Push) -> SendFuture<'a>;
}

/// Drops registrations whose session has ended, then moves new feed items into
/// the outbox. Returns the number of queued alerts.
pub fn collect(db: &Db, live: &dyn Fn(&Device) -> Result<bool>, platforms: Platforms) -> Result<usize> {
    let devices = db.mobile_devices()?;
    if devices.is_empty() {
        return Ok(0);
    }
    let mut active = Vec::new();
    let mut ended = Vec::new();
    for device in devices {
        // Errors keep the registration; only a definite "not live" removes it.
        match live(&device) {
            Ok(true) => active.push(device),
            Ok(false) => ended.push(device.session_digest.clone()),
            Err(error) => return Err(error.context("check mobile session")),
        }
    }
    if !ended.is_empty() {
        db.remove_mobile_sessions(&ended)?;
    }
    let targets: Vec<&Device> = active.iter().filter(|d| platforms.supports(&d.platform)).collect();
    let mut queued = 0;
    for _ in 0..FEED_PAGES_PER_TICK {
        let cutoff = now() - MAX_EVENT_AGE;
        let cursor = {
            let c = db.0.lock().unwrap();
            let Some(cursor) = c
                .query_row("SELECT cursor FROM mobile_push_cursor WHERE id=1", [], |r| r.get::<_, i64>(0))
                .optional()?
            else {
                let latest = latest_event(&c)?;
                c.execute("INSERT OR IGNORE INTO mobile_push_cursor VALUES(1,?)", [latest])?;
                return Ok(queued);
            };
            // Skip directly past events that are already too old to announce.
            let fresh: Option<i64> = c
                .query_row("SELECT seq FROM events WHERE seq>? AND created>=? ORDER BY seq LIMIT 1", params![cursor, cutoff], |r| r.get(0))
                .optional()?;
            let skip_to = match fresh {
                Some(seq) => seq - 1,
                None => latest_event(&c)?,
            };
            if skip_to > cursor {
                c.execute("UPDATE mobile_push_cursor SET cursor=? WHERE id=1 AND cursor=?", params![skip_to, cursor])?;
                skip_to
            } else {
                cursor
            }
        };
        let feed = db.notifications(Some(cursor))?;
        let next = feed["cursor"].as_i64().unwrap_or(cursor);
        let items = feed["items"].as_array().cloned().unwrap_or_default();
        let mut c = db.0.lock().unwrap();
        let tx = c.transaction()?;
        // Compare-and-set keeps a concurrent tick or reset from queueing twice.
        if tx.execute("UPDATE mobile_push_cursor SET cursor=? WHERE id=1 AND cursor=?", params![next.max(cursor), cursor])? != 1 {
            return Ok(queued);
        }
        for item in &items {
            let (Some(event), Some(chat)) = (item["id"].as_i64(), item["chat_id"].as_str()) else { continue };
            // This profile's own bot results in shared rooms are pushed too;
            // other members' shared-room messages are not in this private feed.
            let created: i64 = tx.query_row("SELECT created FROM events WHERE seq=?", [event], |r| r.get(0))?;
            if created < cutoff {
                continue;
            }
            for device in targets.iter().filter(|d| event > d.since_event) {
                // The device may have been removed or re-added since the list was read.
                queued += tx.execute(
                    "INSERT OR IGNORE INTO mobile_push_outbox(installation,event_id,chat_id,next_attempt,created) SELECT ?1,?2,?3,?4,?4 WHERE EXISTS(SELECT 1 FROM mobile_devices WHERE installation=?1 AND since_event<?2)",
                    params![device.installation, event, chat, now()],
                )?;
            }
        }
        tx.execute("DELETE FROM mobile_push_outbox WHERE rowid IN (SELECT rowid FROM mobile_push_outbox ORDER BY created DESC,event_id DESC LIMIT -1 OFFSET ?)", [MAX_OUTBOX_ROWS])?;
        tx.commit()?;
        if next <= cursor || next >= latest_event(&c)? {
            break;
        }
    }
    Ok(queued)
}

/// Whether the notification feed still announces this event now. The feed
/// applies current mutes, preferences and resolution of questions/approvals.
pub fn still_eligible(db: &Db, event_id: i64) -> Result<bool> {
    let feed = db.notifications(Some(event_id - 1))?;
    Ok(feed["items"].as_array().into_iter().flatten().any(|item| item["id"].as_i64() == Some(event_id)))
}

/// Returns due alerts, discarding those that have expired. Retries are checked
/// against the current feed first, so a delayed alert is never sent after the
/// user muted the chat, changed preferences or already answered.
pub fn due(db: &Db, platforms: Platforms) -> Result<Vec<Push>> {
    let rows = {
        let c = db.0.lock().unwrap();
        let expired = c.execute(
            "DELETE FROM mobile_push_outbox WHERE created<? OR attempts>=?",
            params![now() - MAX_OUTBOX_AGE, MAX_ATTEMPTS],
        )?;
        if expired > 0 {
            eprintln!("Mobile push discarded {expired} undeliverable alert(s)");
        }
        c.prepare("SELECT o.installation,d.platform,d.token,d.environment,d.account_id,o.chat_id,o.event_id,o.attempts,d.profile_id FROM mobile_push_outbox o JOIN mobile_devices d ON d.installation=o.installation WHERE o.next_attempt<=? ORDER BY o.next_attempt,o.event_id LIMIT ?")?
            .query_map(params![now(), SENDS_PER_TICK], |r| {
                Ok((Push { installation: r.get(0)?, platform: r.get(1)?, token: r.get(2)?, environment: r.get(3)?, account_id: r.get(4)?, profile_id: r.get(8)?, chat_id: r.get(5)?, event_id: r.get(6)? }, r.get::<_, i64>(7)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut checked = std::collections::HashMap::new();
    let mut pushes = Vec::new();
    for (push, attempts) in rows {
        if !platforms.supports(&push.platform) {
            continue;
        }
        if attempts > 0 {
            let eligible = match checked.get(&push.event_id) {
                Some(eligible) => *eligible,
                None => {
                    let eligible = still_eligible(db, push.event_id)?;
                    checked.insert(push.event_id, eligible);
                    eligible
                }
            };
            if !eligible {
                db.0.lock().unwrap().execute("DELETE FROM mobile_push_outbox WHERE installation=? AND event_id=?", params![push.installation, push.event_id])?;
                continue;
            }
        }
        pushes.push(push);
    }
    Ok(pushes)
}

pub fn backoff(attempts: i64) -> i64 {
    (10i64 << attempts.clamp(0, 10)).min(1800)
}

pub fn record(db: &Db, push: &Push, outcome: Outcome) -> Result<()> {
    let c = db.0.lock().unwrap();
    let key = params![push.installation, push.event_id];
    match outcome {
        Outcome::Delivered | Outcome::Rejected => {
            c.execute("DELETE FROM mobile_push_outbox WHERE installation=? AND event_id=?", key)?;
        }
        Outcome::Retry { after } => {
            let attempts: i64 = c
                .query_row("SELECT attempts FROM mobile_push_outbox WHERE installation=? AND event_id=?", key, |r| r.get(0))
                .optional()?
                .unwrap_or(MAX_ATTEMPTS);
            let attempts = attempts + 1;
            if attempts >= MAX_ATTEMPTS {
                c.execute("DELETE FROM mobile_push_outbox WHERE installation=? AND event_id=?", key)?;
            } else {
                let delay = backoff(attempts).max(after.unwrap_or(0).clamp(0, MAX_OUTBOX_AGE));
                c.execute("UPDATE mobile_push_outbox SET attempts=?,next_attempt=? WHERE installation=? AND event_id=?",
                    params![attempts, now() + delay, push.installation, push.event_id])?;
            }
        }
        Outcome::InvalidToken => {
            // Only the token that failed; a fresh registration may have replaced it.
            c.execute("DELETE FROM mobile_devices WHERE installation=? AND token=?", params![push.installation, push.token])?;
            reset_when_empty(&c)?;
        }
    }
    Ok(())
}

/// Sends due alerts with bounded parallelism and records each outcome.
pub async fn deliver(db: &Db, transport: Arc<dyn Transport>) -> Result<usize> {
    let pushes = due(db, transport.platforms())?;
    let mut sent = 0;
    let mut failure = None;
    for chunk in pushes.chunks(PARALLEL_SENDS) {
        let mut set = tokio::task::JoinSet::new();
        for push in chunk.iter().cloned() {
            let transport = transport.clone();
            set.spawn(async move {
                let outcome = tokio::time::timeout(Duration::from_secs(30), transport.send(&push))
                    .await
                    .unwrap_or(Outcome::Retry { after: None });
                (push, outcome)
            });
        }
        while let Some(result) = set.join_next().await {
            let Ok((push, outcome)) = result else { continue };
            if outcome == Outcome::Delivered {
                sent += 1;
            }
            // Keep recording the rest; dropping in-flight sends could duplicate them.
            if let Err(error) = record(db, &push, outcome) {
                failure.get_or_insert(error);
            }
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(sent),
    }
}

// ---- Provider configuration -------------------------------------------------

pub struct Settings {
    pub apns: Option<ApnsSettings>,
    pub fcm: Option<FcmSettings>,
}

#[derive(Clone)]
pub struct ApnsSettings {
    pub key_id: String,
    pub team_id: String,
    pub topic: String,
    key_pkcs8: Vec<u8>,
}

#[derive(Clone)]
pub struct FcmSettings {
    pub project_id: String,
    pub client_email: String,
    key_pkcs8: Vec<u8>,
}

fn pem_body(text: &str, label: &str) -> Result<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = text.find(&begin).context("Key file is not a PEM private key")? + begin.len();
    let stop = text[start..].find(&end).context("Key file is not a PEM private key")? + start;
    let encoded: String = text[start..stop].chars().filter(|c| !c.is_whitespace()).collect();
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.decode(encoded)?)
}

fn identifier(value: &str, len: std::ops::RangeInclusive<usize>, extra: &[u8]) -> bool {
    len.contains(&value.len()) && value.bytes().all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
}

fn read_private_file(path: &str) -> Result<String> {
    ensure!(path.starts_with('/'), "Push credential paths must be absolute");
    let meta = std::fs::metadata(path).with_context(|| format!("read {path}"))?;
    ensure!(meta.is_file() && meta.len() <= 64 * 1024, "Push credential file {path} is not a small regular file");
    Ok(std::fs::read_to_string(path)?)
}

impl ApnsSettings {
    pub fn new(key_pem: &str, key_id: &str, team_id: &str, topic: &str) -> Result<Self> {
        ensure!(identifier(key_id, 10..=10, b""), "APNs key ID must be the 10-character key identifier");
        ensure!(identifier(team_id, 10..=10, b""), "APNs team ID must be the 10-character team identifier");
        ensure!(identifier(topic, 1..=255, b".-") && topic.contains('.'), "APNs topic must be the app bundle identifier");
        let key_pkcs8 = pem_body(key_pem, "PRIVATE KEY")?;
        ring::signature::EcdsaKeyPair::from_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING, &key_pkcs8, &ring::rand::SystemRandom::new())
            .map_err(|_| anyhow::anyhow!("APNs key must be an ES256 (P-256) .p8 key"))?;
        Ok(Self { key_id: key_id.into(), team_id: team_id.into(), topic: topic.into(), key_pkcs8 })
    }
}

impl FcmSettings {
    pub fn new(service_account: &str) -> Result<Self> {
        let v: Value = serde_json::from_str(service_account).context("FCM service account is not JSON")?;
        ensure!(v["type"] == "service_account", "FCM credentials must be a Google service account key");
        let project_id = v["project_id"].as_str().unwrap_or("");
        ensure!(identifier(project_id, 4..=63, b"-") && project_id.bytes().all(|b| !b.is_ascii_uppercase()), "Invalid FCM project ID");
        let client_email = v["client_email"].as_str().unwrap_or("");
        ensure!(identifier(client_email, 3..=320, b"@.-_") && client_email.contains('@'), "Invalid FCM service account email");
        let key_pkcs8 = pem_body(v["private_key"].as_str().unwrap_or(""), "PRIVATE KEY")?;
        ring::signature::RsaKeyPair::from_pkcs8(&key_pkcs8)
            .map_err(|_| anyhow::anyhow!("FCM service account private key is not a valid RSA key"))?;
        Ok(Self { project_id: project_id.into(), client_email: client_email.into(), key_pkcs8 })
    }
}

impl Settings {
    pub fn platforms(&self) -> Platforms {
        Platforms { ios: self.apns.is_some(), android: self.fcm.is_some() }
    }
    /// Reads optional credentials named by environment variables. A platform
    /// with incomplete or invalid settings stays disabled and is reported once.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let apns_vars = ["KINDRED_APNS_KEY_FILE", "KINDRED_APNS_KEY_ID", "KINDRED_APNS_TEAM_ID", "KINDRED_APNS_TOPIC"].map(var);
        let apns = if apns_vars.iter().all(Option::is_none) {
            None
        } else {
            let [file, key, team, topic] = apns_vars.map(Option::unwrap_or_default);
            match read_private_file(&file).and_then(|pem| ApnsSettings::new(&pem, &key, &team, &topic)) {
                Ok(settings) => Some(settings),
                Err(error) => {
                    eprintln!("APNs push is disabled: {error:#}");
                    None
                }
            }
        };
        let fcm = var("KINDRED_FCM_SERVICE_ACCOUNT_FILE").and_then(|file| {
            match read_private_file(&file).and_then(|json| FcmSettings::new(&json)) {
                Ok(settings) => Some(settings),
                Err(error) => {
                    eprintln!("FCM push is disabled: {error:#}");
                    None
                }
            }
        });
        Self { apns, fcm }
    }
}

// ---- Provider transports ----------------------------------------------------

fn b64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn apns_jwt(s: &ApnsSettings, issued: i64) -> Result<String> {
    let key = ring::signature::EcdsaKeyPair::from_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING, &s.key_pkcs8, &ring::rand::SystemRandom::new())
        .map_err(|_| anyhow::anyhow!("Invalid APNs key"))?;
    let input = format!(
        "{}.{}",
        b64url(json!({"alg":"ES256","kid":s.key_id}).to_string().as_bytes()),
        b64url(json!({"iss":s.team_id,"iat":issued}).to_string().as_bytes())
    );
    let signature = key
        .sign(&ring::rand::SystemRandom::new(), input.as_bytes())
        .map_err(|_| anyhow::anyhow!("Could not sign APNs token"))?;
    Ok(format!("{input}.{}", b64url(signature.as_ref())))
}

pub fn fcm_assertion(s: &FcmSettings, issued: i64) -> Result<String> {
    let key = ring::signature::RsaKeyPair::from_pkcs8(&s.key_pkcs8).map_err(|_| anyhow::anyhow!("Invalid FCM key"))?;
    let input = format!(
        "{}.{}",
        b64url(json!({"alg":"RS256","typ":"JWT"}).to_string().as_bytes()),
        b64url(json!({"iss":s.client_email,"scope":FCM_SCOPE,"aud":GOOGLE_TOKEN_URL,"iat":issued,"exp":issued+3600}).to_string().as_bytes())
    );
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(&ring::signature::RSA_PKCS1_SHA256, &ring::rand::SystemRandom::new(), input.as_bytes(), &mut signature)
        .map_err(|_| anyhow::anyhow!("Could not sign FCM assertion"))?;
    Ok(format!("{input}.{}", b64url(&signature)))
}

const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";

pub fn apns_host(environment: &str) -> &'static str {
    if environment == "sandbox" { "api.sandbox.push.apple.com" } else { "api.push.apple.com" }
}

pub fn apns_payload(push: &Push) -> Value {
    json!({"aps":{"alert":{"title":ALERT_TITLE,"body":ALERT_BODY},"sound":"kindred-pop.wav","thread-id":push.chat_id},
        "account_id":push.account_id,"profile_id":push.profile_id,"installation_uuid":push.installation,"chat_id":push.chat_id,"event_id":push.event_id.to_string()})
}

pub fn fcm_message(push: &Push) -> Value {
    json!({"message":{"token":push.token,
        "notification":{"title":ALERT_TITLE,"body":ALERT_BODY},
        "data":push.data(),
        "android":{"priority":"HIGH","ttl":"3600s","notification":{"channel_id":"kindred_updates"}}}})
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<i64> {
    headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim().parse().ok()
}

/// Classifies an APNs response. `auth` is true when the provider token itself was refused.
pub fn apns_outcome(status: u16, body: &str, retry: Option<i64>) -> (Outcome, bool) {
    let reason = serde_json::from_str::<Value>(body).ok().and_then(|v| v["reason"].as_str().map(str::to_owned)).unwrap_or_default();
    match (status, reason.as_str()) {
        (200, _) => (Outcome::Delivered, false),
        (410, _) | (400, "BadDeviceToken" | "DeviceTokenNotForTopic") => (Outcome::InvalidToken, false),
        (403, _) => (Outcome::Retry { after: retry }, true),
        (429, _) | (500..=599, _) => (Outcome::Retry { after: retry }, false),
        _ => (Outcome::Rejected, false),
    }
}

pub fn fcm_outcome(status: u16, body: &str, retry: Option<i64>) -> (Outcome, bool) {
    let v: Value = serde_json::from_str(body).unwrap_or_default();
    let error = &v["error"];
    let details = error["details"].as_array().cloned().unwrap_or_default();
    let code = details.iter().find_map(|d| d["errorCode"].as_str().map(str::to_owned)).unwrap_or_default();
    let token_field = details.iter().any(|d| {
        d["fieldViolations"].as_array().into_iter().flatten().any(|f| f["field"].as_str().is_some_and(|f| f.contains("token")))
    }) || error["message"].as_str().is_some_and(|m| m.to_ascii_lowercase().contains("registration token"));
    match status {
        200 => (Outcome::Delivered, false),
        404 => (Outcome::InvalidToken, false),
        400 if code == "UNREGISTERED" || token_field => (Outcome::InvalidToken, false),
        403 if code == "SENDER_ID_MISMATCH" => (Outcome::InvalidToken, false),
        401 | 403 => (Outcome::Retry { after: retry }, true),
        429 | 500..=599 => (Outcome::Retry { after: retry }, false),
        _ => (Outcome::Rejected, false),
    }
}

pub struct Providers {
    apns: Option<ApnsSettings>,
    fcm: Option<FcmSettings>,
    apns_client: reqwest::Client,
    http: reqwest::Client,
    apns_token: std::sync::Mutex<Option<(String, i64)>>,
    fcm_token: tokio::sync::Mutex<Option<(String, i64)>>,
}

impl Providers {
    pub fn new(settings: Settings) -> Result<Self> {
        let base = || {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(20))
                .user_agent(concat!("Kindred/", env!("CARGO_PKG_VERSION")))
        };
        Ok(Self {
            apns: settings.apns,
            fcm: settings.fcm,
            apns_client: base().http2_prior_knowledge().build()?,
            http: base().build()?,
            apns_token: Default::default(),
            fcm_token: Default::default(),
        })
    }

    fn apns_bearer(&self, s: &ApnsSettings) -> Result<String> {
        let mut cached = self.apns_token.lock().unwrap();
        // Apple accepts provider tokens for an hour and throttles frequent refreshes.
        if let Some((token, issued)) = cached.as_ref() {
            if now() - issued < 40 * 60 {
                return Ok(token.clone());
            }
        }
        let issued = now();
        let token = apns_jwt(s, issued)?;
        *cached = Some((token.clone(), issued));
        Ok(token)
    }

    async fn fcm_bearer(&self, s: &FcmSettings) -> Result<String> {
        let mut cached = self.fcm_token.lock().await;
        if let Some((token, expires)) = cached.as_ref() {
            if now() < *expires {
                return Ok(token.clone());
            }
        }
        let assertion = fcm_assertion(s, now())?;
        let response = self
            .http
            .post(GOOGLE_TOKEN_URL)
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(format!("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={assertion}"))
            .send()
            .await?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or_default();
        ensure!(status.is_success(), "Google token exchange returned {status}");
        let token = body["access_token"].as_str().filter(|t| !t.is_empty() && t.len() <= 4096).context("Google token exchange returned no access token")?;
        let lifetime = body["expires_in"].as_i64().unwrap_or(3600).clamp(60, 3600);
        *cached = Some((token.to_owned(), now() + lifetime - 60.min(lifetime / 2)));
        Ok(token.to_owned())
    }

    async fn send_apns(&self, s: &ApnsSettings, push: &Push) -> Outcome {
        let Ok(bearer) = self.apns_bearer(s) else { return Outcome::Retry { after: None } };
        let url = format!("https://{}/3/device/{}", apns_host(&push.environment), push.token);
        let response = self
            .apns_client
            .post(url)
            .bearer_auth(bearer)
            .header("apns-topic", &s.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .header("apns-expiration", (now() + 3600).to_string())
            .json(&apns_payload(push))
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                eprintln!("APNs request failed: {}", error.without_url());
                return Outcome::Retry { after: None };
            }
        };
        let status = response.status().as_u16();
        let retry = retry_after(response.headers());
        let body = response.text().await.unwrap_or_default();
        let (outcome, auth) = apns_outcome(status, &body, retry);
        if auth {
            // Apple throttles provider tokens refreshed more than every 20 minutes.
            let mut cached = self.apns_token.lock().unwrap();
            if cached.as_ref().is_some_and(|(_, issued)| now() - issued >= 20 * 60) {
                *cached = None;
            }
            drop(cached);
            eprintln!("APNs refused the provider token ({status}); check the key, key ID, team ID and topic");
        } else if outcome == Outcome::Rejected {
            eprintln!("APNs rejected an alert ({status})");
        }
        outcome
    }

    async fn send_fcm(&self, s: &FcmSettings, push: &Push) -> Outcome {
        let bearer = match self.fcm_bearer(s).await {
            Ok(bearer) => bearer,
            Err(error) => {
                eprintln!("FCM authorization failed: {error:#}");
                return Outcome::Retry { after: None };
            }
        };
        let url = format!("https://fcm.googleapis.com/v1/projects/{}/messages:send", s.project_id);
        let response = match self.http.post(url).bearer_auth(bearer).json(&fcm_message(push)).send().await {
            Ok(response) => response,
            Err(error) => {
                eprintln!("FCM request failed: {}", error.without_url());
                return Outcome::Retry { after: None };
            }
        };
        let status = response.status().as_u16();
        let retry = retry_after(response.headers());
        let body = response.text().await.unwrap_or_default();
        let (outcome, auth) = fcm_outcome(status, &body, retry);
        if auth {
            *self.fcm_token.lock().await = None;
            eprintln!("FCM refused the service account credentials ({status})");
        } else if outcome == Outcome::Rejected {
            eprintln!("FCM rejected an alert ({status})");
        }
        outcome
    }
}

impl Transport for Providers {
    fn platforms(&self) -> Platforms {
        Platforms { ios: self.apns.is_some(), android: self.fcm.is_some() }
    }
    fn send<'a>(&'a self, push: &'a Push) -> SendFuture<'a> {
        Box::pin(async move {
            match (push.platform.as_str(), &self.apns, &self.fcm) {
                ("ios", Some(s), _) => self.send_apns(s, push).await,
                ("android", _, Some(s)) => self.send_fcm(s, push).await,
                _ => Outcome::Rejected,
            }
        })
    }
}

#[cfg(test)]
#[path = "mobile_push_tests.rs"]
mod tests;
