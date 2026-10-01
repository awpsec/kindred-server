//! Gateway side of native mobile push: device registration is bound to the
//! caller's account session, and one worker delivers for every open profile.
use super::*;
use crate::mobile_push::{self as push, Device, Registration, Transport};
use axum::{
    extract::{Path as UrlPath, Query},
    routing::put,
};

pub(super) fn routes() -> Router<Portal> {
    Router::new()
        .route("/api/mobile/devices/{installation}", put(register).delete(unregister))
        .route("/api/mobile/push-status", get(status))
}

fn unauthorized(message: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({"error":message}))).into_response()
}

/// Mobile registration needs a real account session; legacy device tokens have
/// no account or revocable session to bind to.
fn account(p: &Profiles, headers: &HeaderMap) -> std::result::Result<Identity, Response> {
    if p.origin(headers).is_err() {
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    match p.identity(bearer(headers)) {
        Ok(id) if !id.legacy => Ok(id),
        Ok(_) => Err(unauthorized("Sign in to your Kindred account to use mobile notifications.")),
        Err(_) => Err(unauthorized("Sign in to your Kindred account.")),
    }
}

fn reply(result: Result<Value>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => web::Error::from(error).into_response(),
    }
}

async fn register(
    State(p): State<Portal>,
    UrlPath(installation): UrlPath<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let id = match account(&p, &headers) {
        Ok(id) => id,
        Err(response) => return response,
    };
    reply((|| {
        let body: Value = serde_json::from_slice(&body).map_err(|_| anyhow::anyhow!("Invalid device registration"))?;
        let registration = Registration::parse(&installation, &body, &id.account)?;
        // One installation is registered in every profile this account owns,
        // each with its own join point, outbox and profile ID.
        let apps = p.owned_apps(&id.account)?;
        for (profile, app) in &apps {
            app.db.register_mobile_device(&registration, &hash(bearer(&headers)), profile)?;
        }
        Ok(json!({"registered":true,"installation_uuid":registration.installation,"platform":registration.platform,
            "profiles":apps.len(),"delivery_enabled":p.push_platforms.supports(&registration.platform)}))
    })())
}

async fn unregister(
    State(p): State<Portal>,
    UrlPath(installation): UrlPath<String>,
    headers: HeaderMap,
) -> Response {
    let id = match account(&p, &headers) {
        Ok(id) => id,
        Err(response) => return response,
    };
    reply((|| {
        anyhow::ensure!(uuid::Uuid::parse_str(&installation).is_ok() && installation.len() == 36, "Invalid installation ID");
        let installation = installation.to_ascii_lowercase();
        let mut removed = false;
        for (_, app) in p.owned_apps(&id.account)? {
            removed |= app.db.remove_mobile_device(&installation)?;
        }
        Ok(json!({"removed":removed,"installation_uuid":installation}))
    })())
}

#[derive(serde::Deserialize)]
struct StatusQuery {
    installation_uuid: Option<String>,
}

async fn status(State(p): State<Portal>, headers: HeaderMap, Query(q): Query<StatusQuery>) -> Response {
    let id = match account(&p, &headers) {
        Ok(id) => id,
        Err(response) => return response,
    };
    reply((|| {
        let platforms = p.push_platforms;
        let mut value = json!({"enabled":platforms.any(),"platforms":{"ios":platforms.ios,"android":platforms.android}});
        if let Some(installation) = q.installation_uuid {
            anyhow::ensure!(uuid::Uuid::parse_str(&installation).is_ok() && installation.len() == 36, "Invalid installation ID");
            let apps = p.owned_apps(&id.account)?;
            let mut registered = 0;
            for (_, app) in &apps {
                registered += app.db.mobile_device_registered(&installation.to_ascii_lowercase())? as usize;
            }
            // True only when every owned profile is covered.
            value["registered"] = json!(!apps.is_empty() && registered == apps.len());
            value["profiles"] = json!(apps.len());
            value["registered_profiles"] = json!(registered);
        }
        Ok(value)
    })())
}

impl Profiles {
    /// A registration stays live only while its session is unexpired and
    /// unrevoked, its account is enabled, and that account still owns this
    /// profile. The session may currently be on any of the account's profiles.
    pub(super) fn mobile_session_live(&self, profile: &str, device: &Device) -> Result<bool> {
        Ok(self.registry.lock().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions s JOIN accounts a ON a.id=s.account_id JOIN profiles p ON p.account_id=a.id AND p.id=? WHERE s.digest=? AND s.account_id=? AND s.expires>? AND a.disabled=0)",
            params![profile, device.session_digest, device.account_id, db::now()],
            |r| r.get(0),
        )?)
    }

    /// Every profile the account owns. Must not be called while holding the
    /// registry lock, because opening a profile may need it.
    pub(super) fn owned_apps(&self, account: &str) -> Result<Vec<(String, Shared)>> {
        let ids: Vec<String> = self.registry.lock().unwrap()
            .prepare("SELECT id FROM profiles WHERE account_id=? ORDER BY created,rowid")?
            .query_map([account], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids.into_iter().map(|id| Ok((id.clone(), self.app(&id)?))).collect()
    }

    /// Moves registrations to a rotated session in every owned profile. Callers
    /// hold the registry lock so the worker never sees the gap.
    pub(super) fn rebind_mobile(apps: &[(String, Shared)], old: &[u8], new: &[u8]) -> Result<()> {
        for (_, app) in apps {
            app.db.rebind_mobile_session(old, new)?;
        }
        Ok(())
    }

    /// A newly created profile receives the account's live phone registrations,
    /// starting at its own present, so alerts cover it without the app open.
    pub(super) fn inherit_mobile_devices(&self, account: &str, profile: &str) -> Result<()> {
        let apps = self.owned_apps(account)?;
        let Some((_, target)) = apps.iter().find(|(id, _)| id == profile) else { return Ok(()) };
        let mut seen = std::collections::HashSet::new();
        for (id, app) in &apps {
            if id == profile {
                continue;
            }
            for (registration, digest) in app.db.mobile_registrations(account)? {
                if seen.insert(registration.installation.clone()) {
                    target.db.register_mobile_device(&registration, &digest, profile)?;
                }
            }
        }
        Ok(())
    }

    /// One pass over every open profile. Work per pass is bounded by the
    /// feed page and send limits in `mobile_push`.
    pub(super) async fn mobile_push_tick(&self, transport: Arc<dyn Transport>) {
        let apps: Vec<(String, Shared)> =
            self.apps.lock().unwrap().iter().map(|(id, app)| (id.clone(), app.clone())).collect();
        for (profile, app) in apps {
            let live = |device: &Device| self.mobile_session_live(&profile, device);
            if let Err(error) = push::collect(&app.db, &live, transport.platforms()) {
                eprintln!("Mobile push will retry collecting updates: {error:#}");
                continue;
            }
            if let Err(error) = push::deliver(&app.db, transport.clone()).await {
                eprintln!("Mobile push will retry delivery: {error:#}");
            }
        }
    }
}

pub(super) async fn worker(weak: std::sync::Weak<Profiles>, transport: Arc<dyn Transport>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let Some(p) = weak.upgrade() else { return };
        p.mobile_push_tick(transport.clone()).await;
    }
}
