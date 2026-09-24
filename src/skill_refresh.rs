//! Source-bound refresh of imported workflows; reads packages but never executes them.
use crate::{
    db::{self, Bot, Db, Run},
    runtime::App,
    skill_import,
};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde_json::{Value, json};

pub fn catalog(db: &Db) -> Result<Value> {
    Ok(
        json!({"workflows":db.skills()?.into_iter().filter(|s|s["import"].is_object()).map(|s|json!({"name":s["name"],"command":s["command"],"import":s["import"]})).collect::<Vec<_>>(),
        "guidance":"Refresh existing linked workflows from their saved source. Unlinked uploads or older imports need a verified source: preview/import the same unchanged package on the intended desktop to link it. Never infer the originating computer from its filename. Missing source files do not delete installed workflows."}),
    )
}

pub(crate) fn review(
    db: &Db,
    name: &str,
    raw: &Value,
    origin: &Value,
    args: &Value,
) -> Result<Value> {
    let mut incoming = skill_import::parse(raw)?;
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    let old = skill_import::load(&tx, name)?;
    ensure!(old.is_object(), "Imported workflow no longer exists");
    ensure!(
        old["origin"].is_object() && old["origin"] == *origin,
        "The workflow source changed while reading it. Review again"
    );
    let (body, command, description, parameters): (String, String, String, String) = tx.query_row(
        "SELECT body,command,description,parameters FROM skills WHERE name=?",
        [name],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let source_changed = incoming["hash"] != old["source_hash"];
    let locally_changed = old["hash"] != old["source_hash"];
    let conflict = source_changed && locally_changed && incoming["hash"] != old["hash"];
    let added: Vec<_> = incoming["files"]
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| old["files"].get(*k).is_none())
        .cloned()
        .collect();
    let removed: Vec<_> = old["files"]
        .as_object()
        .context("Missing saved package")?
        .keys()
        .filter(|k| incoming["files"].get(*k).is_none())
        .cloned()
        .collect();
    let changed: Vec<_> = incoming["files"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(k, v)| old["files"].get(*k).is_some_and(|previous| previous != *v))
        .map(|(k, _)| k.clone())
        .collect();
    let state = if !source_changed {
        if locally_changed {
            "local_changes_only"
        } else {
            "unchanged"
        }
    } else if conflict {
        "conflict"
    } else {
        "update_available"
    };
    let mut result = json!({"name":name,"command":command,"status":state,"origin":origin,"expected_source_hash":incoming["hash"],"expected_current_hash":old["hash"],"source_changed":source_changed,"kindred_changed":locally_changed,"files":{"added":added,"removed":removed,"changed":changed},"body":incoming["body"],"warnings":incoming["warnings"]});
    if conflict {
        result["kindred_body"] = json!(body);
    }
    if args["action"] == "preview" {
        return Ok(result);
    }
    ensure!(args["action"] == "update", "Choose list, preview or update");
    ensure!(
        args["expected_source_hash"] == incoming["hash"],
        "Source changed since preview. Preview again"
    );
    // A completed retry is a no-op. Otherwise compare the installed version too.
    if incoming["hash"] == old["hash"] && incoming["hash"] == old["source_hash"] {
        result["status"] = json!("unchanged");
        return Ok(result);
    }
    ensure!(
        args["expected_current_hash"] == old["hash"],
        "Kindred changed since preview. Preview again"
    );
    if !source_changed {
        return Ok(result);
    }
    ensure!(
        args["conflict_policy"].is_null()
            || args["conflict_policy"] == "preserve"
            || args["conflict_policy"] == "use_source",
        "Unknown conflict policy"
    );
    if conflict && args["conflict_policy"] != "use_source" {
        return Ok(result);
    }
    incoming["name"] = json!(name);
    incoming["command"] = json!(command);
    incoming["origin"] = origin.clone();
    incoming["source_hash"] = incoming["hash"].clone();
    incoming["source_description"] = incoming["description"].clone();
    incoming["edited"] = json!(false);
    if let Some(index) = old.get("argument_index") {
        incoming["argument_index"] = index.clone();
    }
    incoming["refreshed_at"] = json!(db::now());
    if old["source_description"].as_str() != Some(description.as_str()) {
        incoming["description"] = json!(description);
    }
    tx.execute(
        "UPDATE skills SET body=?,description=?,import_data=? WHERE name=?",
        params![
            incoming["body"].as_str().unwrap(),
            incoming["description"].as_str().unwrap(),
            incoming.to_string(),
            name
        ],
    )?;
    // Command/parameter customization and frozen invocation receipts are untouched.
    let _ = parameters;
    tx.commit()?;
    result["status"] = json!("updated");
    Ok(result)
}

pub async fn local(app: &App, bot: &Bot, run: &Run, args: &Value) -> Result<Value> {
    if args["action"] == "list" {
        return Ok(json!({"text":catalog(&app.db)?.to_string()}));
    }
    ensure!(
        args["action"] == "preview" || args["action"] == "update",
        "Choose list, preview or update"
    );
    let name = args["name"]
        .as_str()
        .context("Choose an imported workflow name")?;
    let old = skill_import::load(&app.db.0.lock().unwrap(), name)?;
    ensure!(old.is_object(), "Imported workflow not found");
    let origin = &old["origin"];
    ensure!(
        origin["kind"] == "local_desktop"
            && origin["device_id"].is_string()
            && origin["path"].is_string(),
        "This import has no verified desktop source. Confirm the source computer and path, then preview/import the unchanged original to link it. Its current copy is preserved."
    );
    let package = crate::local_access::call(
        app,
        bot,
        run,
        "local_skill_bundle",
        json!({"device_id":origin["device_id"],"path":origin["path"]}),
    )
    .await?;
    ensure!(
        package["failed"] != true,
        "Source could not be read; the installed workflow is unchanged: {}",
        package["text"].as_str().unwrap_or("Local read failed")
    );
    ensure!(
        package["kindred_device_id"] == origin["device_id"] && package["source"] == origin["path"],
        "Source identity did not match; installed workflow is unchanged"
    );
    Ok(json!({"text":review(&app.db,name,&package,origin,args)?.to_string()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    fn package(body: &str, helper: &str) -> Value {
        json!({"name":"review","entry":"SKILL.md","source":"/fixture/project/.claude/skills/review/SKILL.md","files":{"SKILL.md":STANDARD.encode(body),"scripts/check.py":STANDARD.encode(helper)}})
    }
    fn origin(p: &Value) -> Value {
        json!({"kind":"local_desktop","device_id":db::id(),"path":p["source"]})
    }
    fn install(db: &Db, p: &Value, o: &Value) {
        let v = skill_import::request_with_origin(
            db,
            &json!({"action":"preview","package":p,"command":"our-review"}),
            Some(o),
        )
        .unwrap();
        skill_import::request_with_origin(db,&json!({"action":"import","package":p,"command":"our-review","expected_hash":v["import"]["hash"]}),Some(o)).unwrap();
    }
    fn apply(v: &Value) -> Value {
        json!({"action":"update","expected_source_hash":v["expected_source_hash"],"expected_current_hash":v["expected_current_hash"]})
    }

    #[test]
    fn refresh_survives_restart_preserves_alias_settings_and_frozen_invocations() {
        let root = std::env::temp_dir().join(db::id());
        let path = root.join("data.db").to_string_lossy().into_owned();
        let db = Db::open(&path).unwrap();
        let source = root.join("project/.claude/skills/review");
        std::fs::create_dir_all(source.join("scripts")).unwrap();
        std::fs::write(source.join("SKILL.md"), "Review original input").unwrap();
        std::fs::write(
            source.join("scripts/check.py"),
            "raise Exception('never run')",
        )
        .unwrap();
        let p = crate::skill_files::bundle(&source.join("SKILL.md")).unwrap();
        let o = origin(&p);
        install(&db, &p, &o);
        let bot = crate::tests::bot(&db, "codex");
        db.save_chat(&crate::chats::Chat {
            bot_only: false,
            description: String::new(),
            id: format!("dm-{}", bot.id),
            name: bot.name.clone(),
            members: vec![bot.id.clone()],
            archived: false,
            pinned: false,
            last_message: None,
        })
        .unwrap();
        let ids = db
            .chat_send(&format!("dm-{}", bot.id), "/our-review input", &[])
            .unwrap();
        let snapshot: String =
            db.0.lock()
                .unwrap()
                .query_row(
                    "SELECT receipt FROM run_commands WHERE run_id=?",
                    [&ids[0]],
                    |r| r.get(0),
                )
                .unwrap();
        db.save_skill(&json!({"name":"review","body":"Review original input","command":"my-review","description":"My description","argument_index":1,"parameters":[{"name":"input","required":false}]})).unwrap();
        drop(db);
        let db = Db::open(&path).unwrap();
        assert_eq!(catalog(&db).unwrap()["workflows"][0]["import"]["origin"], o);
        std::fs::write(source.join("SKILL.md"), "Review improved input").unwrap();
        std::fs::write(
            source.join("scripts/check.py"),
            "raise Exception('still never run')",
        )
        .unwrap();
        std::fs::create_dir_all(source.join("references")).unwrap();
        std::fs::write(source.join("references/new.md"), "New guidance").unwrap();
        let fresh = crate::skill_files::bundle(&source.join("SKILL.md")).unwrap();
        let v = review(&db, "review", &fresh, &o, &json!({"action":"preview"})).unwrap();
        assert_eq!(v["status"], "update_available");
        assert_eq!(
            review(&db, "review", &fresh, &o, &apply(&v)).unwrap()["status"],
            "updated"
        );
        assert_eq!(
            review(&db, "review", &fresh, &o, &apply(&v)).unwrap()["status"],
            "unchanged"
        );
        let skills = db.skills().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0]["command"], "my-review");
        assert_eq!(skills[0]["description"], "My description");
        assert_eq!(skills[0]["import"]["argument_index"], 1);
        assert_eq!(skills[0]["parameters"][0]["name"], "input");
        assert_eq!(
            skill_import::load(&db.0.lock().unwrap(), "review").unwrap()["files"],
            fresh["files"]
        );
        assert_eq!(
            db.0.lock()
                .unwrap()
                .query_row::<String, _, _>(
                    "SELECT receipt FROM run_commands WHERE run_id=?",
                    [&ids[0]],
                    |r| r.get(0)
                )
                .unwrap(),
            snapshot
        );
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_and_kindred_conflicts_require_resolution_and_both_review_hashes() {
        let db = Db::open(":memory:").unwrap();
        let p = package("Original", "old helper");
        let o = origin(&p);
        install(&db, &p, &o);
        db.save_skill(&json!({"name":"review","body":"Kindred edit"}))
            .unwrap();
        let unchanged = review(&db, "review", &p, &o, &json!({"action":"preview"})).unwrap();
        assert_eq!(unchanged["status"], "local_changes_only");
        assert_eq!(
            review(&db, "review", &p, &o, &apply(&unchanged)).unwrap()["status"],
            "local_changes_only"
        );
        let fresh = package("Source edit", "new helper");
        let v = review(&db, "review", &fresh, &o, &json!({"action":"preview"})).unwrap();
        assert_eq!(v["status"], "conflict");
        assert_eq!(
            review(&db, "review", &fresh, &o, &apply(&v)).unwrap()["status"],
            "conflict"
        );
        assert_eq!(db.skills().unwrap()[0]["body"], "Kindred edit");
        let newer = package("Source raced", "new helper");
        assert!(review(&db, "review", &newer, &o, &apply(&v)).is_err());
        db.save_skill(&json!({"name":"review","body":"Kindred raced"}))
            .unwrap();
        assert!(review(&db, "review", &fresh, &o, &apply(&v)).is_err());
        let v = review(&db, "review", &fresh, &o, &json!({"action":"preview"})).unwrap();
        let mut choice = apply(&v);
        choice["conflict_policy"] = json!("use_source");
        assert_eq!(
            review(&db, "review", &fresh, &o, &choice).unwrap()["status"],
            "updated"
        );
        assert_eq!(db.skills().unwrap()[0]["body"], "Source edit");
    }

    #[test]
    fn uploads_cannot_forge_links_and_existing_links_cannot_silently_move() {
        let db = Db::open(":memory:").unwrap();
        let mut p = package("Original", "helper");
        let o = origin(&p);
        p["origin"] = o.clone();
        let v = skill_import::request(&db, &json!({"action":"preview","package":p,"origin":o}))
            .unwrap();
        skill_import::request(
            &db,
            &json!({"action":"import","package":p,"expected_hash":v["import"]["hash"],"origin":o}),
        )
        .unwrap();
        assert_eq!(
            catalog(&db).unwrap()["workflows"][0]["import"]["refreshable"],
            false
        );
        // An unchanged, verified local re-import can link a legacy/uploaded copy.
        let v = skill_import::request_with_origin(
            &db,
            &json!({"action":"preview","package":p}),
            Some(&o),
        )
        .unwrap();
        skill_import::request_with_origin(
            &db,
            &json!({"action":"import","package":p,"expected_hash":v["import"]["hash"]}),
            Some(&o),
        )
        .unwrap();
        assert_eq!(catalog(&db).unwrap()["workflows"][0]["import"]["origin"], o);
        let other = origin(&p);
        assert!(
            skill_import::request_with_origin(
                &db,
                &json!({"action":"import","package":p,"expected_hash":v["import"]["hash"]}),
                Some(&other)
            )
            .is_err()
        );
        assert!(review(&db, "review", &p, &other, &json!({"action":"preview"})).is_err());
        assert!(
            catalog(&Db::open(":memory:").unwrap()).unwrap()["workflows"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn real_local_receipts_bind_refresh_and_offline_sources_preserve_the_copy() {
        use axum::{Json, extract::State};
        let app = crate::tests::app();
        let mut bot = crate::tests::bot(&app.db, "codex");
        let reg = json!({"id":db::id(),"secret":format!("{}{}",db::id(),db::id()),"name":"Source desktop","mode":"full"});
        let _ = crate::local_access::poll(State(app.clone()), Json(reg.clone()))
            .await
            .ok()
            .expect("fixture operation");
        bot.profile.local_access = true;
        bot.profile.local_device_id = "*".into();
        app.db.save_bot(&bot).unwrap();
        app.db
            .save_setting("general", &json!({"local_access":true}))
            .unwrap();
        app.db.queue(&bot.id, "Refresh our workflows", 0).unwrap();
        let run = app.db.claim().unwrap().unwrap();
        async fn invoke(
            app: &crate::runtime::Shared,
            bot: &Bot,
            run: &Run,
            reg: &Value,
            tool: &str,
            args: Value,
            p: Value,
        ) -> Value {
            let expect_failure = p["failed"] == true;
            let a = app.clone();
            let b = bot.clone();
            let r = run.clone();
            let name = tool.to_owned();
            let task = tokio::spawn(async move {
                crate::runtime::call_tool(&a, &b, &r, &name, args)
                    .await
                    .ok()
                    .expect("fixture operation")
            });
            let request = tokio::time::timeout(std::time::Duration::from_secs(4), async {
                loop {
                    let polled = crate::local_access::poll(State(app.clone()), Json(reg.clone()))
                        .await
                        .ok()
                        .expect("fixture operation")
                        .0;
                    if polled["request"].is_object() {
                        break polled["request"].clone();
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .ok()
            .expect("fixture operation");
            assert_eq!(request["tool"], "local_skill_bundle");
            let mut receipt = reg.clone();
            receipt["receipt"] = json!({"id":request["id"],"nonce":request["nonce"],"result":p});
            let _ = crate::local_access::poll(State(app.clone()), Json(receipt))
                .await
                .ok()
                .expect("fixture operation");
            let answer = task.await.ok().expect("fixture operation");
            if expect_failure {
                assert_eq!(answer["failed"], true);
                return answer;
            }
            assert_ne!(answer["failed"], true, "{answer}");
            serde_json::from_str(answer["text"].as_str().unwrap()).unwrap()
        }
        let mut p = package("Original", "never execute");
        p["kindred_device_id"] = json!("forged");
        let preview = invoke(
            &app,
            &bot,
            &run,
            &reg,
            "skill_import_local",
            json!({"path":p["source"],"device_id":reg["id"],"action":"preview"}),
            p.clone(),
        )
        .await;
        invoke(&app,&bot,&run,&reg,"skill_import_local",json!({"path":p["source"],"device_id":reg["id"],"action":"import","expected_hash":preview["import"]["hash"]}),p.clone()).await;
        assert_eq!(
            catalog(&app.db).unwrap()["workflows"][0]["import"]["origin"]["device_id"],
            reg["id"]
        );
        let fresh = package("Improved source", "updated supporting script");
        let v = invoke(
            &app,
            &bot,
            &run,
            &reg,
            "skill_refresh_local",
            json!({"action":"preview","name":"review"}),
            fresh.clone(),
        )
        .await;
        let mut args = apply(&v);
        args["name"] = json!("review");
        assert_eq!(
            invoke(&app, &bot, &run, &reg, "skill_refresh_local", args, fresh).await["status"],
            "updated"
        );
        let failed = invoke(
            &app,
            &bot,
            &run,
            &reg,
            "skill_refresh_local",
            json!({"action":"preview","name":"review"}),
            json!({"failed":true,"text":"Source file no longer exists"}),
        )
        .await;
        assert_eq!(failed["failed"], true);
        app.db
            .0
            .lock()
            .unwrap()
            .execute("UPDATE local_devices SET seen=0", [])
            .unwrap();
        let mut other = reg.clone();
        other["id"] = json!(db::id());
        let _ = crate::local_access::poll(State(app.clone()), Json(other))
            .await
            .ok()
            .expect("fixture operation");
        assert!(
            local(
                &app,
                &bot,
                &run,
                &json!({"action":"preview","name":"review"})
            )
            .await
            .is_err()
        );
        assert_eq!(app.db.skills().unwrap()[0]["body"], "Improved source");
        assert_eq!(app.db.skills().unwrap().len(), 1);
    }
    #[test]
    fn ordinary_bots_can_import_and_refresh_commands_from_other_harnesses() {
        for (provider, folder) in [
            ("codex", ".pi/prompts"),
            ("openrouter", ".codex/prompts"),
            ("claude-code", ".agents/skills/review"),
        ] {
            let app = crate::tests::app();
            let bot = crate::tests::bot(&app.db, provider);
            assert!(
                crate::workspace_import::origin(&app.db, &bot.id)
                    .unwrap()
                    .is_null()
            );
            let mut p = json!({"name":"review","entry":"review.md","source":format!("/fixture/{folder}/review.md"),"files":{"review.md":STANDARD.encode("Review original input")}});
            let o = origin(&p);
            install(&app.db, &p, &o);
            p["files"]["review.md"] = json!(STANDARD.encode("Review the newest input"));
            let v = review(&app.db, "review", &p, &o, &json!({"action":"preview"})).unwrap();
            assert_eq!(v["status"], "update_available");
            assert_eq!(
                review(&app.db, "review", &p, &o, &apply(&v)).unwrap()["status"],
                "updated"
            );
            assert_eq!(app.db.skills().unwrap()[0]["command"], "our-review");
            assert_eq!(
                app.db.skills().unwrap()[0]["body"],
                "Review the newest input"
            );
            assert!(
                crate::workspace_import::origin(&app.db, &bot.id)
                    .unwrap()
                    .is_null()
            );
        }
    }
}
