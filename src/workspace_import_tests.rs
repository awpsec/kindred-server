use super::*;

// Exercise the real Pi SDK, advertised JSON schemas and runtime tool dispatch
// together. Only the model's network responses are deterministic fixtures.
#[tokio::test]
async fn real_sdk_converts_a_workspace_and_merges_an_origin_sync() {
    use axum::{Json, Router, extract::State, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[derive(Clone)]
    struct Scenario {
        id: String,
        version: &'static str,
        calls: Arc<AtomicUsize>,
    }
    async fn respond(
        State(s): State<Scenario>,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        let turn = s.calls.fetch_add(1, Ordering::SeqCst);
        let messages = body["messages"].to_string();
        let key = ".agents/skills/review/SKILL.md";
        let (name, args, expected) = match turn {
            0 => {
                assert!(body["tools"].to_string().contains("workspace_import_draft"));
                (
                    "workspace_import_read",
                    json!({"import_id":s.id}),
                    "Call workspace_import_read first",
                )
            }
            1 => (
                "workspace_import_read",
                json!({"import_id":s.id,"key":"AGENTS.md"}),
                "manifest",
            ),
            2 => (
                "workspace_import_read",
                json!({"import_id":s.id,"key":"src/CLAUDE.md"}),
                "Project guidance",
            ),
            3 => (
                "workspace_import_read",
                json!({"import_id":s.id,"key":key}),
                "Only applies to src",
            ),
            4 => (
                "workspace_import_read",
                json!({"import_id":s.id,"key":key,"file":"references/guide.md"}),
                "using references/guide.md",
            ),
            5 if s.version == "two" => (
                "workspace_import_read",
                json!({"import_id":s.id,"key":"AGENTS.md","version":"previous"}),
                "Keep supporting context",
            ),
            5 | 6 => {
                let mut p = proposal(&s.id, s.version);
                if s.version == "two" {
                    assert!(messages.contains("Local rule"));
                    assert!(messages.contains("Project guidance one"));
                    p["instructions"] = json!("Merged guidance two; preserve Local rule");
                }
                ("workspace_import_draft", p, "Keep supporting context")
            }
            7 => ("", Value::Null, "Nothing is installed yet"),
            _ => panic!("Unexpected model turn {turn}"),
        };
        assert!(
            messages.contains(expected),
            "turn {turn}: missing {expected}"
        );
        // Initial import finishes after the draft tool on turn five.
        let done = name.is_empty() || (s.version == "one" && turn == 6);
        let message = if done {
            assert!(messages.contains("Nothing is installed yet"));
            json!({"role":"assistant","content":"The conversion draft is ready for your review."})
        } else {
            json!({"role":"assistant","tool_calls":[{"id":format!("workspace_{turn}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
        };
        crate::pi::sse_response(
            json!({"choices":[{"message":message,"finish_reason":if done {"stop"} else {"tool_calls"}}]}),
        )
    }
    let app = crate::tests::app();
    let mut actor = crate::tests::bot(&app.db, "openrouter");
    for version in ["one", "two"] {
        let target = if version == "two" {
            actor.id.as_str()
        } else {
            ""
        };
        let (id, run) = started(&app.db, &actor, target, snapshot(version));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/chat/completions", post(respond))
                    .with_state(Scenario {
                        id: id.clone(),
                        version,
                        calls: calls.clone(),
                    }),
            )
            .into_future(),
        );
        let output = crate::pi::fixture(&app, &actor, &run, "fixture-key", &endpoint).await;
        server.abort();
        assert_eq!(
            output.unwrap(),
            "The conversion draft is ready for your review."
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            if version == "one" { 7 } else { 8 }
        );
        let review = get(&app.db, &id).unwrap();
        assert_eq!(review["state"], "ready");
        assert_eq!(
            app.db.bots().unwrap().len(),
            if version == "one" { 1 } else { 2 }
        );
        let result = apply(&app.db, &id, &json!({"revision":review["revision"]})).unwrap();
        app.db
            .finish(&run.id, "completed", "Review ready", "")
            .unwrap();
        actor = app.db.bot(result["bot_id"].as_str().unwrap()).unwrap();
        assert!(actor.memory.contains("Built from project"));
        if version == "one" {
            actor.instructions.push_str(" Local rule");
            app.db.save_bot(&actor).unwrap();
        } else {
            assert_eq!(
                actor.instructions,
                "Merged guidance two; preserve Local rule"
            );
            assert_eq!(app.db.skills().unwrap().len(), 1);
            assert!(
                skill_state(&app.db.0.lock().unwrap(), "Harold review").unwrap()["body"]
                    .as_str()
                    .unwrap()
                    .contains("Adapted review two")
            );
        }
    }
}

fn snapshot(version: &str) -> Value {
    json!({"root":"project","documents":[{"path":"AGENTS.md","text":format!("Project guidance {version}")},{"path":"src/CLAUDE.md","text":"Only applies to src"}],"packages":[{"source":".agents/skills/review/SKILL.md","entry":"SKILL.md","name":"review","files":{"SKILL.md":STANDARD.encode(format!("Review {version} using references/guide.md")),"references/guide.md":STANDARD.encode("Keep supporting context")}}],"warnings":[]})
}
fn proposal(id: &str, version: &str) -> Value {
    json!({"import_id":id,"name":"Harold","instructions":format!("Adapted guidance {version}; src rules remain scoped"),"memory":"The project prefers concise responses.","role":"Workspace assistant","description":"Imported workspace","workflows":[{"key":".agents/skills/review/SKILL.md","include":true,"name":"Harold review","command":"harold-review","description":"Review the project","body":format!("Adapted review {version}; read references/guide.md. $ARGUMENTS")}],"removals":[],"notes":"No source hooks were executed."})
}
fn started(db: &Db, actor: &Bot, target: &str, raw: Value) -> (String, Run) {
    let id = db::id();
    let v=start(db,&json!({"request_id":id,"bot_id":actor.id,"target_id":target,"name":"Harold","source":{"kind":"upload","path":"project","label":"project"},"snapshot":raw})).unwrap();
    assert_eq!(v["state"], "analyzing");
    let run = db.claim_bot(&actor.id).unwrap().unwrap();
    (id, run)
}
fn ready(db: &Db, actor: &Bot, target: &str, version: &str) -> (String, Run, Value) {
    let (id, run) = started(db, actor, target, snapshot(version));
    draft(db, actor, &run, &proposal(&id, version)).unwrap();
    let v = get(db, &id).unwrap();
    (id, run, v)
}
fn created(db: &Db, actor: &Bot) -> Bot {
    let (id, run, v) = ready(db, actor, "", "one");
    let result = apply(db, &id, &json!({"revision":v["revision"]})).unwrap();
    db.finish(&run.id, "completed", "Review ready", "").unwrap();
    db.bot(result["bot_id"].as_str().unwrap()).unwrap()
}
#[test]
fn create_is_atomic_idempotent_preserves_support_files_and_remembers_origin() {
    let app = crate::tests::app();
    let actor = crate::tests::bot(&app.db, "codex");
    let (id, run, v) = ready(&app.db, &actor, "", "one");
    assert_eq!(app.db.bots().unwrap().len(), 1);
    assert!(app.db.skills().unwrap().is_empty());
    let result = apply(&app.db, &id, &json!({"revision":v["revision"]})).unwrap();
    let bot = app.db.bot(result["bot_id"].as_str().unwrap()).unwrap();
    assert_eq!(bot.name, "Harold");
    assert!(bot.memory.contains("Built from project"));
    assert!(!bot.profile.local_access);
    assert_eq!(bot.approval_mode, "inherit");
    let state = skill_state(&app.db.0.lock().unwrap(), "Harold review").unwrap();
    let p: Value = serde_json::from_str(state["import_data"].as_str().unwrap()).unwrap();
    assert_eq!(
        STANDARD
            .decode(p["files"]["references/guide.md"].as_str().unwrap())
            .unwrap(),
        b"Keep supporting context"
    );
    assert!(
        String::from_utf8(
            STANDARD
                .decode(p["files"]["SKILL.md"].as_str().unwrap())
                .unwrap()
        )
        .unwrap()
        .starts_with("Adapted review")
    );
    let repeated = apply(&app.db, &id, &json!({"revision":v["revision"]})).unwrap();
    assert_eq!(result["bot_id"], repeated["bot_id"]);
    assert_eq!(app.db.bots().unwrap().len(), 2);
    assert_eq!(app.db.skills().unwrap().len(), 1);
    app.db
        .save_bot_text(&bot.id, "memory", "Completely new memory", None)
        .unwrap();
    app.db
        .save_bot_text(&bot.id, "instructions", "Completely new instructions", None)
        .unwrap();
    assert!(
        origin_instructions(&app.db, &bot)
            .unwrap()
            .contains("project")
    );
    assert!(
        !origin_instructions(&app.db, &bot)
            .unwrap()
            .contains("Adapted review")
    );
    let messages = app.db.chat_messages(&run.chat_id).unwrap();
    assert!(messages.iter().any(|m| m["workspace_import"]["id"] == id));
    assert!(
        !serde_json::to_string(&messages)
            .unwrap()
            .contains("Keep supporting context")
    );
}
#[test]
fn collisions_roll_back_bot_and_origin_and_allow_reviewed_rename() {
    let app = crate::tests::app();
    let actor = crate::tests::bot(&app.db, "codex");
    let (id, _, v) = ready(&app.db, &actor, "", "one");
    app.db
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO skills(name,body,command) VALUES('Existing','User-owned','harold-review')",
            [],
        )
        .unwrap();
    assert!(
        apply(&app.db, &id, &json!({"revision":v["revision"]}))
            .unwrap_err()
            .to_string()
            .contains("already used")
    );
    assert_eq!(app.db.bots().unwrap().len(), 1);
    assert_eq!(
        app.db
            .0
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM workspace_origins", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let mut edited = v["draft"].clone();
    edited["workflows"][0]["command"] = json!("harold-review-two");
    apply(
        &app.db,
        &id,
        &json!({"revision":v["revision"],"draft":edited}),
    )
    .unwrap();
    assert_eq!(app.db.skills().unwrap().len(), 2);
    assert_eq!(
        skill_state(&app.db.0.lock().unwrap(), "Existing").unwrap()["body"],
        "User-owned"
    );
}
#[tokio::test]
async fn sync_exposes_three_versions_and_preserves_local_edits_after_merge() {
    let app = crate::tests::app();
    let actor = crate::tests::bot(&app.db, "codex");
    let mut bot = created(&app.db, &actor);
    bot.instructions.push_str(" Local rule");
    app.db.save_bot(&bot).unwrap();
    app.db
        .0
        .lock()
        .unwrap()
        .execute("UPDATE skills SET body=body||' Local workflow rule'", [])
        .unwrap();
    let (id, run) = started(&app.db, &bot, &bot.id, snapshot("two"));
    let overview = read(&app, &bot, &run, &json!({"import_id":id}))
        .await
        .unwrap();
    assert!(overview.to_string().contains("Local rule"));
    assert!(!overview.to_string().contains("import_data"));
    let entry = read(
        &app,
        &bot,
        &run,
        &json!({"import_id":id,"key":".agents/skills/review/SKILL.md"}),
    )
    .await
    .unwrap()
    .to_string();
    assert!(entry.contains("Review two"));
    assert!(entry.contains("Adapted review one"));
    assert!(entry.contains("Local workflow rule"));
    let support=read(&app,&bot,&run,&json!({"import_id":id,"key":".agents/skills/review/SKILL.md","file":"references/guide.md"})).await.unwrap();
    assert!(support.to_string().contains("Keep supporting context"));
    let previous = read(
        &app,
        &bot,
        &run,
        &json!({"import_id":id,"key":"AGENTS.md","version":"previous"}),
    )
    .await
    .unwrap();
    assert!(previous.to_string().contains("guidance one"));
    let mut p = proposal(&id, "two");
    p["instructions"] = json!("Merged guidance two with Local rule");
    p["workflows"][0]["body"] = json!("Merged workflow two with Local workflow rule");
    draft(&app.db, &bot, &run, &p).unwrap();
    let v = get(&app.db, &id).unwrap();
    assert!(v["conflicts"].as_array().unwrap().is_empty());
    apply(&app.db, &id, &json!({"revision":v["revision"]})).unwrap();
    let updated = app.db.bot(&bot.id).unwrap();
    assert_eq!(updated.instructions, "Merged guidance two with Local rule");
    assert_eq!(updated.provider, bot.provider);
    assert_eq!(updated.profile.local_access, bot.profile.local_access);
    assert_eq!(app.db.bots().unwrap().len(), 2);
    assert_eq!(app.db.skills().unwrap().len(), 1);
}
#[test]
fn stale_instruction_memory_skill_or_origin_changes_never_overwrite() {
    for field in ["instructions", "memory", "skill", "origin"] {
        let app = crate::tests::app();
        let actor = crate::tests::bot(&app.db, "codex");
        let bot = created(&app.db, &actor);
        let (id, _, v) = ready(&app.db, &bot, &bot.id, "two");
        match field {
            "instructions" | "memory" => app
                .db
                .save_bot_text(&bot.id, field, "Newer user edit", None)
                .unwrap(),
            "skill" => {
                app.db
                    .0
                    .lock()
                    .unwrap()
                    .execute("UPDATE skills SET body='Newer workflow'", [])
                    .unwrap();
            }
            _ => {
                app.db
                    .0
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE workspace_origins SET data=json_set(data,'$.synced_at',0)",
                        [],
                    )
                    .unwrap();
            }
        }
        assert!(
            apply(&app.db, &id, &json!({"revision":v["revision"]})).is_err(),
            "{field}"
        );
        assert_eq!(get(&app.db, &id).unwrap()["state"], "ready");
        assert_eq!(app.db.bots().unwrap().len(), 2);
    }
}
#[test]
fn source_deletions_are_retained_unless_explicitly_removed() {
    for remove in [false, true] {
        let app = crate::tests::app();
        let actor = crate::tests::bot(&app.db, "codex");
        let bot = created(&app.db, &actor);
        let mut raw = snapshot("two");
        raw["packages"] = json!([]);
        let (id, run) = started(&app.db, &bot, &bot.id, raw);
        let mut p = proposal(&id, "two");
        p["workflows"] = json!([]);
        if remove {
            p["removals"] = json!([".agents/skills/review/SKILL.md"]);
        }
        draft(&app.db, &bot, &run, &p).unwrap();
        let v = get(&app.db, &id).unwrap();
        apply(&app.db, &id, &json!({"revision":v["revision"]})).unwrap();
        assert_eq!(app.db.skills().unwrap().len(), if remove { 0 } else { 1 });
    }
}
#[test]
fn drafts_are_task_bound_revision_guarded_and_restartable() {
    let app = crate::tests::app();
    let actor = crate::tests::bot(&app.db, "codex");
    let (id, run, v) = ready(&app.db, &actor, "", "one");
    let other = crate::tests::bot(&app.db, "codex");
    let other_id = app.db.queue(&other.id, "Import", 0).unwrap();
    app.db.claim_bot(&other.id).unwrap();
    let other_run = app.db.run(&other_id).unwrap();
    assert!(draft(&app.db, &other, &other_run, &proposal(&id, "evil")).is_err());
    assert!(retry(&app.db, &id).is_err());
    draft(&app.db, &actor, &run, &proposal(&id, "two")).unwrap();
    assert!(apply(&app.db, &id, &json!({"revision":v["revision"]})).is_err());
    cancel(&app.db, &id).unwrap();
    assert!(draft(&app.db, &actor, &run, &proposal(&id, "one")).is_err());
    assert!(apply(&app.db, &id, &json!({"revision":v["revision"]})).is_err());
    app.db.finish(&run.id, "cancelled", "", "").unwrap();
    let retried = retry(&app.db, &id).unwrap();
    assert_ne!(retried["run_id"], run.id);
    assert_eq!(retried["state"], "analyzing");
}
#[tokio::test]
async fn self_sync_requires_exact_review_in_full_access_and_handles_decline_cancel_stale() {
    for scenario in ["allow", "deny", "cancel", "stale", "redraft"] {
        let app = crate::tests::app();
        let actor = crate::tests::bot(&app.db, "codex");
        let mut bot = created(&app.db, &actor);
        bot.approval_mode = "full".into();
        app.db.save_bot(&bot).unwrap();
        let (id, run, _) = ready(&app.db, &bot, &bot.id, "two");
        let task = {
            let (a, b, r, id) = (app.clone(), bot.clone(), run.clone(), id.clone());
            tokio::spawn(async move {
                runtime::call_tool(&a, &b, &r, "workspace_sync_apply", json!({"import_id":id}))
                    .await
                    .unwrap()
            })
        };
        let approval = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Some(a) = app.db.approvals().unwrap().first() {
                    break a.clone();
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(approval["tool"], "workspace_sync_apply");
        assert!(
            retry(&app.db, &id).is_err(),
            "Preparing again must not strand an active review"
        );
        assert!(!task.is_finished());
        assert_eq!(app.db.bot(&bot.id).unwrap().instructions, bot.instructions);
        if scenario == "stale" {
            app.db
                .save_bot_text(&bot.id, "instructions", "Newer change", None)
                .unwrap();
        }
        if scenario == "redraft" {
            app.db.0.lock().unwrap().execute("UPDATE workspace_imports SET data=json_set(data,'$.revision','changed') WHERE id=?",[&id]).unwrap();
        }
        if scenario == "cancel" {
            app.db.cancel(&run.id).unwrap();
        } else {
            app.db
                .decide(approval["id"].as_str().unwrap(), scenario != "deny")
                .unwrap();
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result["failed"] == true,
            scenario != "allow",
            "{scenario}: {result}"
        );
        let updated = app.db.bot(&bot.id).unwrap();
        assert_eq!(
            updated.instructions,
            if scenario == "allow" {
                "Adapted guidance two; src rules remain scoped"
            } else if scenario == "stale" {
                "Newer change"
            } else {
                &bot.instructions
            }
        );
    }
}
#[test]
fn source_validation_limits_and_every_workflow_must_be_accounted_for() {
    let mut raw = snapshot("one");
    raw["documents"][0]["path"] = json!("../AGENTS.md");
    assert!(normalize_snapshot(&raw).is_err());
    raw = snapshot("one");
    raw["documents"][0]["text"] = json!("a".repeat(65537));
    assert!(normalize_snapshot(&raw).is_err());
    let app = crate::tests::app();
    let actor = crate::tests::bot(&app.db, "codex");
    let (id, run) = started(&app.db, &actor, "", snapshot("one"));
    let mut p = proposal(&id, "one");
    p["workflows"] = json!([]);
    assert!(draft(&app.db, &actor, &run, &p).is_err());
    p["workflows"] = json!([{"key":".agents/skills/review/SKILL.md","include":false}]);
    draft(&app.db, &actor, &run, &p).unwrap();
    let v = get(&app.db, &id).unwrap();
    apply(&app.db, &id, &json!({"revision":v["revision"]})).unwrap();
    assert!(app.db.skills().unwrap().is_empty());
}
#[tokio::test]
async fn routes_require_auth_and_large_draft_has_reviewable_response() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    let app = crate::tests::app();
    let router = crate::web::router(app.clone());
    for endpoint in [
        "/api/workspace-imports",
        "/api/workspace-imports/nope",
        "/api/bots/nope/workspace-origin",
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(endpoint)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
    }
    let actor = crate::tests::bot(&app.db, "codex");
    let (id, _, v) = ready(&app.db, &actor, "", "one");
    let mut draft = v["draft"].clone();
    draft["memory"] = json!("x".repeat(14000));
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/workspace-imports/{id}/apply"))
                .header("authorization", format!("Bearer {}", app.token))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"revision":v["revision"],"draft":draft}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let response = router
        .oneshot(
            Request::builder()
                .uri("/workspace-import.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[test]
fn origins_and_pending_reviews_survive_profile_transfer() {
    let app = crate::tests::app();
    let actor = crate::tests::bot(&app.db, "codex");
    let bot = created(&app.db, &actor);
    let (id, run, _) = ready(&app.db, &bot, &bot.id, "two");
    app.db
        .finish(&run.id, "completed", "Review is ready", "")
        .unwrap();
    let package = app
        .db
        .prepare_transfer(&db::id(), "Kindred source")
        .unwrap();
    let destination = Db::open(":memory:").unwrap();
    destination.import_transfer(&package).unwrap();
    assert_eq!(
        origin(&app.db, &bot.id).unwrap(),
        origin(&destination, &bot.id).unwrap()
    );
    assert_eq!(get(&destination, &id).unwrap()["state"], "ready");
    assert_eq!(get(&destination, &id).unwrap()["conflicts"], json!([]));
    assert!(!destination.bot(&bot.id).unwrap().profile.local_access);
}
#[test]
fn start_retries_are_idempotent_and_snapshot_order_does_not_change_fingerprint() {
    let app = crate::tests::app();
    let actor = crate::tests::bot(&app.db, "codex");
    let request = json!({"request_id":db::id(),"bot_id":actor.id,"name":"Harold","source":{"kind":"upload","path":"project"},"snapshot":snapshot("one")});
    let first = start(&app.db, &request).unwrap();
    let second = start(&app.db, &request).unwrap();
    assert_eq!(first["run_id"], second["run_id"]);
    let mut changed = request.clone();
    changed["name"] = json!("Different");
    assert!(start(&app.db, &changed).is_err());
    let raw = snapshot("one");
    let mut reordered = raw.clone();
    reordered["documents"].as_array_mut().unwrap().reverse();
    assert_eq!(
        normalize_snapshot(&raw).unwrap()["hash"],
        normalize_snapshot(&reordered).unwrap()["hash"]
    );
}

#[test]
fn sync_cannot_adopt_an_unrelated_skill_that_reused_an_old_name() {
    let app = crate::tests::app();
    let actor = crate::tests::bot(&app.db, "codex");
    let bot = created(&app.db, &actor);
    app.db
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE skills SET body='Unrelated replacement',import_data='null'",
            [],
        )
        .unwrap();
    let (id, _, v) = ready(&app.db, &bot, &bot.id, "two");
    assert!(apply(&app.db, &id, &json!({"revision":v["revision"]})).is_err());
    let mut edited = v["draft"].clone();
    edited["workflows"][0]["name"] = json!("Harold new review");
    edited["workflows"][0]["command"] = json!("harold-new-review");
    apply(
        &app.db,
        &id,
        &json!({"revision":v["revision"],"draft":edited}),
    )
    .unwrap();
    assert_eq!(
        skill_state(&app.db.0.lock().unwrap(), "Harold review").unwrap()["body"],
        "Unrelated replacement"
    );
    assert_eq!(app.db.skills().unwrap().len(), 2);
}

#[test]
fn full_256_workflow_import_catalog_and_origin_sync_preserve_every_entry() {
    let app = crate::tests::app();
    let actor = crate::tests::bot(&app.db, "codex");
    let mut raw = snapshot("one");
    let packages: Vec<_> = (0..256).map(|i| {
        let folder = [".claude/commands", ".codex/prompts", ".pi/prompts", "prompts"][i % 4];
        json!({"source":format!("{folder}/task-{i}.md"),"entry":format!("task-{i}.md"),"name":format!("task-{i}"),"files":{format!("task-{i}.md"):STANDARD.encode("Review the selected input")}})
    }).collect();
    raw["packages"] = json!(packages);
    let (id, run) = started(&app.db, &actor, "", raw.clone());
    let workflows: Vec<_> = packages.iter().enumerate().map(|(i,p)| json!({"key":p["source"],"include":true,"name":format!("Harold task {i}"),"command":format!("harold-task-{i}"),"description":"Imported task","body":"Review supplied input"})).collect();
    let mut proposed = proposal(&id, "one");
    proposed["workflows"] = json!(workflows);
    draft(&app.db, &actor, &run, &proposed).unwrap();
    let v = get(&app.db, &id).unwrap();
    assert!(v["conflicts"].as_array().unwrap().is_empty());
    let applied = apply(&app.db, &id, &json!({"revision":v["revision"]})).unwrap();
    let bot = app.db.bot(applied["bot_id"].as_str().unwrap()).unwrap();
    app.db.finish(&run.id, "completed", "Created", "").unwrap();
    assert_eq!(app.db.skills().unwrap().len(), 256);
    assert!(
        app.db
            .commands()
            .unwrap()
            .iter()
            .any(|c| c.name == "harold-task-255")
    );
    assert!(
        app.db
            .save_skill(&json!({"name":"extra","body":"Overflow"}))
            .unwrap_err()
            .to_string()
            .contains("256")
    );
    raw["packages"][0]["files"]["task-0.md"] = json!(STANDARD.encode("Newest source instructions"));
    let (sync_id, sync_run) = started(&app.db, &bot, &bot.id, raw.clone());
    proposed["import_id"] = json!(sync_id);
    proposed["workflows"][0]["body"] = json!("Merged newest instructions");
    draft(&app.db, &bot, &sync_run, &proposed).unwrap();
    let sync = get(&app.db, &sync_id).unwrap();
    apply(&app.db, &sync_id, &json!({"revision":sync["revision"]})).unwrap();
    let skills = app.db.skills().unwrap();
    assert_eq!(skills.len(), 256);
    assert_eq!(
        skills
            .iter()
            .find(|s| s["command"] == "harold-task-0")
            .unwrap()["body"],
        "Merged newest instructions"
    );
    assert_eq!(
        origin(&app.db, &bot.id).unwrap()["workflows"]
            .as_object()
            .unwrap()
            .len(),
        256
    );
    raw["packages"]
        .as_array_mut()
        .unwrap()
        .push(packages[0].clone());
    assert!(
        normalize_snapshot(&raw)
            .unwrap_err()
            .to_string()
            .contains("256")
    );
}

#[test]
fn workspace_converter_receives_large_source_skills_before_condensing() {
    let mut raw = snapshot("one");
    raw["packages"][0]["files"]["SKILL.md"] =
        json!(STANDARD.encode("Full source instructions. ".repeat(3200)));
    let v = normalize_snapshot(&raw).unwrap();
    assert_eq!(v["workflows"].as_object().unwrap().len(), 1);
    assert!(
        v["workflows"][".agents/skills/review/SKILL.md"]["body"]
            .as_str()
            .unwrap()
            .len()
            > 72000
    );
}
