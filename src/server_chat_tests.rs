use super::*;
use axum::body::to_bytes;

struct Fixture {
    p: Portal,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kindred-server-chats-{}", db::id()));
        let mut config = Config::default();
        config.profiles.enabled = true;
        config.profiles.directory = root.to_string_lossy().into_owned();
        config.database = root.join("legacy.db").to_string_lossy().into_owned();
        Self {
            p: Profiles::open(config, None, false).unwrap(),
            root,
        }
    }
    fn account(&self, name: &str) -> (String, Identity, Shared) {
        let account = db::id();
        let profile = db::id();
        let token = {
            let c = self.p.registry.lock().unwrap();
            c.execute("INSERT INTO accounts(id,login,salt,password,admin,created) VALUES(?,?,'fixture',?,0,?)",params![account,name,vec![0u8;32],db::now()]).unwrap();
            c.execute(
                "INSERT INTO profiles(id,account_id,name,created) VALUES(?,?,?,?)",
                params![profile, account, name, db::now()],
            )
            .unwrap();
            Profiles::session(&c, &account, &profile).unwrap()
        };
        let id = self.p.identity(&token).unwrap();
        let app = self.p.app(&profile).unwrap();
        (token, id, app)
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let response = router(self.p.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    async fn ok(&self, method: &str, path: &str, token: &str, body: Value) -> Value {
        let (status, value) = self.request(method, path, token, body).await;
        assert_eq!(status, 200, "{path}: {value}");
        value
    }
    fn sync(&self) {
        let _guard = self.p.shared_lock.lock().unwrap();
        sync(&self.p).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn bot(app: &Shared, name: &str) -> crate::db::Bot {
    let mut b = crate::tests::bot(&app.db, "codex");
    b.name = name.into();
    b.instructions = "PRIVATE INSTRUCTIONS NEVER RETURN FROM DIRECTORY".into();
    b.memory = "PRIVATE MEMORY".into();
    app.db.save_bot(&b).unwrap();
    b
}
fn bot_key(id: &Identity, b: &crate::db::Bot) -> String {
    format!("bot:{}:{}", id.profile, b.id)
}

#[tokio::test]
async fn human_chats_are_attributed_membership_scoped_and_persist_across_restart() {
    let f = Fixture::new();
    let (a, aid, _) = f.account("Alice");
    let (b, bid, _) = f.account("Owen Smith");
    let (x, _, _) = f.account("Outsider");
    let search = f
        .ok("GET", "/api/server-chats/directory?q=owen", &a, Value::Null)
        .await;
    assert_eq!(search["items"].as_array().unwrap().len(), 1);
    assert_eq!(search["items"][0]["kind"], "person");
    let room = f
        .ok(
            "POST",
            "/api/server-chats",
            &a,
            json!({"participants":[format!("person:{}",bid.account)]}),
        )
        .await;
    let path = format!("/api/server-chats/{}", room["id"].as_str().unwrap());
    let payload = json!({"prompt":"Hello Owen","request_id":db::id(),"mentions":[],"files":[]});
    f.ok("POST", &(path.clone() + "/messages"), &a, payload.clone())
        .await;
    f.ok("POST", &(path.clone() + "/messages"), &a, payload.clone())
        .await;
    let mut different = payload;
    different["prompt"] = json!("Different");
    assert_eq!(
        f.request("POST", &(path.clone() + "/messages"), &a, different)
            .await
            .0,
        400
    );
    let bob = f.ok("GET", &path, &b, Value::Null).await;
    assert_eq!(bob["messages"].as_array().unwrap().len(), 1);
    assert_eq!(bob["messages"][0]["mine"], false);
    assert_eq!(bob["messages"][0]["sender_name"], "Alice");
    assert_eq!(
        f.ok("GET", "/identity/profiles", &b, Value::Null).await["profiles"][0]["unread"],
        1
    );
    let seq = bob["messages"][0]["seq"].as_i64().unwrap();
    f.ok(
        "POST",
        &(path.clone() + "/messages"),
        &b,
        json!({"prompt":"Hello Alice","request_id":db::id(),"reply_to":seq}),
    )
    .await;
    let alice = f.ok("GET", &path, &a, Value::Null).await;
    assert_eq!(alice["messages"][0]["mine"], true);
    assert_eq!(alice["messages"][1]["reply_to"]["author"], "Alice");
    for suffix in ["", "/messages", "/read", "/pin", "/leave", "/delegate"] {
        let method = if suffix.is_empty() {
            "GET"
        } else if ["/messages", "/leave"].contains(&suffix) {
            "POST"
        } else {
            "PUT"
        };
        assert_ne!(
            f.request(
                method,
                &(path.clone() + suffix),
                &x,
                json!({"prompt":"intrude","request_id":db::id(),"cursor":seq})
            )
            .await
            .0,
            200
        );
    }
    assert_ne!(f.request("GET", &path, "", Value::Null).await.0, 200);
    assert_eq!(
        f.ok("GET", "/api/server-chats", &x, Value::Null).await,
        json!([])
    );
    f.ok(
        "PUT",
        &(path.clone() + "/read"),
        &b,
        json!({"cursor":i64::MAX}),
    )
    .await;
    let own = f.ok("GET", &path, &b, Value::Null).await;
    assert_eq!(own["chat"]["unread"], 0);
    assert_eq!(
        f.ok("GET", "/identity/profiles", &b, Value::Null).await["profiles"][0]["unread"],
        0
    );
    f.ok("PUT", &(path.clone() + "/pin"), &b, json!({"pinned":true}))
        .await;
    assert_eq!(
        f.ok("GET", &path, &a, Value::Null).await["chat"]["pinned"],
        false
    );
    f.p.apps.lock().unwrap().clear();
    let reopened = Profiles::open(f.p.config.clone(), None, false).unwrap();
    let r = allowed(
        &reopened.registry.lock().unwrap(),
        room["id"].as_str().unwrap(),
        &aid.account,
    )
    .unwrap();
    assert_eq!(r.participants.len(), 2);
    drop(reopened);
    f.ok("POST", &(path.clone() + "/leave"), &b, json!({}))
        .await;
    assert_ne!(f.request("GET", &path, &b, Value::Null).await.0, 200);
}

#[tokio::test]
async fn shared_bots_keep_owner_runtime_and_do_not_leak_private_state() {
    let f = Fixture::new();
    let (a, aid, aa) = f.account("Boss");
    let (b, bid, ba) = f.account("Tester");
    let (x, _, _) = f.account("Other");
    let pm = bot(&aa, "PM Bot");
    let tester = bot(&ba, "Tester Bot");
    let pmkey = bot_key(&aid, &pm);
    let testerkey = bot_key(&bid, &tester);
    let hidden = f
        .ok("GET", "/api/server-chats/directory?q=PM", &b, Value::Null)
        .await;
    assert!(hidden["items"].as_array().unwrap().is_empty());
    assert_eq!(
        f.request(
            "POST",
            "/api/server-chats",
            &b,
            json!({"name":"Forged","participants":[pmkey]})
        )
        .await
        .0,
        400
    );
    f.ok(
        "PUT",
        "/api/server-chats/sharing",
        &a,
        json!({"bot_id":pm.id,"shared":true}),
    )
    .await;
    let visible = f
        .ok("GET", "/api/server-chats/directory?q=PM", &b, Value::Null)
        .await;
    assert_eq!(visible["items"][0]["name"], "PM Bot");
    assert!(!visible.to_string().contains("PRIVATE"));
    assert!(visible["items"][0].get("instructions").is_none());
    let room = f
        .ok(
            "POST",
            "/api/server-chats",
            &b,
            json!({"name":"Project management","participants":[pmkey,testerkey]}),
        )
        .await;
    let key = room["id"].as_str().unwrap();
    let path = format!("/api/server-chats/{key}");
    assert_eq!(
        room["participants"].as_array().unwrap().len(),
        4,
        "Both bot owners are visible participants"
    );
    f.ok(
        "POST",
        &(path.clone() + "/messages"),
        &b,
        json!({"prompt":"Morning everyone","request_id":db::id()}),
    )
    .await;
    assert!(aa.db.runs(None).unwrap().is_empty());
    assert!(ba.db.runs(None).unwrap().is_empty());
    let payload = json!({"prompt":"PM Bot, please check the project","mentions":[pmkey],"request_id":db::id()});
    f.ok("POST", &(path.clone() + "/messages"), &b, payload.clone())
        .await;
    f.ok("POST", &(path.clone() + "/messages"), &b, payload)
        .await;
    f.sync();
    assert_eq!(aa.db.runs(None).unwrap().len(), 1);
    assert!(ba.db.runs(None).unwrap().is_empty());
    let run = aa.db.claim_bot(&pm.id).unwrap().unwrap();
    assert_eq!(run.chat_id, key);
    assert!(run.prompt.contains("never impersonate"));
    let instructions = crate::runtime::instructions(&aa, &pm, &run).unwrap();
    assert!(instructions.contains("Tester Bot"));
    aa.db
        .event(
            &run.id,
            "tool_call",
            json!({"secret":"NEVER EXPOSE TOOL LOGS"}),
        )
        .unwrap();
    aa.db
        .finish(&run.id, "completed", "Project is 85% complete.", "")
        .unwrap();
    aa.db.chat_complete(&aa.db.run(&run.id).unwrap()).unwrap();
    f.sync();
    f.sync();
    let history = f.ok("GET", &path, &b, Value::Null).await;
    let messages = history["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2]["sender"], pmkey);
    assert_eq!(messages[2]["text"], "Project is 85% complete.");
    assert!(!history.to_string().contains("NEVER EXPOSE"));
    assert!(!history.to_string().contains("PRIVATE"));
    assert!(
        history["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["run_id"] == "")
    );
    assert_eq!(
        f.request("GET", &format!("/api/chats/{key}"), &b, Value::Null)
            .await
            .0,
        404
    );
    assert!(
        f.ok("GET", "/api/chats", &b, Value::Null)
            .await
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["id"] != key)
    );
    assert_ne!(f.request("GET", &path, &x, Value::Null).await.0, 200);
    assert_ne!(
        f.request("PUT", &path, &a, json!({"name":"Not creator"}))
            .await
            .0,
        200
    );
    f.ok(
        "PUT",
        "/api/server-chats/sharing",
        &a,
        json!({"bot_id":pm.id,"shared":false}),
    )
    .await;
    assert!(!aa.db.chat(key).unwrap().members.contains(&pm.id));
    assert!(aa.db.bot_chat_read(&pm.id, key, 0, 10).is_err());
    assert_eq!(
        f.request(
            "POST",
            &(path.clone() + "/messages"),
            &b,
            json!({"prompt":"wake revoked bot","mentions":[pmkey],"request_id":db::id()})
        )
        .await
        .0,
        400
    );
}

#[tokio::test]
async fn bot_to_bot_and_answering_for_a_person_are_explicit_and_deduplicated() {
    let f = Fixture::new();
    let (a, aid, aa) = f.account("Boss");
    let (b, bid, ba) = f.account("Owen");
    let pm = bot(&aa, "PM Bot");
    let tester = bot(&ba, "Tester Bot");
    let pmkey = bot_key(&aid, &pm);
    let testerkey = bot_key(&bid, &tester);
    f.ok(
        "PUT",
        "/api/server-chats/sharing",
        &a,
        json!({"bot_id":pm.id,"shared":true}),
    )
    .await;
    let room = f
        .ok(
            "POST",
            "/api/server-chats",
            &b,
            json!({"name":"Project","participants":[pmkey,testerkey]}),
        )
        .await;
    let key = room["id"].as_str().unwrap();
    let path = format!("/api/server-chats/{key}");
    f.ok(
        "POST",
        &(path.clone() + "/messages"),
        &b,
        json!({"prompt":"PM Bot please check","request_id":db::id()}),
    )
    .await;
    let run = aa.db.claim_bot(&pm.id).unwrap().unwrap();
    aa.db.bot_chat_post(&pm,&run,&json!({"chat_id":key,"key":"first","message":"@Tester Bot please report","mentions":[testerkey]})).unwrap();
    f.sync();
    assert!(
        ba.db.runs(None).unwrap().is_empty(),
        "Automatic bot-to-bot replies default off"
    );
    f.ok("PUT", &path, &b, json!({"bot_to_bot":true})).await;
    assert_eq!(
        f.request(
            "PUT",
            &(path.clone() + "/delegate"),
            &a,
            json!({"bot":testerkey})
        )
        .await
        .0,
        400,
        "One person cannot opt another person's bot into representation"
    );
    f.ok(
        "PUT",
        &(path.clone() + "/delegate"),
        &b,
        json!({"bot":testerkey}),
    )
    .await;
    aa.db.bot_chat_post(&pm,&run,&json!({"chat_id":key,"key":"status","message":"@Owen where are we?","mentions":[format!("person:{}",bid.account)]})).unwrap();
    f.sync();
    f.sync();
    assert_eq!(ba.db.runs(None).unwrap().len(), 1);
    let next = ba.db.runs(None).unwrap().remove(0);
    assert!(next.prompt.contains("@Owen where are we?"));
    let visible = f.ok("GET", &path, &b, Value::Null).await;
    assert_eq!(visible["messages"].as_array().unwrap().len(), 3);
    let local = ba.db.bot_chat_read(&tester.id, key, 0, 30).unwrap();
    assert!(local.to_string().contains("Boss"));
    assert!(local.to_string().contains("PM Bot"));
    drop(aa);
    drop(ba);
    f.p.apps.lock().unwrap().clear();
    let reopened = Profiles::open(f.p.config.clone(), None, false).unwrap();
    {
        let _guard = reopened.shared_lock.lock().unwrap();
        sync(&reopened).unwrap();
    }
    assert_eq!(
        reopened
            .app(&bid.profile)
            .unwrap()
            .db
            .runs(None)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn shared_questions_continue_once_and_notifications_do_not_cross_membership() {
    let f = Fixture::new();
    let (a, aid, aa) = f.account("Boss");
    let (b, bid, _) = f.account("Owen");
    let (x, _, _) = f.account("Outsider");
    let pm = bot(&aa, "PM Bot");
    let pmkey = bot_key(&aid, &pm);
    let room = f
        .ok(
            "POST",
            "/api/server-chats",
            &a,
            json!({"name":"PM","participants":[pmkey,format!("person:{}",bid.account)]}),
        )
        .await;
    let key = room["id"].as_str().unwrap();
    let path = format!("/api/server-chats/{key}");
    let initial = f.ok("GET", "/api/notifications", &b, Value::Null).await;
    let cursor = initial["cursor"].as_i64().unwrap();
    f.ok("GET", "/api/notifications", &x, Value::Null).await;
    f.ok(
        "POST",
        &(path.clone() + "/messages"),
        &b,
        json!({"prompt":"PM Bot please check","request_id":db::id()}),
    )
    .await;
    let run = aa.db.claim_bot(&pm.id).unwrap().unwrap();
    aa.db
        .ask_question(
            &run,
            crate::questions::QuestionInput {
                topic_key: "status".into(),
                question: "What is blocking the project?".into(),
                context: "Please pick the current blocker.".into(),
                options: vec!["Client access".into(), "Nothing".into()],
            },
        )
        .unwrap();
    aa.db.finish(&run.id, "completed", "", "").unwrap();
    aa.db.chat_complete(&aa.db.run(&run.id).unwrap()).unwrap();
    f.sync();
    let history = f.ok("GET", &path, &b, Value::Null).await;
    assert!(history["messages"].as_array().unwrap().iter().all(|m|m["kind"]!="question"));
    let dm=format!("dm-{}",pm.id);
    let private=aa.db.chat_messages(&dm).unwrap();
    let question=private.iter().find(|m|m["kind"]=="question").unwrap()["question"].clone();
    assert_eq!(question["delivery_chat_id"],dm);
    assert_eq!(question["chat_id"],key);
    assert!(question["context"].as_str().unwrap().contains(key));
    let qid=question["id"].as_str().unwrap();
    let own=aa.db.notifications(Some(0)).unwrap();
    assert!(own["items"].as_array().unwrap().iter().any(|n|n["chat_id"]==dm));
    let answered=aa.db.answer_question(qid,crate::questions::Answer{selected:Some(0),custom:None}).unwrap();
    let repeated=aa.db.answer_question(qid,crate::questions::Answer{selected:Some(0),custom:None}).unwrap();
    assert_eq!(answered.continuation_run_id,repeated.continuation_run_id);
    assert_eq!(aa.db.run(&answered.continuation_run_id).unwrap().chat_id,dm);
    assert_eq!(aa.db.runs(None).unwrap().len(),2);
    assert!(aa.db.answer_question(qid,crate::questions::Answer{selected:Some(1),custom:None}).is_err());
    let notes=f.ok("GET",&format!("/api/notifications?after={cursor}"),&b,Value::Null).await;
    assert_eq!(notes["items"],json!([]),"Private owner questions must not notify other room members");
    assert_eq!(f.ok("GET","/api/notifications?after=0",&x,Value::Null).await["items"],json!([]));
    f.ok("POST", &(path.clone() + "/leave"), &b, json!({}))
        .await;
    assert!(
        f.ok("GET", "/api/notifications?after=0", &b, Value::Null)
            .await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn pagination_personal_archive_and_revocation_preserve_other_members() {
    let f = Fixture::new();
    let (a, aid, aa) = f.account("Alice");
    let (b, bid, _) = f.account("Bob");
    let pm = bot(&aa, "PM Bot");
    let pmkey = bot_key(&aid, &pm);
    let room = f
        .ok(
            "POST",
            "/api/server-chats",
            &a,
            json!({"name":"History","participants":[pmkey,format!("person:{}",bid.account)]}),
        )
        .await;
    let key = room["id"].as_str().unwrap();
    let path = format!("/api/server-chats/{key}");
    for index in 0..55 {
        f.ok(
            "POST",
            &(path.clone() + "/messages"),
            &a,
            json!({"prompt":format!("Progress note {index}"),"request_id":db::id()}),
        )
        .await;
    }
    let latest = f.ok("GET", &path, &b, Value::Null).await;
    assert_eq!(latest["messages"].as_array().unwrap().len(), 50);
    assert_eq!(latest["page"]["has_before"], true);
    let first = latest["messages"][0]["seq"].as_i64().unwrap();
    let older = f
        .ok("GET", &format!("{path}?before={first}"), &b, Value::Null)
        .await;
    assert_eq!(older["messages"].as_array().unwrap().len(), 5);
    let inclusive = f
        .ok(
            "GET",
            &format!("{path}?after={first}&inclusive=true&limit=2"),
            &b,
            Value::Null,
        )
        .await;
    assert_eq!(inclusive["messages"][0]["seq"], first);
    f.ok("PUT", &path, &b, json!({"archived":true})).await;
    assert_eq!(
        f.ok("GET", &path, &a, Value::Null).await["chat"]["archived"],
        false
    );
    f.ok(
        "POST",
        &(path.clone() + "/messages"),
        &a,
        json!({"prompt":"PM Bot please check","request_id":db::id()}),
    )
    .await;
    assert_eq!(aa.db.runs(None).unwrap()[0].status, "queued");
    f.ok(
        "PUT",
        &path,
        &a,
        json!({"participants":[format!("person:{}",bid.account)]}),
    )
    .await;
    assert_eq!(aa.db.runs(None).unwrap()[0].status, "cancelled");
    assert!(aa.db.bot_chat_read(&pm.id, key, 0, 10).is_err());
    assert!(
        f.ok("GET", &path, &b, Value::Null).await["messages"]
            .as_array()
            .unwrap()
            .len()
            > 0
    );
}

#[tokio::test]
async fn members_can_add_their_private_bot_without_sharing_it_server_wide() {
    let f = Fixture::new();
    let (a, aid, _) = f.account("Boss");
    let (b, bid, ba) = f.account("Tester");
    let (x, _, _) = f.account("Outsider");
    let tester = bot(&ba, "Private Tester");
    let botkey = bot_key(&bid, &tester);
    let created = f
        .ok(
            "POST",
            "/api/server-chats",
            &a,
            json!({"participants":[format!("person:{}",bid.account)]}),
        )
        .await;
    let key = created["id"].as_str().unwrap();
    let path = format!("/api/server-chats/{key}");
    assert_eq!(
        f.ok("GET", &path, &b, Value::Null).await["chat"]["name"],
        "Boss"
    );
    assert_eq!(
        f.ok(
            "POST",
            "/api/server-chats",
            &a,
            json!({"participants":[format!("person:{}",bid.account)]})
        )
        .await["id"],
        key
    );
    f.ok(
        "PUT",
        &(path.clone() + "/my-bots"),
        &b,
        json!({"bots":[botkey]}),
    )
    .await;
    assert_eq!(
        f.ok(
            "GET",
            "/api/server-chats/directory?q=Private",
            &x,
            Value::Null
        )
        .await["items"],
        json!([])
    );
    assert_eq!(
        f.request(
            "PUT",
            &(path.clone() + "/my-bots"),
            &a,
            json!({"bots":[botkey]})
        )
        .await
        .0,
        400
    );
    f.ok(
        "POST",
        &(path.clone() + "/messages"),
        &a,
        json!({"prompt":"Private Tester please check","mentions":[botkey],"request_id":db::id()}),
    )
    .await;
    let run = ba.db.claim_bot(&tester.id).unwrap().unwrap();
    ba.db
        .finish(
            &run.id,
            "failed",
            "",
            "PRIVATE CREDENTIAL IN PROVIDER ERROR",
        )
        .unwrap();
    ba.db.chat_complete(&ba.db.run(&run.id).unwrap()).unwrap();
    f.sync();
    let history = f.ok("GET", &path, &a, Value::Null).await;
    assert!(!history.to_string().contains("PRIVATE CREDENTIAL"));
    assert!(history.to_string().contains("could not finish"));
    assert_eq!(
        f.request(
            "GET",
            &format!("/api/bots/{}/avatar.png", tester.id),
            &a,
            Value::Null
        )
        .await
        .0,
        200
    );
    assert_ne!(
        f.request(
            "GET",
            &format!("/api/bots/{}/avatar.png", tester.id),
            &x,
            Value::Null
        )
        .await
        .0,
        200
    );
    f.ok("PUT", &(path.clone() + "/my-bots"), &b, json!({"bots":[]}))
        .await;
    assert!(ba.db.bot_chat_read(&tester.id, key, 0, 10).is_err());
    assert_eq!(
        f.ok("GET", &path, &a, Value::Null).await["chat"]["owner"],
        aid.account
    );
}

#[tokio::test]
async fn cross_profile_automatic_replies_stop_at_the_round_budget() {
    let f = Fixture::new();
    let (a, aid, aa) = f.account("Alice");
    let (b, bid, ba) = f.account("Bob");
    let one = bot(&aa, "Atlas");
    let two = bot(&ba, "Oliver");
    f.ok(
        "PUT",
        "/api/server-chats/sharing",
        &b,
        json!({"bot_id":two.id,"shared":true}),
    )
    .await;
    let r=f.ok("POST","/api/server-chats",&a,json!({"name":"Bounded replies","participants":[bot_key(&aid,&one),bot_key(&bid,&two)],"bot_to_bot":true})).await;
    let key = r["id"].as_str().unwrap();
    f.ok(
        "POST",
        &format!("/api/server-chats/{key}/messages"),
        &a,
        json!({"prompt":"Atlas please start","request_id":db::id()}),
    )
    .await;
    for _ in 0..30 {
        let next = aa
            .db
            .claim_bot(&one.id)
            .unwrap()
            .map(|run| (aa.clone(), run, "@Oliver please respond"))
            .or_else(|| {
                ba.db
                    .claim_bot(&two.id)
                    .unwrap()
                    .map(|run| (ba.clone(), run, "@Atlas please respond"))
            });
        let Some((app, run, text)) = next else {
            break;
        };
        app.db.finish(&run.id, "completed", text, "").unwrap();
        app.db.chat_complete(&app.db.run(&run.id).unwrap()).unwrap();
        f.sync();
        f.sync(); // A reply from the later profile is delivered on the next relay tick.
    }
    let runs = aa
        .db
        .runs(None)
        .unwrap()
        .into_iter()
        .chain(ba.db.runs(None).unwrap())
        .collect::<Vec<_>>();
    assert!(runs.len() > 2 && runs.len() <= 24, "{} tasks", runs.len());
    assert!(
        runs.iter()
            .all(|r| r.status == "completed" && r.depth <= 12)
    );
}

#[tokio::test]
async fn group_description_syncs_to_bots_and_only_owner_can_edit_it() {
    let f=Fixture::new();
    let (owner, identity, app)=f.account("Owner");
    let (member, other, _)=f.account("Member");
    let leader=bot(&app,"Piper");
    let room=f.ok("POST","/api/server-chats",&owner,json!({"name":"Team updates","description":"One line per update.","participants":[bot_key(&identity,&leader),format!("person:{}",other.account)]})).await;
    let id=room["id"].as_str().unwrap();
    assert_eq!(room["description"],"One line per update.");
    assert_eq!(app.db.chat(id).unwrap().description,"One line per update.");
    let path=format!("/api/server-chats/{id}");
    let (status,_)=f.request("PUT",&path,&member,json!({"description":"Changed by non-owner"})).await;
    assert_ne!(status,StatusCode::OK);
    f.ok("PUT",&path,&owner,json!({"description":"Piper coordinates. Do not contact clients."})).await;
    assert_eq!(app.db.bot_chat_read(&leader.id,id,0,10).unwrap()["chat"]["description"],"Piper coordinates. Do not contact clients.");
    let (status,_)=f.request("PUT",&path,&owner,json!({"description":"x".repeat(2001)})).await;
    assert_ne!(status,StatusCode::OK);
}

#[tokio::test]
async fn shared_files_are_scoped_bound_once_and_delivered_to_bot_profiles() {
    let f=Fixture::new();let (a,aid,aa)=f.account("Owner");let(b,bid,_)=f.account("Colleague");let(x,_,_)=f.account("Outsider");
    let pm=bot(&aa,"Reader");let member=bot_key(&aid,&pm);
    let room=f.ok("POST","/api/server-chats",&a,json!({"name":"Files","participants":[member,format!("person:{}",bid.account)]})).await;
    let key=room["id"].as_str().unwrap();let path=format!("/api/server-chats/{key}");
    let file=f.ok("POST","/api/server-uploads",&b,json!({"chat_id":key,"name":"notes.txt","data":"aGVsbG8="})).await;
    let file_id=file["id"].as_str().unwrap();
    assert_ne!(f.request("GET",&format!("/api/server-uploads/{file_id}"),&x,Value::Null).await.0,200);
    assert_ne!(f.request("GET",&format!("/api/server-uploads/{file_id}"),&a,Value::Null).await.0,200,"Unsent drafts are private to the uploader");
    let request=json!({"prompt":"Reader please read this file","files":[file_id],"request_id":db::id()});
    f.ok("POST",&(path.clone()+"/messages"),&b,request.clone()).await;
    f.ok("POST",&(path.clone()+"/messages"),&b,request).await;
    let history=f.ok("GET",&path,&a,Value::Null).await;
    assert_eq!(history["messages"][0]["files"][0]["name"],"notes.txt");
    assert_eq!(f.request("GET",&format!("/api/server-uploads/{file_id}"),&a,Value::Null).await.0,200);
    assert_ne!(f.request("GET",&format!("/api/server-uploads/{file_id}"),&x,Value::Null).await.0,200);
    let run=aa.db.claim_bot(&pm.id).unwrap().unwrap();
    assert_eq!(aa.db.run_upload(&run,file_id).unwrap().1,b"hello");
    assert_ne!(f.request("POST",&(path.clone()+"/messages"),&b,json!({"prompt":"Reader again","files":[file_id],"request_id":db::id()})).await.0,200);
    assert_ne!(f.request("POST","/api/server-uploads",&x,json!({"chat_id":key,"name":"notes.txt","data":"aGVsbG8="})).await.0,200);
}

#[tokio::test]
async fn group_identity_refresh_updates_legacy_titles_and_avatars_but_keeps_custom_names() {
    let f = Fixture::new();
    let (token, identity, app) = f.account("Alex");
    let mut alpha = bot(&app, "Alpha");
    let mut beta = bot(&app, "Beta");
    let members = json!([bot_key(&identity,&alpha),bot_key(&identity,&beta)]);
    let automatic = f.ok("POST", "/api/server-chats", &token, json!({"participants":members})).await;
    let custom = f.ok("POST", "/api/server-chats", &token, json!({"participants":members,"name":"Team General"})).await;
    // Simulate a room created before automatic title tracking existed.
    {
        let c = f.p.registry.lock().unwrap();
        let mut r = room(&c, automatic["id"].as_str().unwrap()).unwrap();
        r.automatic_name = None;
        c.execute("UPDATE server_rooms SET body=? WHERE id=?",params![serde_json::to_string(&r).unwrap(),r.id]).unwrap();
    }
    alpha.name = "Charlie".into(); beta.name = "Delta".into();
    for b in [&mut alpha, &mut beta] {
        b.profile.shape = "round".into(); b.profile.color = "#ff0000".into();
        app.db.save_bot(b).unwrap();
    }
    let chats = f.ok("GET", "/api/server-chats", &token, Value::Null).await;
    let updated = chats.as_array().unwrap().iter().find(|c|c["id"]==automatic["id"]).unwrap();
    assert_eq!(updated["name"],"Charlie, Delta");
    for m in updated["participants"].as_array().unwrap().iter().filter(|m|m["kind"]=="bot") {
        assert_eq!(m["avatar"]["shape"],"round");
        assert_eq!(m["avatar"]["color"],"#ff0000");
        assert!(m["name"]=="Charlie" || m["name"]=="Delta");
        assert!(!m.to_string().contains("PRIVATE"));
    }
    assert_eq!(chats.as_array().unwrap().iter().find(|c|c["id"]==custom["id"]).unwrap()["name"],"Team General");
    f.sync();
    let c=f.p.registry.lock().unwrap();
    assert_eq!(room(&c,automatic["id"].as_str().unwrap()).unwrap().name,"Charlie, Delta");
}

#[tokio::test]
async fn chat_edit_shared_owner_approval_membership_and_stale_protection(){
    let f=Fixture::new();
    let (token,owner,app)=f.account("Owner");
    let (_,other_id,other_app)=f.account("Guest");
    let mut actor=bot(&app,"Editor");actor.approval_mode="full".into();app.db.save_bot(&actor).unwrap();
    let peer=bot(&app,"Helper");let guest=bot(&other_app,"Guest bot");
    f.p.registry.lock().unwrap().execute("INSERT INTO server_bot_shares VALUES(?,?)",params![other_id.profile,guest.id]).unwrap();
    let created=f.ok("POST","/api/server-chats",&token,json!({"name":"Team","description":"Before","participants":[bot_key(&owner,&actor),bot_key(&owner,&peer),bot_key(&other_id,&guest)]})).await;
    let key=created["id"].as_str().unwrap();
    assert!(crate::chat_edit::get(&other_app,&guest,&json!({"chat_id":key})).is_err());
    let id=app.db.queue(&actor.id,"Edit group",0).unwrap();app.db.claim_bot(&actor.id).unwrap();let run=app.db.run(&id).unwrap();
    for scenario in ["stale","allow"]{
        let snapshot=crate::chat_edit::get(&app,&actor,&json!({"chat_id":key})).unwrap();
        let args=json!({"chat_id":key,"expected_revision":snapshot["revision"],"name":"team-updates","description":"Completed work only","members":[bot_key(&owner,&actor),format!("person:{}",owner.account)]});
        let a=app.clone();let b=actor.clone();let r=run.clone();
        let task=tokio::spawn(async move{crate::runtime::call_tool(&a,&b,&r,"chat_update",args).await.unwrap()});
        let approval=tokio::time::timeout(std::time::Duration::from_secs(4),async{
            loop{if let Some(row)=app.db.approvals().unwrap().first(){break row.clone();}tokio::time::sleep(std::time::Duration::from_millis(10)).await;}
        }).await.unwrap();
        assert_eq!(room(&f.p.registry.lock().unwrap(),key).unwrap().name,"Team");
        if scenario=="stale"{f.ok("PUT",&format!("/api/server-chats/{key}"),&token,json!({"description":"Human change"})).await;}
        app.db.decide(approval["id"].as_str().unwrap(),true).unwrap();
        let result=tokio::time::timeout(std::time::Duration::from_secs(5),task).await.unwrap().unwrap();
        let saved=room(&f.p.registry.lock().unwrap(),key).unwrap();
        if scenario=="stale"{assert_eq!(result["failed"],true);assert_eq!(saved.description,"Human change");}
        else{assert_ne!(result["failed"],true,"{result}");assert_eq!(saved.name,"team-updates");assert_eq!(saved.participants.len(),2);assert_eq!(app.db.chat(key).unwrap().description,"Completed work only");}
    }
}

#[tokio::test]
async fn general_room_messages_select_one_bot_but_broadcasts_and_quotes_keep_their_targets() {
    let f=Fixture::new();let (token,identity,app)=f.account("Owner");
    let lead=bot(&app,"Piper");let helper=bot(&app,"Atlas");
    let lead_key=bot_key(&identity,&lead);let helper_key=bot_key(&identity,&helper);
    let room=f.ok("POST","/api/server-chats",&token,json!({"name":"Team","description":"Piper coordinates","participants":[lead_key,helper_key],"all_messages":true,"bot_to_bot":true})).await;
    let path=format!("/api/server-chats/{}",room["id"].as_str().unwrap());
    f.ok("POST",&(path.clone()+"/messages"),&token,json!({"prompt":"Should be in a better place now","request_id":db::id()})).await;
    let runs=app.db.runs(None).unwrap();assert_eq!(runs.len(),1);assert_eq!(runs[0].bot_id,lead.id);
    assert!(app.db.recipient_selected(&runs[0].id).unwrap());assert!(app.db.group_activity_started(&runs[0].id).unwrap());
    f.ok("POST",&(path.clone()+"/messages"),&token,json!({"prompt":"Each of you give an update","request_id":db::id()})).await;
    assert_eq!(app.db.runs(None).unwrap().len(),3);
    let work=app.db.claim_bot(&helper.id).unwrap().unwrap();
    app.db.bot_chat_post(&helper,&work,&json!({"chat_id":room["id"],"key":"report","message":"The report is ready","mentions":[]})).unwrap();f.sync();
    let detail=f.ok("GET",&path,&token,Value::Null).await;
    let seq=detail["messages"].as_array().unwrap().iter().find(|m|m["text"]=="The report is ready").unwrap()["seq"].clone();
    f.ok("POST",&(path.clone()+"/messages"),&token,json!({"prompt":"Please shorten it","reply_to":seq,"request_id":db::id()})).await;
    let runs=app.db.runs(None).unwrap();assert_eq!(runs.len(),4);assert_eq!(runs.iter().find(|r|r.prompt.contains("Please shorten it")).unwrap().bot_id,helper.id);
    f.ok("POST",&(path.clone()+"/messages"),&token,json!({"prompt":"What did Casey ask for again @Atlas","request_id":db::id()})).await;
    let runs=app.db.runs(None).unwrap();assert_eq!(runs.len(),5);
    let tagged=runs.iter().find(|r|r.prompt.contains("What did Casey ask for again")).unwrap();
    assert_eq!(tagged.bot_id,helper.id);assert!(app.db.group_activity_started(&tagged.id).unwrap());
    assert!(crate::runtime::instructions(&app,&helper,tagged).unwrap().contains("Recipient selection is complete"));
    f.ok("POST",&(path.clone()+"/messages"),&token,json!({"prompt":"Don't worry about this further Atlas, lets move on.","request_id":db::id()})).await;
    let runs=app.db.runs(None).unwrap();assert_eq!(runs.len(),6);
    assert_eq!(runs.iter().find(|r|r.prompt.contains("Don't worry about this further Atlas")).unwrap().bot_id,helper.id);
    let detail=f.ok("GET",&path,&token,Value::Null).await;
    assert!(detail["workers"].as_array().unwrap().iter().all(|w|w["id"].is_string() && w["activity_started"]==true));
}
