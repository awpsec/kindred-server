use super::*;
use crate::{db, tests::bot};
use std::sync::Mutex;

const ACCOUNT: &str = "6a1d4b52-6b2f-4a4c-9d55-0d2f6a9b7c11";
const IOS_TOKEN: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
const PROFILE: &str = "1c7a0b2e-3f4d-4e5f-8a9b-0c1d2e3f4a5b";
const BOTH: Platforms = Platforms { ios: true, android: true };

fn device(db: &Db, installation: &str, platform: &str, token: &str, session: &[u8]) {
    let r = Registration::parse(installation, &json!({"platform":platform,"token":token,"environment":"production","account_id":ACCOUNT}), ACCOUNT).unwrap();
    db.register_mobile_device(&r, session, PROFILE).unwrap();
}
fn finished(db: &Db, bot: &str, output: &str) -> (String, i64) {
    let run = db.queue(bot, "Private prompt", 0).unwrap();
    db.finish(&run, "completed", output, "").unwrap();
    db.event(&run, "run_finished", json!({})).unwrap();
    let seq = db.0.lock().unwrap().query_row("SELECT MAX(seq) FROM events", [], |r| r.get(0)).unwrap();
    (run, seq)
}
fn outbox(db: &Db) -> Vec<(String, i64, i64, i64)> {
    db.0.lock().unwrap()
        .prepare("SELECT installation,event_id,attempts,next_attempt FROM mobile_push_outbox ORDER BY installation,event_id").unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap()
}
fn live(_: &Device) -> Result<bool> {
    Ok(true)
}

struct Fake {
    sent: Mutex<Vec<Push>>,
    outcomes: Mutex<Vec<Outcome>>,
    platforms: Platforms,
}
impl Fake {
    fn new(outcomes: Vec<Outcome>) -> Arc<Self> {
        Arc::new(Self { sent: Default::default(), outcomes: Mutex::new(outcomes), platforms: BOTH })
    }
}
impl Transport for Fake {
    fn platforms(&self) -> Platforms {
        self.platforms
    }
    fn send<'a>(&'a self, push: &'a Push) -> SendFuture<'a> {
        self.sent.lock().unwrap().push(push.clone());
        let mut outcomes = self.outcomes.lock().unwrap();
        let outcome = if outcomes.is_empty() { Outcome::Delivered } else { outcomes.remove(0) };
        Box::pin(async move { outcome })
    }
}

#[test]
fn registration_contract_is_validated_and_bound_to_the_signed_in_account() {
    let installation = "0F8FAD5B-D9CB-469F-A165-70867728950E";
    let ok = json!({"platform":"ios","token":IOS_TOKEN.to_uppercase(),"environment":"sandbox","account_id":ACCOUNT});
    let r = Registration::parse(installation, &ok, ACCOUNT).unwrap();
    assert_eq!(r.installation, installation.to_lowercase());
    assert_eq!(r.token, IOS_TOKEN);
    assert_eq!(r.environment, "sandbox");
    let android = json!({"platform":"android","token":"fcm-token_1:APA91bH.example","environment":"production","account_id":ACCOUNT});
    assert!(Registration::parse(installation, &android, ACCOUNT).is_ok());
    let invalid = [
        json!({"platform":"web","token":IOS_TOKEN,"environment":"production","account_id":ACCOUNT}),
        json!({"platform":"ios","token":IOS_TOKEN,"environment":"development","account_id":ACCOUNT}),
        json!({"platform":"ios","token":"not-hex","environment":"production","account_id":ACCOUNT}),
        json!({"platform":"ios","token":&IOS_TOKEN[1..],"environment":"production","account_id":ACCOUNT}),
        json!({"platform":"android","token":"short","environment":"production","account_id":ACCOUNT}),
        json!({"platform":"android","token":"token with spaces and more characters","environment":"production","account_id":ACCOUNT}),
        json!({"platform":"android","token":"x".repeat(4097),"environment":"production","account_id":ACCOUNT}),
        json!({"platform":"ios","token":IOS_TOKEN,"environment":"production","account_id":"not-a-uuid"}),
        json!({"platform":"ios","token":IOS_TOKEN,"environment":"production","account_id":"0f8fad5b-d9cb-469f-a165-70867728950e"}),
        json!({"platform":"ios","token":IOS_TOKEN,"environment":"production","account_id":ACCOUNT,"server_url":"https://evil.test"}),
        json!({"platform":"ios","token":IOS_TOKEN,"environment":"production"}),
        json!("not an object"),
    ];
    for body in invalid {
        assert!(Registration::parse(installation, &body, ACCOUNT).is_err(), "{body}");
    }
    for bad in ["", "legacy", "0f8fad5bd9cb469fa16570867728950e", "{0f8fad5b-d9cb-469f-a165-70867728950e}", "../0f8fad5b-d9cb-469f-a165-70867728950e"] {
        assert!(Registration::parse(bad, &ok, ACCOUNT).is_err(), "{bad}");
    }
    assert!(Registration::parse(installation, &ok, "").is_err());
}

#[test]
fn registrations_are_bounded_and_tokens_move_between_installations() {
    let db = Db::open(":memory:").unwrap();
    for _ in 0..MAX_DEVICES_PER_PROFILE {
        device(&db, &db::id(), "android", &format!("token-{}", db::id()), b"session");
    }
    let extra = Registration::parse(&db::id(), &json!({"platform":"android","token":"another-token-value","environment":"production","account_id":ACCOUNT}), ACCOUNT).unwrap();
    assert!(db.register_mobile_device(&extra, b"session", PROFILE).is_err());
    let db = Db::open(":memory:").unwrap();
    let (a, b) = (db::id(), db::id());
    device(&db, &a, "ios", IOS_TOKEN, b"one");
    device(&db, &b, "ios", IOS_TOKEN, b"two");
    let devices = db.mobile_devices().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].installation, b);
    assert_eq!(devices[0].session_digest, b"two");
    // Re-registering an installation updates it in place.
    device(&db, &b, "android", "replacement-fcm-token", b"three");
    let devices = db.mobile_devices().unwrap();
    assert_eq!((devices.len(), devices[0].platform.as_str()), (1, "android"));
}

#[test]
fn history_before_registration_or_older_than_the_window_is_never_pushed() {
    let db = Db::open(":memory:").unwrap();
    let b = bot(&db, "codex");
    finished(&db, &b.id, "Old private answer");
    finished(&db, &b.id, "Another old private answer");
    let installation = db::id();
    device(&db, &installation, "ios", IOS_TOKEN, b"session");
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 0);
    assert!(outbox(&db).is_empty());
    let (_, fresh) = finished(&db, &b.id, "New private answer");
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 1);
    assert_eq!(outbox(&db).iter().map(|r| r.1).collect::<Vec<_>>(), vec![fresh]);
    // Collecting again is idempotent.
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 0);
    // A long outage: events now older than the window are skipped, not replayed.
    db.0.lock().unwrap().execute("DELETE FROM mobile_push_outbox", []).unwrap();
    for _ in 0..3 {
        finished(&db, &b.id, "Missed while offline");
    }
    db.0.lock().unwrap().execute("UPDATE events SET created=? WHERE seq>?", params![now() - MAX_EVENT_AGE - 5, fresh]).unwrap();
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 0);
    let latest: i64 = db.0.lock().unwrap().query_row("SELECT MAX(seq) FROM events", [], |r| r.get(0)).unwrap();
    let cursor: i64 = db.0.lock().unwrap().query_row("SELECT cursor FROM mobile_push_cursor", [], |r| r.get(0)).unwrap();
    assert_eq!(cursor, latest);
    // Removing the last device forgets the cursor; returning later starts fresh.
    assert!(db.remove_mobile_device(&installation).unwrap());
    finished(&db, &b.id, "While no phone was registered");
    device(&db, &installation, "ios", IOS_TOKEN, b"session");
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 0);
}

#[test]
fn large_backlogs_are_paged_without_flooding() {
    let db = Db::open(":memory:").unwrap();
    let b = bot(&db, "codex");
    device(&db, &db::id(), "android", "android-token-value", b"session");
    for i in 0..230 {
        let run = db.queue(&b.id, "p", 0).unwrap();
        db.finish(&run, "completed", &format!("answer {i}"), "").unwrap();
        db.event(&run, "tool_result", json!({})).unwrap();
        db.event(&run, "run_finished", json!({})).unwrap();
    }
    let queued = collect(&db, &live, BOTH).unwrap();
    assert!(queued > 0 && queued <= MAX_OUTBOX_ROWS as usize);
    let total: usize = queued + collect(&db, &live, BOTH).unwrap() + collect(&db, &live, BOTH).unwrap();
    assert_eq!(total, 230);
    assert!(outbox(&db).len() <= MAX_OUTBOX_ROWS as usize);
}

#[test]
fn mute_preference_and_quiet_filters_from_the_feed_apply_to_pushes() {
    let db = Db::open(":memory:").unwrap();
    let mut b = bot(&db, "codex");
    device(&db, &db::id(), "ios", IOS_TOKEN, b"session");
    collect(&db, &live, BOTH).unwrap();
    let mut general = db::general_settings(None);
    general["notifications"] = json!("none");
    db.save_setting("general", &general).unwrap();
    finished(&db, &b.id, "Hidden by preference");
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 0);
    general["notifications"] = json!("input_needed");
    db.save_setting("general", &general).unwrap();
    finished(&db, &b.id, "Completion is not input needed");
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 0);
    general["notifications"] = json!("all");
    db.save_setting("general", &general).unwrap();
    db.mute_notifications("bot", &b.id, 3600).unwrap();
    finished(&db, &b.id, "Muted bot");
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 0);
    db.mute_notifications("bot", &b.id, 0).unwrap();
    b.profile.notifications = false;
    db.save_bot(&b).unwrap();
    finished(&db, &b.id, "Bot notifications off");
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 0);
    b.profile.notifications = true;
    db.save_bot(&b).unwrap();
    let run = db.queue(&b.id, "p", 0).unwrap();
    db.finish(&run, "completed", "Quiet routine", "").unwrap();
    db.event(&run, "tool_result", json!({"tool":"finish_quietly","failed":0})).unwrap();
    db.event(&run, "run_finished", json!({})).unwrap();
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 0);
    finished(&db, &b.id, "Visible");
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 1);
    // Unconfigured platforms queue nothing, but the cursor still advances.
    finished(&db, &b.id, "No APNs configured");
    assert_eq!(collect(&db, &live, Platforms { ios: false, android: true }).unwrap(), 0);
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 0);
}

#[test]
fn ended_sessions_remove_their_registrations_before_delivery() {
    let db = Db::open(":memory:").unwrap();
    let b = bot(&db, "codex");
    let (kept, revoked) = (db::id(), db::id());
    device(&db, &kept, "ios", IOS_TOKEN, b"live");
    device(&db, &revoked, "android", "android-token-value", b"ended");
    collect(&db, &live, BOTH).unwrap();
    finished(&db, &b.id, "After sign-out");
    let only_live = |d: &Device| Ok(d.session_digest == b"live");
    assert_eq!(collect(&db, &only_live, BOTH).unwrap(), 1);
    assert_eq!(db.mobile_devices().unwrap().iter().map(|d| d.installation.clone()).collect::<Vec<_>>(), vec![kept.clone()]);
    // A registry error is not proof of revocation and keeps the device.
    let broken = |_: &Device| -> Result<bool> { anyhow::bail!("registry busy") };
    assert!(collect(&db, &broken, BOTH).is_err());
    assert_eq!(db.mobile_devices().unwrap().len(), 1);
    assert_eq!(db.remove_mobile_sessions(&[b"live".to_vec()]).unwrap(), 1);
    assert!(db.mobile_devices().unwrap().is_empty());
    assert!(outbox(&db).is_empty());
}

#[tokio::test]
async fn transient_failures_back_off_and_eventually_expire() {
    let db = Db::open(":memory:").unwrap();
    let b = bot(&db, "codex");
    let installation = db::id();
    device(&db, &installation, "ios", IOS_TOKEN, b"session");
    collect(&db, &live, BOTH).unwrap();
    let (_, event) = finished(&db, &b.id, "Private answer");
    collect(&db, &live, BOTH).unwrap();
    let fake = Fake::new(vec![Outcome::Retry { after: None }, Outcome::Retry { after: Some(600) }]);
    assert_eq!(deliver(&db, fake.clone()).await.unwrap(), 0);
    let row = outbox(&db)[0].clone();
    assert_eq!((row.1, row.2), (event, 1));
    assert!(row.3 >= now() + backoff(1) - 1);
    // Not due yet: nothing is sent.
    deliver(&db, fake.clone()).await.unwrap();
    assert_eq!(fake.sent.lock().unwrap().len(), 1);
    db.0.lock().unwrap().execute("UPDATE mobile_push_outbox SET next_attempt=0", []).unwrap();
    deliver(&db, fake.clone()).await.unwrap();
    let row = outbox(&db)[0].clone();
    assert_eq!(row.2, 2);
    assert!(row.3 >= now() + 599, "Retry-After is honored");
    db.0.lock().unwrap().execute("UPDATE mobile_push_outbox SET next_attempt=0", []).unwrap();
    assert_eq!(deliver(&db, fake.clone()).await.unwrap(), 1);
    assert!(outbox(&db).is_empty());
    assert_eq!(fake.sent.lock().unwrap().len(), 3);
    // Repeated failure stops after the attempt limit.
    finished(&db, &b.id, "Second");
    collect(&db, &live, BOTH).unwrap();
    let failing = Fake::new(vec![Outcome::Retry { after: None }; MAX_ATTEMPTS as usize + 2]);
    for _ in 0..MAX_ATTEMPTS + 2 {
        db.0.lock().unwrap().execute("UPDATE mobile_push_outbox SET next_attempt=0", []).unwrap();
        deliver(&db, failing.clone()).await.unwrap();
    }
    assert!(outbox(&db).is_empty());
    assert_eq!(failing.sent.lock().unwrap().len(), MAX_ATTEMPTS as usize);
    // Old undelivered alerts expire rather than arriving hours late.
    finished(&db, &b.id, "Third");
    collect(&db, &live, BOTH).unwrap();
    db.0.lock().unwrap().execute("UPDATE mobile_push_outbox SET created=?,next_attempt=0", [now() - MAX_OUTBOX_AGE - 1]).unwrap();
    assert!(due(&db, BOTH).unwrap().is_empty());
    assert!(outbox(&db).is_empty());
    assert_eq!(backoff(1), 20);
    assert_eq!(backoff(20), 1800);
}

#[tokio::test]
async fn invalid_tokens_remove_only_the_failed_registration() {
    let db = Db::open(":memory:").unwrap();
    let b = bot(&db, "codex");
    let (ios, android) = (db::id(), db::id());
    device(&db, &ios, "ios", IOS_TOKEN, b"session");
    device(&db, &android, "android", "android-token-value", b"session");
    collect(&db, &live, BOTH).unwrap();
    finished(&db, &b.id, "Private answer");
    collect(&db, &live, BOTH).unwrap();
    let pushes = due(&db, BOTH).unwrap();
    assert_eq!(pushes.len(), 2);
    let stale = pushes.iter().find(|p| p.platform == "ios").unwrap().clone();
    // The app re-registered with a fresh token while the old send was in flight.
    let fresh = IOS_TOKEN.replace('a', "b");
    device(&db, &ios, "ios", &fresh, b"session");
    record(&db, &stale, Outcome::InvalidToken).unwrap();
    assert_eq!(db.mobile_devices().unwrap().len(), 2);
    let fake = Fake::new(vec![Outcome::InvalidToken, Outcome::InvalidToken]);
    deliver(&db, fake.clone()).await.unwrap();
    assert!(db.mobile_devices().unwrap().is_empty());
    assert!(outbox(&db).is_empty());
    let cursor: Option<i64> = db.0.lock().unwrap().query_row("SELECT cursor FROM mobile_push_cursor", [], |r| r.get(0)).optional().unwrap();
    assert_eq!(cursor, None);
    let sent = fake.sent.lock().unwrap();
    assert!(sent.iter().any(|p| p.platform == "ios" && p.token == fresh));
}

#[test]
fn payloads_are_generic_and_carry_only_routing_identifiers() {
    let push = Push { installation: db::id(), platform: "ios".into(), token: IOS_TOKEN.into(), environment: "production".into(), account_id: ACCOUNT.into(), profile_id: PROFILE.into(), chat_id: "dm-bot".into(), event_id: 42, reminder_alert: None };
    let apns = apns_payload(&push);
    assert_eq!(apns["aps"]["sound"], "kindred-pop.wav");
    assert_eq!(apns["installation_uuid"], push.installation);
    assert_eq!(apns["profile_id"], PROFILE);
    assert_eq!(apns["aps"]["alert"], json!({"title":"Kindred","body":ALERT_BODY}));
    assert_eq!((apns["account_id"].as_str(), apns["chat_id"].as_str(), apns["event_id"].as_str()), (Some(ACCOUNT), Some("dm-bot"), Some("42")));
    let fcm = fcm_message(&Push { platform: "android".into(), token: "android-token-value".into(), ..push.clone() });
    assert_eq!(fcm["message"]["android"]["notification"]["channel_id"], "kindred_updates");
    assert_eq!(fcm["message"]["data"], json!({"account_id":ACCOUNT,"profile_id":PROFILE,"installation_uuid":push.installation,"chat_id":"dm-bot","event_id":"42"}));
    assert!(fcm["message"]["data"].as_object().unwrap().values().all(Value::is_string));
    for text in [apns.to_string(), fcm.to_string()] {
        assert!(!text.contains("http") && !text.contains("Private"));
    }
    assert_eq!(apns_host("sandbox"), "api.sandbox.push.apple.com");
    assert_eq!(apns_host("production"), "api.push.apple.com");
    assert_eq!(apns_host("anything-else"), "api.push.apple.com");
}

#[test]
fn provider_responses_are_classified_for_retry_and_cleanup() {
    let apns = |status, reason: &str| apns_outcome(status, &json!({"reason":reason}).to_string(), Some(30));
    assert_eq!(apns(200, ""), (Outcome::Delivered, false));
    assert_eq!(apns(410, "Unregistered"), (Outcome::InvalidToken, false));
    assert_eq!(apns(400, "BadDeviceToken"), (Outcome::InvalidToken, false));
    assert_eq!(apns(400, "DeviceTokenNotForTopic"), (Outcome::InvalidToken, false));
    assert_eq!(apns(400, "PayloadTooLarge"), (Outcome::Rejected, false));
    assert_eq!(apns(403, "ExpiredProviderToken"), (Outcome::Retry { after: Some(30) }, true));
    assert_eq!(apns(429, "TooManyRequests"), (Outcome::Retry { after: Some(30) }, false));
    assert_eq!(apns(503, "ServiceUnavailable"), (Outcome::Retry { after: Some(30) }, false));
    let fcm = |status, code: &str, field: &str| {
        let body = json!({"error":{"message":"x","details":[{"errorCode":code,"fieldViolations":[{"field":field}]}]}});
        fcm_outcome(status, &body.to_string(), None)
    };
    assert_eq!(fcm(200, "", ""), (Outcome::Delivered, false));
    assert_eq!(fcm(404, "UNREGISTERED", ""), (Outcome::InvalidToken, false));
    assert_eq!(fcm(400, "INVALID_ARGUMENT", "message.token"), (Outcome::InvalidToken, false));
    assert_eq!(fcm(400, "INVALID_ARGUMENT", "message.android.ttl"), (Outcome::Rejected, false));
    assert_eq!(fcm(403, "SENDER_ID_MISMATCH", ""), (Outcome::InvalidToken, false));
    assert_eq!(fcm(401, "THIRD_PARTY_AUTH_ERROR", ""), (Outcome::Retry { after: None }, true));
    assert_eq!(fcm(429, "QUOTA_EXCEEDED", ""), (Outcome::Retry { after: None }, false));
    assert_eq!(fcm(500, "INTERNAL", ""), (Outcome::Retry { after: None }, false));
    assert_eq!(fcm_outcome(502, "<html>", None), (Outcome::Retry { after: None }, false));
}

fn pem(der: &[u8]) -> String {
    use base64::Engine;
    let body = base64::engine::general_purpose::STANDARD.encode(der);
    let lines: Vec<&str> = body.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).unwrap()).collect();
    format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n", lines.join("\n"))
}
fn segments(jwt: &str) -> (Value, Value, Vec<u8>, String) {
    use base64::Engine;
    let decode = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).unwrap();
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3);
    (serde_json::from_slice(&decode(parts[0])).unwrap(), serde_json::from_slice(&decode(parts[1])).unwrap(), decode(parts[2]), format!("{}.{}", parts[0], parts[1]))
}

#[test]
fn apns_provider_tokens_are_es256_signed_with_the_p8_key() {
    use ring::signature::KeyPair;
    let rng = ring::rand::SystemRandom::new();
    let der = ring::signature::EcdsaKeyPair::generate_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
    let key = ring::signature::EcdsaKeyPair::from_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING, der.as_ref(), &rng).unwrap();
    let settings = ApnsSettings::new(&pem(der.as_ref()), "ABCDE12345", "TEAM123456", "com.example.kindred").unwrap();
    let jwt = apns_jwt(&settings, 1_700_000_000).unwrap();
    let (header, claims, signature, input) = segments(&jwt);
    assert_eq!(header, json!({"alg":"ES256","kid":"ABCDE12345"}));
    assert_eq!(claims, json!({"iss":"TEAM123456","iat":1_700_000_000}));
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, key.public_key().as_ref())
        .verify(input.as_bytes(), &signature)
        .unwrap();
    for (key_id, team, topic) in [("short", "TEAM123456", "com.example.kindred"), ("ABCDE12345", "TEAM/23456", "com.example.kindred"), ("ABCDE12345", "TEAM123456", "https://x"), ("ABCDE12345", "TEAM123456", "nodots")] {
        assert!(ApnsSettings::new(&pem(der.as_ref()), key_id, team, topic).is_err());
    }
    assert!(ApnsSettings::new("not a key", "ABCDE12345", "TEAM123456", "com.example.kindred").is_err());
}

#[test]
fn fcm_assertions_are_rs256_signed_for_the_fixed_google_audience() {
    let pem_text = include_str!("test-fixtures/mobile-push-test-rsa.pem");
    let account = json!({"type":"service_account","project_id":"kindred-test-1","client_email":"push@kindred-test-1.iam.gserviceaccount.com","private_key":pem_text,
        "token_uri":"https://attacker.example/token"});
    let settings = FcmSettings::new(&account.to_string()).unwrap();
    assert_eq!(settings.project_id, "kindred-test-1");
    let assertion = fcm_assertion(&settings, 1_700_000_000).unwrap();
    let (header, claims, signature, input) = segments(&assertion);
    assert_eq!(header["alg"], "RS256");
    assert_eq!(claims["aud"], GOOGLE_TOKEN_URL);
    assert_eq!(claims["scope"], FCM_SCOPE);
    assert_eq!(claims["exp"], 1_700_003_600);
    let key = ring::signature::RsaKeyPair::from_pkcs8(&settings.key_pkcs8).unwrap();
    ring::signature::UnparsedPublicKey::new(&ring::signature::RSA_PKCS1_2048_8192_SHA256, key.public().as_ref())
        .verify(input.as_bytes(), &signature)
        .unwrap();
    for (field, value) in [("type", json!("authorized_user")), ("project_id", json!("../other")), ("project_id", json!("UPPER-case")), ("client_email", json!("no-at-sign")), ("private_key", json!("x"))] {
        let mut bad = account.clone();
        bad[field] = value;
        assert!(FcmSettings::new(&bad.to_string()).is_err(), "{field}");
    }
}

#[test]
fn credential_files_must_be_absolute_small_regular_files() {
    assert!(read_private_file("relative/key.p8").is_err());
    assert!(read_private_file("/nonexistent/kindred/key.p8").is_err());
    assert!(read_private_file("/").is_err());
}

#[test]
fn a_device_joining_an_existing_profile_cursor_never_receives_earlier_events() {
    let db = Db::open(":memory:").unwrap();
    let b = bot(&db, "codex");
    let first = db::id();
    device(&db, &first, "ios", IOS_TOKEN, b"session");
    collect(&db, &live, BOTH).unwrap();
    // Not collected yet when the second phone joins.
    let (_, before) = finished(&db, &b.id, "Before the second phone");
    let second = db::id();
    device(&db, &second, "android", "android-token-value", b"session");
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 1);
    assert_eq!(outbox(&db).iter().map(|r| (r.0.clone(), r.1)).collect::<Vec<_>>(), vec![(first.clone(), before)]);
    // Refreshing the token keeps the join point rather than moving it.
    device(&db, &second, "android", "android-token-refreshed", b"session");
    let (_, after) = finished(&db, &b.id, "After both joined");
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 2);
    let mut expected = vec![(first.clone(), before), (first, after), (second, after)];
    expected.sort();
    let mut actual: Vec<_> = outbox(&db).into_iter().map(|r| (r.0, r.1)).collect();
    actual.sort();
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn delayed_retries_are_revalidated_against_current_feed_rules() {
    let db = Db::open(":memory:").unwrap();
    let b = bot(&db, "codex");
    device(&db, &db::id(), "ios", IOS_TOKEN, b"session");
    collect(&db, &live, BOTH).unwrap();
    let retry = |db: &Db| db.0.lock().unwrap().execute("UPDATE mobile_push_outbox SET next_attempt=0", []).unwrap();
    // Muted after the first failed attempt: the retry is dropped, not sent.
    finished(&db, &b.id, "Answer");
    collect(&db, &live, BOTH).unwrap();
    let fake = Fake::new(vec![Outcome::Retry { after: None }]);
    deliver(&db, fake.clone()).await.unwrap();
    db.mute_notifications("chat", &format!("dm-{}", b.id), -1).unwrap();
    retry(&db);
    assert_eq!(deliver(&db, fake.clone()).await.unwrap(), 0);
    assert!(outbox(&db).is_empty());
    assert_eq!(fake.sent.lock().unwrap().len(), 1);
    db.mute_notifications("chat", &format!("dm-{}", b.id), 0).unwrap();
    // Global preference changed to input-needed only: a completion retry is dropped.
    finished(&db, &b.id, "Second answer");
    collect(&db, &live, BOTH).unwrap();
    deliver(&db, Fake::new(vec![Outcome::Retry { after: None }])).await.unwrap();
    let mut general = db::general_settings(None);
    general["notifications"] = json!("input_needed");
    db.save_setting("general", &general).unwrap();
    retry(&db);
    assert!(due(&db, BOTH).unwrap().is_empty());
    assert!(outbox(&db).is_empty());
    // An approval decided before the retry is no longer announced.
    let pending = db.queue(&b.id, "Needs consent", 0).unwrap();
    db.claim().unwrap();
    let approval = db.request_approval(&pending, "routine_create", &json!({})).unwrap();
    db.event(&pending, "approval", json!({"id":approval})).unwrap();
    collect(&db, &live, BOTH).unwrap();
    assert_eq!(outbox(&db).len(), 1);
    deliver(&db, Fake::new(vec![Outcome::Retry { after: None }])).await.unwrap();
    retry(&db);
    assert_eq!(due(&db, BOTH).unwrap().len(), 1, "still pending, still eligible");
    db.decide(&approval, false).unwrap();
    assert!(due(&db, BOTH).unwrap().is_empty());
    assert!(outbox(&db).is_empty());
}

#[test]
fn own_bot_results_in_shared_rooms_are_pushed_from_the_private_feed() {
    let db = Db::open(":memory:").unwrap();
    let b = bot(&db, "codex");
    device(&db, &db::id(), "ios", IOS_TOKEN, b"session");
    collect(&db, &live, BOTH).unwrap();
    let room = format!("server-{}", db::id());
    let run = db.queue(&b.id, "Shared request", 0).unwrap();
    {
        let c = db.0.lock().unwrap();
        c.execute("INSERT INTO chats(id,name,members) VALUES(?,'Room',?)", params![room, json!([b.id]).to_string()]).unwrap();
        c.execute("UPDATE runs SET chat_id=? WHERE id=?", params![room, run]).unwrap();
    }
    db.finish(&run, "completed", "Shared answer", "").unwrap();
    db.event(&run, "run_finished", json!({})).unwrap();
    assert_eq!(collect(&db, &live, BOTH).unwrap(), 1);
    assert_eq!(due(&db, BOTH).unwrap()[0].chat_id, room);
}

#[test]
fn reminders_push_text_and_schedule_once_and_dismissal_cancels_unsent_pushes() {
    let db=Db::open(":memory:").unwrap();
    let b=bot(&db,"codex");
    device(&db,&db::id(),"ios",IOS_TOKEN,b"session");
    device(&db,&db::id(),"android","android-reminder-test-token",b"session");
    collect(&db,&live,BOTH).unwrap();
    let id=db.queue(&b.id,"Set a reminder",0).unwrap();
    let run=db.run(&id).unwrap();
    let a=db.reminder_set(&run,json!({"key":"push-test","message":"Leave for the station","local_time":"2099-09-10T12:00","timezone":"America/New_York"})).unwrap();
    db.tick_reminders(a["run_at"].as_i64().unwrap()).unwrap();
    // Delivery is simulated in the future; the push collection wall clock is now.
    assert_eq!(collect(&db,&live,BOTH).unwrap(),2);
    assert_eq!(collect(&db,&live,BOTH).unwrap(),0);
    let pushes=due(&db,BOTH).unwrap();
    assert_eq!(pushes.len(),2);
    for push in &pushes {
        let apns=apns_payload(push);
        assert_eq!(apns["aps"]["alert"]["title"],"Kindred reminder");
        assert_eq!(apns["aps"]["alert"]["body"],"Reminder · Sep 10, 12:00 PM EDT\nLeave for the station");
        assert_eq!(fcm_message(push)["message"]["notification"],apns["aps"]["alert"]);
        assert!(still_eligible(&db,push.event_id).unwrap());
    }
    db.reminder_update(&run.chat_id,None,a["id"].as_str().unwrap(),json!({"expected_revision":2,"dismiss":true})).unwrap();
    assert!(due(&db,BOTH).unwrap().is_empty());
    assert!(outbox(&db).is_empty());
}
