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
    assert_eq!(app.db.setting("general").unwrap().unwrap(), saved);

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
    assert_eq!(app.db.setting("general").unwrap().unwrap(), preserved);
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
