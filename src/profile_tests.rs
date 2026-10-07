use super::*;
use axum::body::to_bytes;

struct Fixture {
    p: Portal,
    root: PathBuf,
}
impl Fixture {
    fn new(legacy: bool) -> Self {
        let root = std::env::temp_dir().join(format!("kindred-profiles-{}", db::id()));
        let mut config = Config::default();
        config.profiles.enabled = true;
        config.profiles.directory = root.to_string_lossy().into_owned();
        config.profiles.vm_manager = root.join("fixture-manager.py").to_string_lossy().into_owned();
        config.database = root.join("legacy.db").to_string_lossy().into_owned();
        let app = if legacy {
            Some(
                App::open(
                    config.clone(),
                    "legacy-owner-token-12345678901234567890".into(),
                )
                .unwrap(),
            )
        } else {
            None
        };
        Self {
            p: Profiles::open(config, app, false).unwrap(),
            root,
        }
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if !token.is_empty() {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        let response = router(self.p.clone())
            .oneshot(
                req.body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!({"text":String::from_utf8_lossy(&bytes)})),
        )
    }
    async fn register(&self, name: &str) -> Value {
        let (status, v) = self
            .request(
                "POST",
                "/identity/register",
                "",
                json!({"login":name,"name":name,"password":"test password for profiles"}),
            )
            .await;
        assert_eq!(status, 200, "{v}");
        v
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn fresh_and_sparse_profiles_can_save_local_access_without_losing_identity() {
    let f = Fixture::new(false);
    let account = f.register("fresh-settings").await;
    let token = account["token"].as_str().unwrap();
    let app = f.p.app(&f.p.identity(token).unwrap().profile).unwrap();
    let (status, mut settings) = f.request("GET", "/api/settings", token, Value::Null).await;
    assert_eq!(status, 200);
    assert_eq!(settings["identity"], "");
    assert_eq!(settings["name"], "fresh-settings");
    assert_eq!(settings["local_access"], false);
    settings["local_access"] = json!(true);
    let (status, saved) = f.request("PUT", "/api/settings", token, settings).await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["local_access"], true);
    // The API includes the separately stored primary-bot selection. It is
    // intentionally not persisted as part of general settings.
    let mut persisted = saved.clone();
    assert_eq!(persisted.as_object_mut().unwrap().remove("primary_bot_id"), Some(Value::Null));
    assert_eq!(app.db.setting("general").unwrap().unwrap(), persisted);

    // This is the sparse row created by servers before the fix, including a
    // client that has already loaded it before upgrading the server.
    let mut old = json!({"name":"Personal","theme":"light","approval_mode":"full",
        "local_access":false,"timezone":"America/New_York","timezone_mode":"fixed"});
    app.db.save_setting("general", &old).unwrap();
    let (_, loaded) = f.request("GET", "/api/settings", token, Value::Null).await;
    assert_eq!(loaded["identity"], "");
    assert_eq!(loaded["theme"], "light");
    assert_eq!(loaded["local_access"], false);
    old["local_access"] = json!(true);
    let (status, mut saved) = f.request("PUT", "/api/settings", token, old.clone()).await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["identity"], "");
    assert_eq!(saved["timezone_mode"], "fixed");
    saved["identity"] = json!("Keep my preferences");
    app.db.save_setting("general", &saved).unwrap();
    let (_, preserved) = f.request("PUT", "/api/settings", token, old.clone()).await;
    assert_eq!(preserved["identity"], "Keep my preferences");
    old["identity"] = json!(42);
    assert_eq!(f.request("PUT", "/api/settings", token, old).await.0, 400);
    let mut persisted = preserved.clone();
    assert_eq!(persisted.as_object_mut().unwrap().remove("primary_bot_id"), Some(Value::Null));
    assert_eq!(app.db.setting("general").unwrap().unwrap(), persisted);
}

#[tokio::test]
async fn provider_checks_report_setup_without_starting_the_computer_or_provider() {
    let f = Fixture::new(false);
    let manager = f.root.join("fixture-manager.py");
    let account = f.register("setup-state").await;
    let token = account["token"].as_str().unwrap();
    let app = f.p.app(&f.p.identity(token).unwrap().profile).unwrap();
    let endpoints = [
        "/api/codex/account",
        "/api/codex/models",
        "/api/codex/usage",
        "/api/codex/connectors",
        "/api/provider-cli/claude-code/account",
        "/api/provider-cli/kimi-code/account",
    ];
    // No manager is installed yet: these reads must not try to provision a VM.
    for endpoint in endpoints {
        let (status, result) = f.request("POST", endpoint, token, json!({})).await;
        assert_eq!(status, 200, "{endpoint}: {result}");
        assert_eq!(result["setup"], "not_started");
    }
    let ssh = Path::new(&app.config.vm.ssh_config);
    std::fs::create_dir_all(ssh.parent().unwrap()).unwrap();
    std::fs::write(ssh, "Do not launch SSH from this fixture").unwrap();
    for phase in ["installing", "starting", "failed", "stopped"] {
        std::fs::write(&manager, format!("import sys,json\nassert sys.argv[1]=='connection-status'\nprint(json.dumps({{'setup':'{phase}'}}))\n")).unwrap();
        for endpoint in endpoints {
            let (status, result) = f.request("POST", endpoint, token, json!({})).await;
            assert_eq!(status, 200, "{endpoint}: {result}");
            assert_eq!(result["setup"], phase);
            assert_eq!(
                result["preparing"],
                matches!(phase, "starting" | "installing")
            );
            assert_eq!(result["connected"], false);
            assert!(!result["message"].as_str().unwrap().contains("disconnected"));
        }
    }
    std::fs::write(
        &manager,
        "import json\nprint(json.dumps({'setup':'runtime_failed'}))\n",
    )
    .unwrap();
    for endpoint in endpoints
        .into_iter()
        .filter(|endpoint| endpoint.starts_with("/api/codex/"))
    {
        let (status, result) = f.request("POST", endpoint, token, json!({})).await;
        assert_eq!(status, 200, "{endpoint}: {result}");
        assert_eq!(result["setup"], "runtime_failed");
        assert_eq!(result["connected"], false);
        assert_eq!(result["preparing"], false);
        assert!(
            result["message"]
                .as_str()
                .unwrap()
                .contains("Memory and other Codex tools")
        );
    }
    for provider in ["claude-code", "kimi-code", "openrouter"] {
        assert!(
            crate::vm::provider_unavailable(&app.config.vm, provider)
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(app.auth.lock().await.is_none());
}

#[tokio::test]
async fn first_admin_is_atomic_and_accounts_cannot_cross_profile_boundary() {
    let f = Fixture::new(false);
    let (one, two) = tokio::join!(f.register("alex"), f.register("owner"));
    let t1 = one["token"].as_str().unwrap();
    let t2 = two["token"].as_str().unwrap();
    let i1 = f.p.identity(t1).unwrap();
    let i2 = f.p.identity(t2).unwrap();
    assert_eq!(f.p.projection(&i1).unwrap()["username"], "alex");
    assert_eq!(f.p.projection(&i2).unwrap()["username"], "owner");
    assert_ne!(i1.admin, i2.admin);
    let a1 = f.p.app(&i1.profile).unwrap();
    let a2 = f.p.app(&i2.profile).unwrap();
    let bot = crate::tests::bot(&a1.db, "codex");
    a1.db
        .save_setting(
            "general",
            &json!({"name":"Personal","identity":"private personal preferences"}),
        )
        .unwrap();
    crate::connections::save_openrouter(&a1, "private-key-only-personal").unwrap();
    assert_ne!(a1.config.database, a2.config.database);
    assert_ne!(a1.config.vm.managed_id, a2.config.vm.managed_id);
    assert_ne!(a1.config.vm.ssh_config, a2.config.vm.ssh_config);
    assert!(
        !Path::new(&a1.config.vm.ssh_config).exists(),
        "Signup must not provision a computer"
    );
    let (status, v) = f.request("GET", "/api/bots", t2, Value::Null).await;
    assert_eq!(status, 200);
    assert_eq!(v, json!([]));
    let (_, v) = f.request("GET", "/api/settings", t2, Value::Null).await;
    assert!(!v.to_string().contains("private personal"));
    assert!(crate::connections::openrouter_key(&a2).is_none());
    assert!(
        crate::connections::credential(&a2, "fixture", "PATH").is_none(),
        "Process-wide credentials must not enter tenant profiles"
    );
    assert_eq!(
        f.request(
            "PUT",
            &format!("/api/bots/{}/text", bot.id),
            t2,
            json!({"field":"memory","value":"attack"})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        f.request(
            "POST",
            "/identity/switch",
            t2,
            json!({"profile_id":i1.profile})
        )
        .await
        .0,
        400
    );
    let (_, v) = f
        .request("GET", "/identity/profiles", t2, Value::Null)
        .await;
    assert_eq!(v["profiles"].as_array().unwrap().len(), 1);
    assert!(!v.to_string().contains("Personal"));
    assert_eq!(f.request("GET", "/api/bots", "", Value::Null).await.0, 401);
    assert_eq!(
        f.request("GET", "/api/bots", &a1.token, Value::Null)
            .await
            .0,
        401,
        "Internal tokens must never authenticate externally"
    );
    assert_eq!(f.request("GET", "/health", "", Value::Null).await.0, 200);
}

#[tokio::test]
async fn profile_switch_rotates_only_this_device_and_syncs_unread_cursors() {
    let f = Fixture::new(false);
    let one = f.register("personal").await;
    let token = one["token"].as_str().unwrap();
    let (_, other) = f
        .request(
            "POST",
            "/identity/login",
            "",
            json!({"login":"personal","password":"test password for profiles"}),
        )
        .await;
    let second_device = other["token"].as_str().unwrap();
    let (_, created) = f
        .request("POST", "/identity/profiles", token, json!({"name":"owner"}))
        .await;
    let profile = created["id"].as_str().unwrap();
    let (_,last)=f.request("POST","/identity/login","",json!({"login":"personal","password":"test password for profiles","profile_id":profile})).await;
    assert_eq!(last["profile_id"], profile);
    let outsider = f.register("outsider").await;
    let (_,safe)=f.request("POST","/identity/login","",json!({"login":"personal","password":"test password for profiles","profile_id":outsider["profile_id"]})).await;
    assert_eq!(safe["profile_id"], one["profile_id"]);
    let app = f.p.app(profile).unwrap();
    let bot = crate::tests::bot(&app.db, "codex");
    let run = app.db.queue(&bot.id, "test", 0).unwrap();
    for i in 0..12 {
        app.db
            .event(&run, "assistant", json!({"text":format!("Reply {i}")}))
            .unwrap();
    }
    let (_, list) = f
        .request("GET", "/identity/profiles", second_device, Value::Null)
        .await;
    assert_eq!(list["profiles"].as_array().unwrap().len(), 2);
    assert_eq!(list["profiles"][1]["unread"], 12);
    let (_, switched) = f
        .request(
            "POST",
            "/identity/switch",
            token,
            json!({"profile_id":profile}),
        )
        .await;
    let next = switched["token"].as_str().unwrap();
    assert!(f.p.identity(token).is_err());
    assert!(f.p.identity(second_device).is_ok());
    assert_eq!(f.p.identity(next).unwrap().profile, profile);
    let chat = format!("dm-{}", bot.id);
    let cursor = app.db.attention().unwrap()["chats"][&chat]["cursor"]
        .as_i64()
        .unwrap();
    assert_eq!(
        f.request(
            "PUT",
            &format!("/api/chats/{chat}/read"),
            next,
            json!({"cursor":cursor})
        )
        .await
        .0,
        200
    );
    let (_, list) = f
        .request("GET", "/identity/profiles", second_device, Value::Null)
        .await;
    assert_eq!(list["profiles"][1]["unread"], 0);
    drop(app);
    f.p.apps.lock().unwrap().clear();
    let reopened = Profiles::open(f.p.config.clone(), None, false).unwrap();
    assert_eq!(reopened.identity(next).unwrap().profile, profile);
    assert_eq!(
        reopened
            .projection(&reopened.identity(next).unwrap())
            .unwrap()["profiles"][1]["unread"],
        0
    );
}

#[tokio::test]
async fn legacy_claim_preserves_data_and_old_devices_never_gain_other_profiles() {
    let f = Fixture::new(true);
    let old = f.p.legacy.as_ref().unwrap().clone();
    let b = crate::tests::bot(&old.db, "codex");
    let body =
        json!({"login":"owner","name":"Alex Morgan","password":"test password for profiles"});
    assert_eq!(
        f.request("POST", "/identity/register", "", body.clone())
            .await
            .0,
        400
    );
    assert_eq!(
        f.request("POST", "/identity/register", &old.token, body.clone())
            .await
            .0,
        400
    );
    let mut body = body;
    body["claim_legacy"] = json!(true);
    let (status, claimed) = f
        .request("POST", "/identity/register", &old.token, body)
        .await;
    assert_eq!(status, 200, "{claimed}");
    assert_eq!(claimed["profile_id"], "legacy");
    let owner = claimed["token"].as_str().unwrap();
    let (_, bots) = f.request("GET", "/api/bots", owner, Value::Null).await;
    assert_eq!(bots[0]["id"], b.id);
    let (_, profile) = f
        .request("POST", "/identity/profiles", owner, json!({"name":"Work"}))
        .await;
    assert_eq!(
        f.request(
            "POST",
            "/identity/switch",
            &old.token,
            json!({"profile_id":profile["id"]})
        )
        .await
        .0,
        400
    );
    let (_, list) = f
        .request("GET", "/identity/profiles", &old.token, Value::Null)
        .await;
    assert_eq!(list["profiles"].as_array().unwrap().len(), 1);
    assert_eq!(list["claim_available"], false);
    assert_eq!(
        f.request("GET", "/api/bots", &old.token, Value::Null)
            .await
            .0,
        200
    );
    assert_eq!(
        f.request("GET", "/identity/admin", &old.token, Value::Null)
            .await
            .0,
        400
    );
}

#[tokio::test]
async fn invitations_disabled_accounts_logout_and_pairing_are_scoped() {
    let f = Fixture::new(false);
    let owner = f.register("admin").await;
    let admin = owner["token"].as_str().unwrap();
    f.request(
        "POST",
        "/identity/admin",
        admin,
        json!({"action":"registration","open":false}),
    )
    .await;
    let body = json!({"login":"team","name":"Team","password":"test password for profiles"});
    assert_eq!(
        f.request("POST", "/identity/register", "", body.clone())
            .await
            .0,
        400
    );
    let (_, invite) = f
        .request("POST", "/identity/admin", admin, json!({"action":"invite"}))
        .await;
    let mut body = body;
    body["invite"] = invite["invite"].clone();
    let (status, team) = f
        .request("POST", "/identity/register", "", body.clone())
        .await;
    assert_eq!(status, 200);
    body["login"] = json!("replay");
    assert_eq!(
        f.request("POST", "/identity/register", "", body).await.0,
        400
    );
    let token = team["token"].as_str().unwrap();
    assert_eq!(
        f.request("GET", "/identity/admin", token, Value::Null)
            .await
            .0,
        400
    );
    let (_, link) = f
        .request("POST", "/api/devices/link", token, json!({}))
        .await;
    let code = link["url"]
        .as_str()
        .unwrap()
        .split("#pair=")
        .nth(1)
        .unwrap();
    assert_eq!(
        f.request("POST", "/device/claim", "", json!({"code":code}))
            .await
            .0,
        400
    );
    let claim = || {
        Request::builder()
            .method("POST")
            .uri("/device/claim")
            .header("content-type", "application/json")
            .header("origin", "http://127.0.0.1:7340")
            .body(Body::from(json!({"code":code}).to_string()))
            .unwrap()
    };
    assert_eq!(
        router(f.p.clone()).oneshot(claim()).await.unwrap().status(),
        200
    );
    assert_eq!(
        router(f.p.clone()).oneshot(claim()).await.unwrap().status(),
        400
    );
    let team_identity = f.p.identity(token).unwrap();
    let team_app = f.p.app(&team_identity.profile).unwrap();
    let bot = crate::tests::bot(&team_app.db, "codex");
    let queued = team_app
        .db
        .queue(&bot.id, "disabled account must not run", 0)
        .unwrap();
    let id = team_identity.account;
    assert_eq!(
        f.request(
            "POST",
            "/identity/admin",
            admin,
            json!({"action":"disable","user_id":id,"disabled":true})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        f.request("GET", "/api/bots", token, Value::Null).await.0,
        401
    );
    assert!(team_app.account_disabled());
    assert_eq!(team_app.db.run(&queued).unwrap().status, "cancelled");
    assert_eq!(
        f.request(
            "POST",
            "/identity/login",
            "",
            json!({"login":"team","password":"test password for profiles"})
        )
        .await
        .0,
        400
    );
    let account = f.p.identity(admin).unwrap().account;
    assert_eq!(
        f.request(
            "POST",
            "/identity/admin",
            admin,
            json!({"action":"disable","user_id":account,"disabled":true})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        f.request("POST", "/identity/logout", admin, json!({}))
            .await
            .0,
        200
    );
    assert!(f.p.identity(admin).is_err());
}

#[tokio::test]
async fn passwords_sessions_and_private_origins_fail_closed() {
    let f = Fixture::new(false);
    for password in ["a", "ab", "abc", "ééé"] {
        let (status, value) = f
            .request(
                "POST",
                "/identity/register",
                "",
                json!({"login":"owner","name":"Owner","password":password}),
            )
            .await;
        assert_eq!(status, 400, "{value}");
        assert_eq!(value["error"], "Use a password of at least 4 characters");
    }
    let (status, one) = f
        .request(
            "POST",
            "/identity/register",
            "",
            json!({"login":"owner","name":"Owner","password":"aaaa"}),
        )
        .await;
    assert_eq!(status, 200, "{one}");
    let token = one["token"].as_str().unwrap();
    assert_eq!(
        f.request(
            "POST",
            "/identity/login",
            "",
            json!({"login":"owner","password":"aaaa"})
        )
        .await
        .0,
        200
    );
    for password in ["1", "12", "123", "ééé"] {
        let (status, value) = f
            .request(
                "POST",
                "/identity/password",
                token,
                json!({"current_password":"aaaa","password":password}),
            )
            .await;
        assert_eq!(status, 400, "{value}");
        assert_eq!(value["error"], "Use a password of at least 4 characters");
        assert!(
            f.p.identity(token).is_ok(),
            "Rejected password changes preserve the session"
        );
    }
    let (status, v) = f
        .request(
            "POST",
            "/identity/password",
            token,
            json!({"current_password":"wrong password","password":"1234"}),
        )
        .await;
    assert_eq!(status, 400, "{v}");
    let (status, v) = f
        .request(
            "POST",
            "/identity/password",
            token,
            json!({"current_password":"aaaa","password":"1234"}),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert!(f.p.identity(token).is_err());
    assert!(f.p.identity(v["token"].as_str().unwrap()).is_ok());
    let request = Request::builder()
        .uri("/api/bots")
        .header(
            "authorization",
            format!("Bearer {}", v["token"].as_str().unwrap()),
        )
        .header("origin", "https://evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router(f.p.clone()).oneshot(request).await.unwrap().status(),
        403
    );
    let c = f.p.registry.lock().unwrap();
    let stored: Vec<u8> = c
        .query_row("SELECT password FROM accounts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored.len(), 32);
    drop(c);
    let next = v["token"].as_str().unwrap();
    f.p.registry
        .lock()
        .unwrap()
        .execute("UPDATE sessions SET expires=0", [])
        .unwrap();
    assert!(f.p.identity(next).is_err());
    assert_eq!(
        f.request(
            "POST",
            "/identity/login",
            "",
            json!({"login":"owner","password":"aaaa"})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        f.request(
            "POST",
            "/identity/login",
            "",
            json!({"login":"owner","password":"1234"})
        )
        .await
        .0,
        200
    );
}

#[tokio::test]
async fn legacy_desktops_cannot_reuse_local_permissions_in_a_new_profile() {
    let f = Fixture::new(false);
    let one = f.register("scoped-desktop").await;
    let token = one["token"].as_str().unwrap();
    let mut body = json!({"id":db::id(),"secret":format!("{}{}",uuid::Uuid::new_v4(),uuid::Uuid::new_v4()),"mode":"off","name":"Test desktop","busy":false});
    assert_eq!(
        f.request("POST", "/api/local/poll", token, body.clone())
            .await
            .0,
        400
    );
    body["profile_id"] = json!(db::id());
    assert_eq!(
        f.request("POST", "/api/local/poll", token, body.clone())
            .await
            .0,
        400
    );
    body["profile_id"] = one["profile_id"].clone();
    assert_eq!(
        f.request("POST", "/api/local/poll", token, body).await.0,
        200
    );
}

#[tokio::test]
async fn explicit_new_profile_login_is_empty_idempotent_and_rename_preserves_bots() {
    let f = Fixture::new(false);
    let old = f.register("owner").await;
    let token = old["token"].as_str().unwrap();
    let app = f.p.app(old["profile_id"].as_str().unwrap()).unwrap();
    let bot = crate::tests::bot(&app.db, "codex");
    let request = db::id();
    let body = json!({"login":"owner","password":"test password for profiles","new_profile_name":"owner","request_id":request});
    let (status, new) = f.request("POST", "/identity/login", "", body.clone()).await;
    assert_eq!(status, 200, "{new}");
    assert_ne!(new["profile_id"], old["profile_id"]);
    let fresh = new["token"].as_str().unwrap();
    let (_, bots) = f.request("GET", "/api/bots", fresh, Value::Null).await;
    assert_eq!(bots, json!([]));
    let (_, again) = f.request("POST", "/identity/login", "", body).await;
    assert_eq!(again["profile_id"], new["profile_id"]);
    let (_, list) = f
        .request("GET", "/identity/profiles", token, Value::Null)
        .await;
    assert_eq!(list["profiles"].as_array().unwrap().len(), 2);
    let (status, _) = f
        .request(
            "POST",
            "/identity/profile",
            token,
            json!({"name":"Alex Morgan"}),
        )
        .await;
    assert_eq!(status, 200);
    let (_, list) = f
        .request("GET", "/identity/profiles", token, Value::Null)
        .await;
    assert_eq!(
        list["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["active"] == true)
            .unwrap()["name"],
        "Alex Morgan"
    );
    assert_eq!(app.db.bot(&bot.id).unwrap().id, bot.id);
    assert_eq!(
        app.db.setting("general").unwrap().unwrap()["name"],
        "Alex Morgan"
    );
}

#[tokio::test]
async fn workspace_transfer_preserves_history_and_attachments_without_credentials_or_double_scheduling()
 {
    let source = Fixture::new(false);
    let account = source.register("owner").await;
    let token = account["token"].as_str().unwrap();
    let app = source
        .p
        .app(account["profile_id"].as_str().unwrap())
        .unwrap();
    let mut bot = crate::tests::bot(&app.db, "codex");
    bot.memory = "Preserve my durable role".into();
    bot.profile.local_access = true;
    bot.profile.local_device_id = db::id();
    app.db.save_bot(&bot).unwrap();
    app.db
        .save_skill(&json!({"name":"Daily brief","body":"Write the brief","command":"daily-brief"}))
        .unwrap();
    let run = app.db.queue(&bot.id, "Keep this conversation", 0).unwrap();
    app.db
        .finish(
            &run,
            "interrupted",
            "Preserved partial result",
            "Historical interruption",
        )
        .unwrap();
    app.db.chat_complete(&app.db.run(&run).unwrap()).unwrap();
    let routine = crate::db::Routine {
        id: db::id(),
        bot_id: bot.id.clone(),
        name: "Morning".into(),
        prompt: "/daily-brief".into(),
        interval_seconds: 3600,
        next_run: db::now() + 3600,
        enabled: true,
        run_at: None,
        schedule: None,
    };
    app.db.save_routine(&routine).unwrap();
    app.db
        .save_setting("private_provider_secret", &json!("SECRET_MUST_NOT_TRAVEL"))
        .unwrap();
    app.db
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO attachments VALUES('screenshot',?,'History image',?,?)",
            params![run, vec![12u8; 24000], db::now()],
        )
        .unwrap();
    let id = db::id();
    let (status, package) = source
        .request("POST", "/identity/transfer", token, json!({"id":id}))
        .await;
    assert_eq!(status, 200, "{package}");
    assert!(!package.to_string().contains("SECRET_MUST_NOT_TRAVEL"));
    assert!(
        app.db
            .queue(&bot.id, "Cannot run during transfer", 0)
            .is_err()
    );
    app.db.tick(routine.next_run).unwrap();
    assert_eq!(app.db.runs(None).unwrap().len(), 1);
    let (_, again) = source
        .request("POST", "/identity/transfer", token, json!({"id":id}))
        .await;
    assert_eq!(package, again);
    let destination = Fixture::new(false);
    let other = destination.register("owner").await;
    let target_token = other["token"].as_str().unwrap();
    let (status, receipt) = destination
        .request(
            "POST",
            "/identity/transfer/import",
            target_token,
            package.clone(),
        )
        .await;
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(receipt["receipt"]["id"], id);
    let target = destination
        .p
        .app(other["profile_id"].as_str().unwrap())
        .unwrap();
    let copied = target.db.bot(&bot.id).unwrap();
    assert_eq!(copied.memory, bot.memory);
    assert!(!copied.profile.local_access);
    assert!(copied.profile.local_device_id.is_empty());
    assert_eq!(target.db.run(&run).unwrap().status, "interrupted");
    assert_eq!(
        target.db.run(&run).unwrap().error,
        "Historical interruption"
    );
    assert_eq!(
        target.db.attachment_png("screenshot").unwrap().unwrap(),
        vec![12u8; 24000]
    );
    assert!(!target.db.routines().unwrap()[0].enabled);
    assert!(
        target
            .db
            .setting("private_provider_secret")
            .unwrap()
            .is_none()
    );
    let (status, _) = destination
        .request(
            "POST",
            "/identity/transfer/import",
            target_token,
            package.clone(),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(target.db.bots().unwrap().len(), 1);
    let (status, _) = source
        .request(
            "POST",
            "/identity/transfer/finish",
            token,
            json!({"id":id,"destination":"https://work.example"}),
        )
        .await;
    assert_eq!(status, 200);
    assert!(app.db.queue(&bot.id, "Archived source", 0).is_err());
    assert_eq!(app.db.routines().unwrap()[0].next_run, routine.next_run);
}
#[tokio::test]
async fn transfer_rejects_active_work_overwrite_and_corrupt_rows_and_can_cancel() {
    let source = Fixture::new(false);
    let owner = source.register("owner").await;
    let token = owner["token"].as_str().unwrap();
    let app = source.p.app(owner["profile_id"].as_str().unwrap()).unwrap();
    let bot = crate::tests::bot(&app.db, "codex");
    let run = app.db.queue(&bot.id, "Pending task", 0).unwrap();
    let id = db::id();
    assert_eq!(
        source
            .request("POST", "/identity/transfer", token, json!({"id":id}))
            .await
            .0,
        400
    );
    app.db.finish(&run, "cancelled", "", "").unwrap();
    app.db.chat_complete(&app.db.run(&run).unwrap()).unwrap();
    let (_, package) = source
        .request("POST", "/identity/transfer", token, json!({"id":id}))
        .await;
    let target = crate::db::Db::open(":memory:").unwrap();
    let mut corrupt = package.clone();
    corrupt["tables"]["runs"]["rows"][0][1] = json!("missing-owner");
    assert!(target.import_transfer(&corrupt).is_err());
    assert!(target.bots().unwrap().is_empty());
    crate::tests::bot(&target, "codex");
    assert!(
        target
            .import_transfer(&package)
            .unwrap_err()
            .to_string()
            .contains("empty profile")
    );
    assert_eq!(
        source
            .request("POST", "/identity/transfer/cancel", token, json!({"id":id}))
            .await
            .0,
        200
    );
    assert!(app.db.queue(&bot.id, "Source resumed", 0).is_ok());
    assert!(app.db.transfer_status().unwrap().is_null());
}

#[tokio::test]
async fn computer_resource_controls_require_admin_and_use_current_profile() {
    let f = Fixture::new(false);
    let owner = f.register("owner").await;
    let member = f.register("member").await;
    let token = owner["token"].as_str().unwrap();
    let profile = f.p.identity(token).unwrap().profile;
    std::fs::write(f.root.join("fixture-manager.py"), "import json,sys\nprint(json.dumps({'profile':sys.argv[2],'state':'shut off','resources':{'cpus':2,'memory_mb':6144,'disk_gb':30}}))\n").unwrap();
    for method in ["GET", "POST"] {
        let (status, _) = f
            .request(
                method,
                "/identity/computer-settings",
                member["token"].as_str().unwrap(),
                json!({"cpus":4,"memory_mb":8192,"disk_gb":40}),
            )
            .await;
        assert!(!status.is_success());
        let (status, value) = f
            .request(
                method,
                "/identity/computer-settings",
                token,
                json!({"cpus":4,"memory_mb":8192,"disk_gb":40}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["profile"], profile);
    }
}

#[tokio::test]
async fn server_update_requires_administrator_and_valid_release() {
    let f=Fixture::new(false);
    let owner=f.register("update-owner").await;
    let member=f.register("update-member").await;
    for method in ["GET", "POST"] {
        for token in ["", member["token"].as_str().unwrap()] {
            let (status, _)=f.request(method,"/identity/server-update",token,json!({"version":"0.83.0"})).await;
            assert!(!status.is_success());
        }
    }
    for version in ["", "../latest", "1.2", "1.2.3;reboot", "https://example.com"] {
        let (status, _)=f.request("POST","/identity/server-update",owner["token"].as_str().unwrap(),json!({"version":version})).await;
        assert!(!status.is_success());
    }
}

#[tokio::test]
async fn transfer_registration_retry_logs_into_the_same_empty_destination() {
    let destination=Fixture::new(false);
    let request=db::id();
    let credentials=json!({"login":"moving","name":"Workspace","password":"fixture migration password","request_id":request});
    let (status,first)=destination.request("POST","/identity/register","",credentials.clone()).await;
    assert_eq!(status,200,"{first}");
    // The desktop may have lost the successful registration response.
    let (status,_)=destination.request("POST","/identity/register","",credentials.clone()).await;
    assert_eq!(status,400);
    let mut retry=credentials;retry["new_profile_name"]=json!("Workspace");
    let (status,recovered)=destination.request("POST","/identity/login","",retry).await;
    assert_eq!(status,200,"{recovered}");
    assert_eq!(recovered["profile_id"],first["profile_id"]);
    let (_,identity)=destination.request("GET","/identity/profiles",recovered["token"].as_str().unwrap(),Value::Null).await;
    assert_eq!(identity["profiles"].as_array().unwrap().len(),1);
}

#[tokio::test]
async fn invited_users_have_independent_profiles_and_removal_cleans_all_private_data() {
    let f = Fixture::new(false);
    let admin = f.register("administrator").await;
    let token = admin["token"].as_str().unwrap();
    f.request(
        "POST",
        "/identity/admin",
        token,
        json!({"action":"registration","open":false}),
    )
    .await;
    let mut users = Vec::new();
    for login in ["member-one", "member-two"] {
        let (_, invite) = f
            .request("POST", "/identity/admin", token, json!({"action":"invite"}))
            .await;
        let body = json!({"login":login,"name":"Personal","password":"test password for profiles","invite":invite["invite"]});
        let (status, user) = f
            .request("POST", "/identity/register", "", body.clone())
            .await;
        assert_eq!(status, 200);
        let mut replay = body;
        replay["login"] = json!(format!("{login}-replay"));
        assert_ne!(
            f.request("POST", "/identity/register", "", replay).await.0,
            200
        );
        let t = user["token"].as_str().unwrap();
        let (status, work) = f
            .request("POST", "/identity/profiles", t, json!({"name":"Work"}))
            .await;
        assert_eq!(status, 200);
        let (_, projection) = f.request("GET", "/identity/profiles", t, Value::Null).await;
        assert_eq!(projection["profiles"].as_array().unwrap().len(), 2);
        users.push((user, work));
    }
    let first = users[0].0["token"].as_str().unwrap();
    let second = users[1].0["token"].as_str().unwrap();
    assert_ne!(
        f.request(
            "POST",
            "/identity/switch",
            second,
            json!({"profile_id":users[0].1["id"]})
        )
        .await
        .0,
        200
    );
    let identity = f.p.identity(first).unwrap();
    let other_account=f.p.identity(second).unwrap().account;
    let room=json!({"id":"shared-room","name":"Team","owner":identity.account,"participants":[{"id":format!("person:{}",identity.account),"kind":"person","name":"Member one","account":identity.account},{"id":format!("person:{other_account}"),"kind":"person","name":"Member two","account":other_account}]});
    f.p.registry.lock().unwrap().execute("INSERT INTO server_rooms(id,body,created) VALUES(?,?,?)",params!["shared-room",room.to_string(),db::now()]).unwrap();
    let body = json!({"user_id":identity.account,"confirm":"member-one"});
    assert_ne!(
        f.request("POST", "/identity/admin/remove", second, body.clone())
            .await
            .0,
        200
    );
    assert_ne!(
        f.request(
            "POST",
            "/identity/admin/remove",
            token,
            json!({"user_id":f.p.identity(token).unwrap().account,"confirm":"administrator"})
        )
        .await
        .0,
        200
    );
    assert_ne!(
        f.request(
            "POST",
            "/identity/admin/remove",
            token,
            json!({"user_id":identity.account,"confirm":"wrong"})
        )
        .await
        .0,
        200
    );
    let ids = [
        identity.profile.clone(),
        users[0].1["id"].as_str().unwrap().into(),
    ];
    for id in &ids {
        let root = f.root.join(id).join("computer");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("disk.qcow2"), b"private disk").unwrap();
    }
    // Failed VM shutdown must retain the account and disks, but revoke access.
    std::fs::write(
        f.root.join("fixture-manager.py"),
        "raise SystemExit('VM still busy')",
    )
    .unwrap();
    assert_ne!(
        f.request("POST", "/identity/admin/remove", token, body.clone())
            .await
            .0,
        200
    );
    assert!(f.p.identity(first).is_err());
    assert!(f.root.join(&ids[0]).exists());
    assert_ne!(
        f.request(
            "POST",
            "/identity/admin",
            token,
            json!({"action":"disable","user_id":identity.account,"disabled":false})
        )
        .await
        .0,
        200
    );
    std::fs::write(
        f.root.join("fixture-manager.py"),
        "import sys,json\nassert sys.argv[1]=='retire'\nprint(json.dumps({'state':'retired'}))\n",
    )
    .unwrap();
    let (status, result) = f
        .request("POST", "/identity/admin/remove", token, body)
        .await;
    assert_eq!(status, 200, "{result}");
    for id in ids {
        assert!(!f.root.join(&id).exists());
        assert!(!f.p.apps.lock().unwrap().contains_key(&id));
    }
    let room:String=f.p.registry.lock().unwrap().query_row("SELECT body FROM server_rooms WHERE id='shared-room'",[],|r|r.get(0)).unwrap();
    let room:Value=serde_json::from_str(&room).unwrap();assert_eq!(room["participants"].as_array().unwrap().len(),1);assert_eq!(room["owner"],other_account);
    assert!(f.p.identity(second).is_ok());
    assert!(
        f.root
            .join(users[1].0["profile_id"].as_str().unwrap())
            .exists()
    );
    let (_, users) = f
        .request("GET", "/identity/admin", token, Value::Null)
        .await;
    assert_eq!(users["users"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn password_reset_requires_admin_approval_and_requesting_browser_proof() {
    let f=Fixture::new(false);let owner=f.register("admin").await;let member=f.register("member").await;
    let admin=owner["token"].as_str().unwrap();let user=member["token"].as_str().unwrap();
    let req=Request::builder().method("POST").uri("/identity/password-reset/request").header("content-type","application/json").header("x-forwarded-for","203.0.113.99").extension(axum::extract::ConnectInfo("192.0.2.10:4567".parse::<std::net::SocketAddr>().unwrap())).body(Body::from(r#"{"login":"member"}"#)).unwrap();
    let response=router(f.p.clone()).oneshot(req).await.unwrap();assert_eq!(response.status(),200);
    let request:Value=serde_json::from_slice(&to_bytes(response.into_body(),16384).await.unwrap()).unwrap();
    let (_,list)=f.request("GET","/identity/admin/password-resets",admin,Value::Null).await;
    assert_eq!(list["requests"][0]["ip"],"192.0.2.10");assert!(list["requests"][0]["created"].as_i64().unwrap()>0);
    assert!(!list.to_string().contains(request["token"].as_str().unwrap()));
    let save=json!({"token":request["token"],"password":"new test password","confirm_password":"new test password"});
    assert_ne!(f.request("POST","/identity/password-reset/finish","",save.clone()).await.0,200);
    let approval=json!({"id":request["id"],"action":"approve"});
    assert_ne!(f.request("POST","/identity/admin/password-resets",user,approval.clone()).await.0,200);
    assert_eq!(f.request("POST","/identity/admin/password-resets",admin,approval.clone()).await.0,200);
    assert_ne!(f.request("POST","/identity/admin/password-resets",admin,approval).await.0,200);
    let (_,state)=f.request("POST","/identity/password-reset/status","",json!({"token":request["token"]})).await;assert_eq!(state["state"],"approved");
    let mut wrong=save.clone();wrong["token"]=json!("wrong browser proof");assert_ne!(f.request("POST","/identity/password-reset/finish","",wrong).await.0,200);
    let mut wrong=save.clone();wrong["confirm_password"]=json!("mismatch");assert_ne!(f.request("POST","/identity/password-reset/finish","",wrong).await.0,200);
    assert_eq!(f.request("POST","/identity/password-reset/finish","",save.clone()).await.0,200);
    assert!(f.p.identity(user).is_err());assert!(f.p.identity(admin).is_ok());
    assert_ne!(f.request("POST","/identity/password-reset/finish","",save).await.0,200);
    assert_ne!(f.request("POST","/identity/login","",json!({"login":"member","password":"test password for profiles"})).await.0,200);
    assert_eq!(f.request("POST","/identity/login","",json!({"login":"member","password":"new test password"})).await.0,200);
    let (_,denied)=f.request("POST","/identity/password-reset/request","",json!({"login":"member"})).await;
    assert_eq!(f.request("POST","/identity/admin/password-resets",admin,json!({"id":denied["id"],"action":"deny"})).await.0,200);
    let (_,state)=f.request("POST","/identity/password-reset/status","",json!({"token":denied["token"]})).await;assert_eq!(state["state"],"denied");
    f.p.registry.lock().unwrap().execute("UPDATE password_resets SET expires=0",[]).unwrap();
    let (_,state)=f.request("POST","/identity/password-reset/status","",json!({"token":denied["token"]})).await;assert_eq!(state["state"],"expired");
    let (_,unknown)=f.request("POST","/identity/password-reset/request","",json!({"login":"unknown"})).await;assert_eq!(unknown["state"],"pending");
    let (_,list)=f.request("GET","/identity/admin/password-resets",admin,Value::Null).await;assert!(list["requests"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn switching_accounts_does_not_extend_sign_in_lifetime() {
    let f=Fixture::new(false);
    let (status,user)=f.request("POST","/identity/register","",json!({"login":"owner","name":"Owner","password":"test-password"})).await;
    assert_eq!(status,200,"{user}");
    let token=user["token"].as_str().unwrap();
    let expiry=db::now()+3600;
    f.p.registry.lock().unwrap().execute("UPDATE sessions SET expires=? WHERE digest=?",params![expiry,hash(token)]).unwrap();
    let (status,switched)=f.request("POST","/identity/switch",token,json!({"profile_id":user["profile_id"]})).await;
    assert_eq!(status,200,"{switched}");
    let rotated=switched["token"].as_str().unwrap();
    let actual:i64=f.p.registry.lock().unwrap().query_row("SELECT expires FROM sessions WHERE digest=?",[hash(rotated)],|r|r.get(0)).unwrap();
    assert_eq!(actual,expiry);
    assert!(f.p.identity(token).is_err());
}

struct RecordingPush(Mutex<Vec<crate::mobile_push::Push>>);
impl crate::mobile_push::Transport for RecordingPush {
    fn platforms(&self) -> crate::mobile_push::Platforms {
        crate::mobile_push::Platforms { ios: true, android: true }
    }
    fn send<'a>(&'a self, push: &'a crate::mobile_push::Push) -> crate::mobile_push::SendFuture<'a> {
        self.0.lock().unwrap().push(push.clone());
        Box::pin(async { crate::mobile_push::Outcome::Delivered })
    }
}

fn finished_run(app: &Shared, bot: &str) -> i64 {
    let run = app.db.queue(bot, "Private prompt", 0).unwrap();
    app.db.finish(&run, "completed", "Private answer", "").unwrap();
    app.db.event(&run, "run_finished", json!({})).unwrap();
    app.db.0.lock().unwrap().query_row("SELECT MAX(seq) FROM events", [], |r| r.get(0)).unwrap()
}

#[tokio::test]
async fn mobile_devices_cover_every_owned_profile_and_follow_the_session() {
    let f = Fixture::new(false);
    let user = f.register("phone-owner").await;
    let token = user["token"].as_str().unwrap().to_owned();
    let first = user["profile_id"].as_str().unwrap().to_owned();
    let (_, me) = f.request("GET", "/identity/profiles", &token, Value::Null).await;
    let account = me["account_id"].as_str().unwrap().to_owned();
    // A second owned profile exists before the phone registers.
    let (_, created) = f.request("POST", "/identity/profiles", &token, json!({"name":"Second"})).await;
    let second = created["id"].as_str().unwrap().to_owned();
    let installation = db::id();
    let path = format!("/api/mobile/devices/{installation}");
    let body = json!({"platform":"android","token":"fcm-registration-token-1","environment":"production","account_id":account});
    assert_eq!(f.request("PUT", &path, "", body.clone()).await.0, 401);
    assert_eq!(f.request("PUT", &path, "x".repeat(64).as_str(), body.clone()).await.0, 401);
    let mut other = body.clone();
    other["account_id"] = json!(db::id());
    assert_eq!(f.request("PUT", &path, &token, other).await.0, 400);
    assert_eq!(f.request("PUT", "/api/mobile/devices/not-a-uuid", &token, body.clone()).await.0, 400);
    let (status, saved) = f.request("PUT", &path, &token, body.clone()).await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!((saved["delivery_enabled"].clone(), saved["profiles"].clone()), (json!(false), json!(2)));
    assert!(!saved.to_string().contains("fcm-registration-token-1"));
    let (status, push) = f.request("GET", &format!("/api/mobile/push-status?installation_uuid={installation}"), &token, Value::Null).await;
    assert_eq!(status, 200);
    assert_eq!(push, json!({"enabled":false,"platforms":{"ios":false,"android":false},"registered":true,"profiles":2,"registered_profiles":2}));
    assert_eq!(f.request("GET", "/api/mobile/push-status", "", Value::Null).await.0, 401);

    // Bots in both owned profiles alert while the session sits on the first.
    let (one, two) = (f.p.app(&first).unwrap(), f.p.app(&second).unwrap());
    let transport = Arc::new(RecordingPush(Default::default()));
    f.p.mobile_push_tick(transport.clone()).await;
    let (bot_one, bot_two) = (crate::tests::bot(&one.db, "codex"), crate::tests::bot(&two.db, "codex"));
    let event_one = finished_run(&one, &bot_one.id);
    let event_two = finished_run(&two, &bot_two.id);
    f.p.mobile_push_tick(transport.clone()).await;
    {
        let mut sent: Vec<_> = transport.0.lock().unwrap().iter().map(|p| (p.profile_id.clone(), p.chat_id.clone(), p.event_id, p.account_id.clone(), p.installation.clone())).collect();
        sent.sort();
        let mut expected = vec![(first.clone(), format!("dm-{}", bot_one.id), event_one, account.clone(), installation.clone()), (second.clone(), format!("dm-{}", bot_two.id), event_two, account.clone(), installation.clone())];
        expected.sort();
        assert_eq!(sent, expected);
    }
    transport.0.lock().unwrap().clear();

    // Switching profiles rotates the session; registrations in every owned profile follow it.
    let (status, switched) = f.request("POST", "/identity/switch", &token, json!({"profile_id":second})).await;
    assert_eq!(status, 200, "{switched}");
    let token = switched["token"].as_str().unwrap().to_owned();
    f.p.mobile_push_tick(transport.clone()).await;
    assert_eq!((one.db.mobile_devices().unwrap().len(), two.db.mobile_devices().unwrap().len()), (1, 1));
    finished_run(&one, &bot_one.id);
    f.p.mobile_push_tick(transport.clone()).await;
    assert_eq!(transport.0.lock().unwrap().iter().map(|p| p.profile_id.clone()).collect::<Vec<_>>(), vec![first.clone()]);

    // A profile created later inherits the phone, starting at its own present.
    let (_, third) = f.request("POST", "/identity/profiles", &token, json!({"name":"Third"})).await;
    let three = f.p.app(third["id"].as_str().unwrap()).unwrap();
    assert_eq!(three.db.mobile_devices().unwrap().len(), 1);
    assert_eq!(three.db.mobile_devices().unwrap()[0].installation, installation);

    // Another account's profiles never receive this account's registration or events.
    let other_user = f.register("other-phone-owner").await;
    let other_token = other_user["token"].as_str().unwrap();
    let other_app = f.p.app(other_user["profile_id"].as_str().unwrap()).unwrap();
    assert!(other_app.db.mobile_devices().unwrap().is_empty());
    let other_bot = crate::tests::bot(&other_app.db, "codex");
    transport.0.lock().unwrap().clear();
    finished_run(&other_app, &other_bot.id);
    f.p.mobile_push_tick(transport.clone()).await;
    assert!(transport.0.lock().unwrap().is_empty());
    // The other account cannot remove or probe this account's installation.
    assert_eq!(f.request("DELETE", &path, other_token, Value::Null).await.1["removed"], false);
    assert_eq!(one.db.mobile_devices().unwrap().len(), 1);
    // A device moved into a profile the account no longer owns is not live there.
    other_app.db.register_mobile_device(&crate::mobile_push::Registration::parse(&installation, &body, &account).unwrap(), &hash(&token), "forged").unwrap();
    f.p.mobile_push_tick(transport.clone()).await;
    assert!(other_app.db.mobile_devices().unwrap().is_empty());

    // Password rotation keeps registrations in every owned profile.
    let (status, changed) = f.request("POST", "/identity/password", &token, json!({"current_password":"test password for profiles","password":"a new password"})).await;
    assert_eq!(status, 200, "{changed}");
    let token = changed["token"].as_str().unwrap().to_owned();
    f.p.mobile_push_tick(transport.clone()).await;
    for app in [&one, &two, &three] {
        assert_eq!(app.db.mobile_devices().unwrap().len(), 1);
    }

    // Session expiry removes them everywhere on the next pass.
    f.p.registry.lock().unwrap().execute("UPDATE sessions SET expires=0 WHERE digest=?", [hash(&token)]).unwrap();
    f.p.mobile_push_tick(transport.clone()).await;
    for app in [&one, &two, &three] {
        assert!(app.db.mobile_devices().unwrap().is_empty());
    }

    // Signing out removes them immediately from every owned profile.
    let login = json!({"login":"phone-owner","password":"a new password"});
    let token = f.request("POST", "/identity/login", "", login.clone()).await.1["token"].as_str().unwrap().to_owned();
    assert_eq!(f.request("PUT", &path, &token, body.clone()).await.0, 200);
    assert_eq!(f.request("POST", "/identity/logout", &token, json!({})).await.0, 200);
    for app in [&one, &two, &three] {
        assert!(app.db.mobile_devices().unwrap().is_empty());
    }

    // Delete covers every owned profile and is idempotent; sign out everywhere clears all.
    let token = f.request("POST", "/identity/login", "", login.clone()).await.1["token"].as_str().unwrap().to_owned();
    assert_eq!(f.request("PUT", &path, &token, body.clone()).await.0, 200);
    let (status, removed) = f.request("DELETE", &path, &token, Value::Null).await;
    assert_eq!((status, removed["removed"].clone()), (StatusCode::OK, json!(true)));
    for app in [&one, &two, &three] {
        assert!(app.db.mobile_devices().unwrap().is_empty());
    }
    assert_eq!(f.request("DELETE", &path, &token, Value::Null).await.1["removed"], false);
    assert_eq!(f.request("PUT", &path, &token, body.clone()).await.0, 200);
    assert_eq!(f.request("POST", "/identity/logout", &token, json!({"all_devices":true})).await.0, 200);
    for app in [&one, &two, &three] {
        assert!(app.db.mobile_devices().unwrap().is_empty());
    }
}

#[tokio::test]
async fn legacy_device_tokens_cannot_register_mobile_push() {
    let f = Fixture::new(true);
    let path = format!("/api/mobile/devices/{}", db::id());
    let body = json!({"platform":"ios","token":"ab".repeat(32),"environment":"production","account_id":db::id()});
    assert_eq!(f.request("PUT", &path, "legacy-owner-token-12345678901234567890", body).await.0, 401);
}

fn mobile_pairing_fixture() -> Fixture {
    let mut f=Fixture::new(false);
    let mut config=f.p.config.clone();
    config.allowed_origins=vec!["https://kindred.example".into(),"https://server.example".into()];
    f.p=Profiles::open(config,None,false).unwrap();
    f
}
fn mobile_pairing_code(value: &Value) -> String {
    let url=reqwest::Url::parse(value["url"].as_str().unwrap()).unwrap();
    assert_eq!(url.scheme(),"kindred"); assert_eq!(url.host_str(),Some("pair"));
    let code=url.fragment().unwrap().strip_prefix("code=").unwrap().to_owned();
    assert_eq!(code.len(),64);code
}
#[tokio::test]
async fn mobile_pairing_preserves_account_profile_and_consumes_once() {
    let f=mobile_pairing_fixture();let account=f.register("phone-owner").await;
    let token=account["token"].as_str().unwrap();let identity=f.p.identity(token).unwrap();
    let (unauth,_)=f.request("POST","/identity/mobile-pairing","",json!({"server":"https://kindred.example"})).await;
    assert_ne!(unauth,200);
    let (_,profile)=f.request("POST","/identity/profiles",token,json!({"name":"Work"})).await;
    let (_,switched)=f.request("POST","/identity/switch",token,json!({"profile_id":profile["id"]})).await;
    let token=switched["token"].as_str().unwrap();
    let (status,issued)=f.request("POST","/identity/mobile-pairing",token,json!({"server":"https://kindred.example"})).await;
    assert_eq!(status,200,"{issued}");let code=mobile_pairing_code(&issued);
    let width=issued["qr"]["width"].as_u64().unwrap() as usize;
    assert_eq!(issued["qr"]["modules"].as_array().unwrap().len(),width*width);
    let stored:Vec<u8>=f.p.registry.lock().unwrap().query_row("SELECT digest FROM mobile_pairings WHERE id=?",[issued["id"].as_str().unwrap()],|r|r.get(0)).unwrap();
    assert_eq!(stored,hash(&code));assert_ne!(stored,code.as_bytes());
    let claim=f.request("POST","/identity/mobile-pairing/claim","",json!({"code":code}));
    let duplicate=f.request("POST","/identity/mobile-pairing/claim","",json!({"code":code}));
    let (a,b)=tokio::join!(claim,duplicate);assert_ne!(a.0,b.0);
    let phone=if a.0==200 {a.1}else{assert_eq!(b.0,200);b.1};
    let mobile_token=phone["token"].as_str().unwrap();assert_ne!(mobile_token,token);
    let mobile=f.p.identity(mobile_token).unwrap();assert_eq!(mobile.account,identity.account);assert_eq!(mobile.profile,profile["id"].as_str().unwrap());
    assert_eq!(phone["login"],"phone-owner");assert_eq!(phone["account_id"],identity.account);
    let path=format!("/identity/mobile-pairing/{}",issued["id"].as_str().unwrap());
    assert_eq!(f.request("GET",&path,token,Value::Null).await.1["status"],"claimed");
    f.request("POST","/identity/logout",token,json!({})).await;
    assert!(f.p.identity(mobile_token).is_ok(),"Phone has its own session");
}
#[tokio::test]
async fn mobile_pairing_expires_cancels_and_tracks_issuer_revocation() {
    let f=mobile_pairing_fixture();let one=f.register("pair-one").await;let two=f.register("pair-two").await;
    let token=one["token"].as_str().unwrap();let other=two["token"].as_str().unwrap();
    for mode in ["cancel","replace","expire","revoke"] {
        let (_,issued)=f.request("POST","/identity/mobile-pairing",token,json!({"server":"https://server.example"})).await;
        let path=format!("/identity/mobile-pairing/{}",issued["id"].as_str().unwrap());
        f.request("DELETE",&path,other,Value::Null).await;
        assert_eq!(f.request("GET",&path,token,Value::Null).await.1["status"],"pending");
        assert_eq!(f.request("GET",&path,other,Value::Null).await.1["status"],"expired");
        match mode {
            "cancel"=>{f.request("DELETE",&path,token,Value::Null).await;},
            "replace"=>{f.request("POST","/identity/mobile-pairing",token,json!({"server":"https://server.example"})).await;},
            "expire"=>{f.p.registry.lock().unwrap().execute("UPDATE mobile_pairings SET expires=0",[]).unwrap();},
            _=>{f.request("POST","/identity/logout",token,json!({})).await;},
        }
        let (status,_)=f.request("POST","/identity/mobile-pairing/claim","",json!({"code":mobile_pairing_code(&issued)})).await;
        assert_ne!(status,200,"{mode}");
    }
}
#[tokio::test]
async fn mobile_pairing_rejects_local_addresses_and_untrusted_browser_claims() {
    let f=mobile_pairing_fixture();let one=f.register("pair-origin").await;let token=one["token"].as_str().unwrap();
    for address in ["http://server.example","https://localhost","https://127.0.0.1","https://[::1]","https://[::ffff:127.0.0.1]","https://0.0.0.0","https://user@server.example","https://server.example/path","https://server.example?query=yes","https://unconfigured.example","https://server.example."] {
        assert_ne!(f.request("POST","/identity/mobile-pairing",token,json!({"server":address})).await.0,200,"{address}");
    }
    let (_,issued)=f.request("POST","/identity/mobile-pairing",token,json!({"server":"https://server.example"})).await;
    let body=json!({"code":mobile_pairing_code(&issued)});
    let request=Request::builder().method("POST").uri("/identity/mobile-pairing/claim").header("content-type","application/json").header("origin","https://evil.example").body(Body::from(body.to_string())).unwrap();
    assert_ne!(router(f.p.clone()).oneshot(request).await.unwrap().status(),200);
    assert_eq!(f.request("POST","/identity/mobile-pairing/claim","",body).await.0,200);
}

#[tokio::test]
async fn mobile_pairing_private_http_keeps_exact_origin_account_and_workspace() {
    let mut f=mobile_pairing_fixture();let mut config=f.p.config.clone();config.allowed_origins.extend(["http://192.168.1.20:9444".into(),"http://[fd7a:115c::5]:9444".into()]);f.p=Profiles::open(config,None,false).unwrap();
    let one=f.register("private-owner").await;let two=f.register("private-other").await;let token=one["token"].as_str().unwrap();let other=two["token"].as_str().unwrap();
    let (_,profile)=f.request("POST","/identity/profiles",token,json!({"name":"Private work"})).await;let (_,switched)=f.request("POST","/identity/switch",token,json!({"profile_id":profile["id"]})).await;let token=switched["token"].as_str().unwrap();
    assert_eq!(f.request("GET","/identity/meta","",Value::Null).await.1["mobile_pairing_private_http"],true);
    for server in ["http://192.168.1.20:9444","http://[fd7a:115c::5]:9444"] {
        let (status,issued)=f.request("POST","/identity/mobile-pairing",token,json!({"server":server})).await;assert_eq!(status,200,"{issued}");assert_eq!(issued["server"],server);let url=reqwest::Url::parse(issued["url"].as_str().unwrap()).unwrap();assert_eq!(url.query_pairs().find(|(k,_)|k=="server").unwrap().1,server);
        let path=format!("/identity/mobile-pairing/{}",issued["id"].as_str().unwrap());assert_eq!(f.request("GET",&path,other,Value::Null).await.1["status"],"expired");
        let code=mobile_pairing_code(&issued);
        for bad_origin in ["https://192.168.1.20:9444","http://192.168.1.20:9445","http://192.168.1.21:9444","http://8.8.8.8:9444"] {
            let request=Request::builder().method("POST").uri("/identity/mobile-pairing/claim").header("content-type","application/json").header("origin",bad_origin).body(Body::from(json!({"code":code}).to_string())).unwrap();
            assert_ne!(router(f.p.clone()).oneshot(request).await.unwrap().status(),200,"{bad_origin}");
        }
        assert_eq!(f.request("GET",&path,token,Value::Null).await.1["status"],"pending","Denied origins must not consume the code");
        let (status,phone)=f.request("POST","/identity/mobile-pairing/claim","",json!({"code":code})).await;assert_eq!(status,200,"{phone}");let identity=f.p.identity(phone["token"].as_str().unwrap()).unwrap();assert_eq!(identity.account,f.p.identity(token).unwrap().account);assert_eq!(identity.profile,profile["id"].as_str().unwrap());assert_ne!(identity.account,f.p.identity(other).unwrap().account);
        assert_ne!(f.request("POST","/identity/mobile-pairing/claim","",json!({"code":code})).await.0,200);
    }
    for server in ["http://192.168.1.20:9445","https://192.168.1.20:9444","http://10.1.2.3:9444","http://8.8.8.8:9444","http://computer.tailnet.ts.net:9444"] {assert_ne!(f.request("POST","/identity/mobile-pairing",token,json!({"server":server})).await.0,200,"{server}");}
}

#[tokio::test]
async fn decisions_key_is_account_scoped_persistent_private_and_live() {
    let f=Fixture::new(false);
    let one=f.register("decisions-owner").await;let two=f.register("decisions-other").await;
    let token=one["token"].as_str().unwrap();let outsider=two["token"].as_str().unwrap();
    let profile=f.p.identity(token).unwrap().profile;let app=f.p.app(&profile).unwrap();
    let key="sk-synthetic-one-123456789";let replacement="sk-synthetic-two-123456789";
    let (status,_)=f.request("POST","/api/connections/decisions","",json!({"key":key})).await;assert_eq!(status,401);
    let (status,result)=f.request("POST","/api/connections/decisions",token,json!({"key":key})).await;assert_eq!(status,200,"{result}");assert_eq!(result["configured"],true);assert!(!result.to_string().contains(key));
    assert_eq!(crate::connections::decisions_key(&app).as_deref(),Some(key));
    let path=f.p.account_credential_path(&profile).unwrap();
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode()&0o777,0o600);}
    let (_,foreign)=f.request("GET","/api/connections/decisions",outsider,Value::Null).await;assert_eq!(foreign["configured"],false);assert!(!foreign.to_string().contains(key));
    let (status,_)=f.request("POST","/api/connections/decisions",token,json!({"key":""})).await;assert_eq!(status,400);assert_eq!(crate::connections::decisions_key(&app).as_deref(),Some(key));
    let (status,second)=f.request("POST","/identity/profiles",token,json!({"name":"Second"})).await;assert_eq!(status,200,"{second}");
    let (_,login)=f.request("POST","/identity/login","",json!({"login":"decisions-owner","password":"test password for profiles","profile_id":second["id"]})).await;
    let second_token=login["token"].as_str().unwrap();let second_app=f.p.app(&f.p.identity(second_token).unwrap().profile).unwrap();assert_eq!(crate::connections::decisions_key(&second_app).as_deref(),Some(key));
    let (status,_)=f.request("POST","/api/connections/decisions",second_token,json!({"key":replacement})).await;assert_eq!(status,200);assert_eq!(crate::connections::decisions_key(&app).as_deref(),Some(replacement));
    // Failure before atomic rename preserves the previous working bytes.
    let stored=std::fs::read(&path).unwrap();
    let broken=f.p.account_credential_path(&profile).unwrap();std::fs::rename(&broken,broken.with_extension("saved")).unwrap();std::fs::create_dir(&broken).unwrap();
    let (status,result)=f.request("POST","/api/connections/decisions",second_token,json!({"key":key})).await;assert_eq!(status,400);assert!(result["error"].as_str().unwrap().contains("previous key was retained"));assert!(!result.to_string().contains(key));
    std::fs::remove_dir(&broken).unwrap();std::fs::rename(broken.with_extension("saved"),&broken).unwrap();assert_eq!(std::fs::read(&broken).unwrap(),stored);
    drop(app);drop(second_app);f.p.apps.lock().unwrap().clear();
    let app=f.p.app(&profile).unwrap();assert_eq!(crate::connections::decisions_key(&app).as_deref(),Some(replacement));
    let (status,result)=f.request("DELETE","/api/connections/decisions",second_token,Value::Null).await;assert_eq!(status,200);assert_eq!(result["configured"],false);assert!(crate::connections::decisions_key(&app).is_none());
    assert!(!crate::browser_use::preferred(&app,&crate::tests::bot(&app.db,"codex")));
}

#[tokio::test]
async fn decisions_key_claimed_standalone_is_shared_only_with_its_account() {
    let f=Fixture::new(true);let legacy=f.p.legacy.as_ref().unwrap().clone();
    crate::connections::save_decisions(&legacy,Some("sk-synthetic-legacy-123456789")).unwrap();
    let (status,owner)=f.request("POST","/identity/register",&legacy.token,json!({"login":"legacy-decisions","name":"Owner","password":"test password for profiles","claim_legacy":true})).await;assert_eq!(status,200,"{owner}");
    let token=owner["token"].as_str().unwrap();let (status,second)=f.request("POST","/identity/profiles",token,json!({"name":"Second"})).await;assert_eq!(status,200);
    let second_app=f.p.app(second["id"].as_str().unwrap()).unwrap();assert!(crate::connections::decisions_key(&second_app).is_some());
    let other=f.register("legacy-decisions-other").await;let (_,status)=f.request("GET","/api/connections/decisions",other["token"].as_str().unwrap(),Value::Null).await;assert_eq!(status["configured"],false);
    crate::connections::save_decisions(&second_app,None).unwrap();assert!(crate::connections::decisions_key(&legacy).is_none());assert!(crate::connections::decisions_key(&second_app).is_none());
}
