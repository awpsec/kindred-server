use crate::db::{self, Routine};
use serde_json::json;
use std::future::IntoFuture;

fn archived(bot: &crate::db::Bot, value: bool) -> crate::db::Bot {
    let mut candidate = bot.clone();
    candidate.profile.archived = value;
    candidate
}

#[test]
fn archiving_a_bot_is_rejected_while_any_run_is_active() {
    for status in [
        "queued",
        "running",
        "awaiting_user",
        "awaiting_approval",
        "cancelling",
    ] {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "codex");
        let run = app.db.queue(&bot.id, "active work", 0).unwrap();
        app.db
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE runs SET status=? WHERE id=?",
                rusqlite::params![status, run],
            )
            .unwrap();

        assert!(
            app.db
                .save_bot_preferences(&archived(&bot, true), false)
                .is_err(),
            "archiving must be rejected for {status} work"
        );
        assert!(!app.db.bot(&bot.id).unwrap().profile.archived);
    }
}

#[test]
fn stopping_work_then_disabling_a_routine_allows_archive_and_preserves_history() {
    let app = crate::tests::app();
    let bot = crate::tests::bot(&app.db, "codex");
    let run = app.db.queue(&bot.id, "queued work", 0).unwrap();
    app.db.cancel(&run).unwrap();
    app.db
        .save_bot_preferences(&archived(&bot, true), false)
        .unwrap();
    assert!(app.db.bot(&bot.id).unwrap().profile.archived);

    let app = crate::tests::app();
    let bot = crate::tests::bot(&app.db, "codex");
    let routine = Routine {
        id: db::id(),
        bot_id: bot.id.clone(),
        name: "Keep my queue".into(),
        prompt: "routine work".into(),
        interval_seconds: 60,
        next_run: 0,
        enabled: true,
        schedule: None,
        run_at: None,
    };
    app.db.save_routine(&routine).unwrap();
    app.db.tick(0).unwrap();
    assert_eq!(app.db.routines().unwrap()[0].enabled, true);
    assert_eq!(
        app.db
            .runs(Some(&bot.id))
            .unwrap()
            .iter()
            .filter(|r| r.status == "queued")
            .count(),
        1
    );
    assert!(
        app.db
            .save_bot_preferences(&archived(&bot, true), false)
            .is_err()
    );
    assert!(!app.db.bot(&bot.id).unwrap().profile.archived);
    assert!(app.db.routines().unwrap()[0].enabled);
    assert_eq!(
        app.db
            .runs(Some(&bot.id))
            .unwrap()
            .iter()
            .filter(|r| r.status == "queued")
            .count(),
        1
    );

    let mut disabled = routine.clone();
    disabled.enabled = false;
    app.db.save_routine(&disabled).unwrap();
    assert_eq!(
        app.db
            .runs(Some(&bot.id))
            .unwrap()
            .iter()
            .filter(|r| r.status == "queued")
            .count(),
        0
    );
    app.db
        .save_bot_preferences(&archived(&bot, true), false)
        .unwrap();
    assert!(app.db.bot(&bot.id).unwrap().profile.archived);

    let mut restored = archived(&bot, false);
    restored.name = "Restored teammate".into();
    app.db.save_bot_preferences(&restored, false).unwrap();
    assert!(!app.db.bot(&bot.id).unwrap().profile.archived);
}

#[test]
fn archiving_removes_enabled_routines_without_queued_work() {
    let app = crate::tests::app();
    let bot = crate::tests::bot(&app.db, "codex");
    let routine = Routine {
        id: db::id(),
        bot_id: bot.id.clone(),
        name: "Future check".into(),
        prompt: "routine work".into(),
        interval_seconds: 60,
        next_run: db::now() + 3600,
        enabled: true,
        schedule: None,
        run_at: None,
    };
    app.db.save_routine(&routine).unwrap();
    assert!(app.db.runs(Some(&bot.id)).unwrap().is_empty());
    app.db.save_bot_preferences(&archived(&bot, true), false).unwrap();
    assert!(app.db.routines().unwrap().is_empty());
    assert!(app.db.bot(&bot.id).unwrap().profile.archived);
}

#[tokio::test]
async fn authenticated_http_archive_attempt_is_blocked_while_queued_work_exists() {
    let app = crate::tests::app();
    let bot = crate::tests::bot(&app.db, "codex");
    app.db.queue(&bot.id, "queued work", 0).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
    let client = reqwest::Client::new();
    let mut candidate = archived(&bot, true);
    let response = client
        .put(format!("{base}/api/bots/{}", bot.id))
        .bearer_auth(&app.token)
        .json(&json!(candidate))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    candidate = app.db.bot(&bot.id).unwrap();
    assert!(!candidate.profile.archived);
    server.abort();
}

#[test]
fn a_legacy_queued_run_cannot_be_claimed_while_its_bot_is_archived() {
    let app = crate::tests::app();
    let bot = crate::tests::bot(&app.db, "codex");
    let id = app.db.queue(&bot.id, "Older queued work", 0).unwrap();
    app.db
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE bots SET profile=json_set(profile,'$.archived',json('true')) WHERE id=?",
            [&bot.id],
        )
        .unwrap();
    assert!(app.db.claim_bot(&bot.id).unwrap().is_none());
    assert_eq!(app.db.run(&id).unwrap().status, "queued");
    app.db
        .save_bot_preferences(&archived(&bot, false), false)
        .unwrap();
    assert_eq!(app.db.claim_bot(&bot.id).unwrap().unwrap().id, id);
}


#[test]
fn bot_only_chat_archives_only_after_its_last_active_member_and_preserves_history() {
    let app = crate::tests::app();
    let a = crate::tests::bot(&app.db, "codex");
    let b = crate::tests::bot(&app.db, "codex");
    let c = crate::tests::bot(&app.db, "codex");
    let room = |id: &str, members: Vec<String>, bot_only| crate::chats::Chat {id:id.into(),name:id.into(),description:String::new(),members,bot_only,archived:false,pinned:false,last_message:None};
    let pair=room("pair",vec![a.id.clone(),b.id.clone()],true);
    let human=room("human",pair.members.clone(),false);
    let trio=room("trio",vec![a.id.clone(),b.id.clone(),c.id.clone()],true);
    for chat in [&pair,&human,&trio] { app.db.save_chat(chat).unwrap(); }
    app.db.0.lock().unwrap().execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES('pair',?,'Keep this history','message',0)",[&a.id]).unwrap();
    app.db.save_bot_preferences(&archived(&a,true),false).unwrap();
    assert!(!app.db.chat("pair").unwrap().archived);
    app.db.save_bot_preferences(&archived(&b,true),false).unwrap();
    assert!(app.db.chat("pair").unwrap().archived);
    assert!(!app.db.chat("human").unwrap().archived);
    assert!(!app.db.chat("trio").unwrap().archived);
    let count:i64=app.db.0.lock().unwrap().query_row("SELECT count(*) FROM chat_messages WHERE chat_id='pair' AND body='Keep this history'",[],|r|r.get(0)).unwrap();
    assert_eq!(count,1);
    let mut manual=human.clone();manual.archived=true;
    app.db.save_chat(&manual).unwrap();
    assert!(app.db.chat("human").unwrap().archived);
    let mut new=manual.clone();new.id="new-with-archived".into();
    assert!(app.db.save_chat(&new).is_err());
    // Startup reconciliation also handles rooms left behind by older versions.
    let db=app.db.0.lock().unwrap();
    db.execute("UPDATE chats SET archived=0 WHERE id='pair'",[]).unwrap();
    assert_eq!(crate::chats::archive_inactive_bot_chats(&db).unwrap(),1);
    assert_eq!(crate::chats::archive_inactive_bot_chats(&db).unwrap(),0);
}


#[test]
fn leftover_archived_bot_routine_can_be_removed_and_cleanup_preserves_runs() {
    let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");
    let routine=Routine{id:db::id(),bot_id:bot.id.clone(),name:"Legacy".into(),prompt:"Check".into(),interval_seconds:60,next_run:0,enabled:true,schedule:None,run_at:None};
    app.db.save_routine(&routine).unwrap();app.db.tick(0).unwrap();
    let run=app.db.runs(Some(&bot.id)).unwrap()[0].id.clone();
    app.db.0.lock().unwrap().execute("UPDATE bots SET profile=json_set(profile,'$.archived',json('true')) WHERE id=?",[&bot.id]).unwrap();
    crate::routine_controls::remove_scheduled(&app,None,&routine.id).unwrap();
    assert_eq!(app.db.run(&run).unwrap().status,"cancelled");
    assert!(app.db.routines().unwrap().is_empty());
    // Recreate an old leftover directly, as an earlier version could have left it.
    app.db.0.lock().unwrap().execute("INSERT INTO routines(id,bot_id,name,prompt,interval_seconds,next_run,enabled) VALUES(?,?,'Old','Check',60,0,0)",rusqlite::params![routine.id,bot.id]).unwrap();
    crate::routine_controls::remove_archived_bot_schedules(&app.db.0.lock().unwrap()).unwrap();
    assert!(app.db.routines().unwrap().is_empty());
    assert!(app.db.run(&run).is_ok());
}
