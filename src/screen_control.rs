//! Persisted manual-control state and its source, shared across clients.
use crate::db::{self, Db};
use anyhow::{Result, ensure};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub fn migrate(c: &Connection) -> Result<()> {
    let columns = c
        .prepare("PRAGMA table_info(screens)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (name, definition) in [
        ("control_id", "TEXT NOT NULL DEFAULT ''"),
        ("control_reason", "TEXT NOT NULL DEFAULT ''"),
        ("control_started", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !columns.iter().any(|v| v == name) {
            c.execute(
                &format!("ALTER TABLE screens ADD COLUMN {name} {definition}"),
                [],
            )?;
        }
    }
    Ok(())
}

pub fn set(db: &Db, slot: i64, enabled: bool, reason: &str) -> Result<()> {
    ensure!(
        !enabled || matches!(reason, "manual" | "open_app" | "teaching"),
        "Unknown control action"
    );
    let c = db.0.lock().unwrap();
    if enabled {
        anyhow::ensure!(
            !crate::vm_maintenance::busy(&c)?,
            "The bot computer is updating. Try again when updates finish."
        );
    }
    crate::vm_maintenance::touch(&c)?;
    c.execute("UPDATE screens SET takeover=?,control_id=?,control_reason=?,control_started=? WHERE slot=?", params![enabled, if enabled {db::id()} else {String::new()}, if enabled {reason} else {""}, if enabled {db::now()} else {0}, slot])?;
    Ok(())
}

pub fn paused(db: &Db) -> Result<Vec<Value>> {
    Ok(db.0.lock().unwrap().prepare("SELECT s.bot_id,b.name,s.slot,s.control_id,s.control_reason,s.control_started,(SELECT COUNT(*) FROM runs r WHERE r.bot_id=s.bot_id AND r.status='queued') FROM screens s JOIN bots b ON b.id=s.bot_id WHERE s.takeover=1 ORDER BY b.name")?.query_map([], |r| {
        let id: String=r.get(3)?;
        let slot: i64=r.get(2)?;
        Ok(json!({"bot_id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"control_id":if id.is_empty(){format!("legacy-{slot}")}else{id},"reason":r.get::<_,String>(4)?,"started":r.get::<_,i64>(5)?,"queued":r.get::<_,i64>(6)?}))
    })?.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn return_control_is_scoped_and_keeps_queued_work_and_other_pauses() {
        let app = crate::tests::app();
        let one = crate::tests::bot(&app.db, "codex");
        let two = crate::tests::bot(&app.db, "codex");
        let slot = app.db.screen(&one.id).unwrap();
        let other = app.db.screen(&two.id).unwrap();
        set(&app.db, slot, true, "open_app").unwrap();
        set(&app.db, other, true, "manual").unwrap();
        let run = app.db.queue(&one.id, "Wait for the user", 0).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let router = crate::web::router(app.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::new();
        let status: Value = client
            .get(format!("{base}/api/status?bot_id={}", two.id))
            .bearer_auth(&app.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status["control_pauses"].as_array().unwrap().len(), 2);
        let saved = status["control_pauses"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["bot_id"] == one.id)
            .unwrap();
        assert_eq!(saved["queued"], 1);
        let id = saved["control_id"].clone();
        let bad = client
            .post(format!("{base}/api/takeover"))
            .bearer_auth(&app.token)
            .json(&json!({"bot_id":one.id,"enabled":false,"control_id":"stale"}))
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status(), 400);
        assert!(app.db.screen_takeover(slot).unwrap());
        for _ in 0..2 {
            let result = client
                .post(format!("{base}/api/takeover"))
                .bearer_auth(&app.token)
                .json(&json!({"bot_id":one.id,"enabled":false,"control_id":id}))
                .send()
                .await
                .unwrap();
            assert_eq!(result.status(), 200);
        }
        assert!(!app.db.screen_takeover(slot).unwrap());
        assert!(app.db.screen_takeover(other).unwrap());
        assert_eq!(app.db.run(&run).unwrap().status, "queued");
        assert_eq!(paused(&app.db).unwrap().len(), 1);
        assert!(paused(&Db::open(":memory:").unwrap()).unwrap().is_empty());
        set(&app.db, slot, true, "manual").unwrap();
        let stale = client
            .post(format!("{base}/api/takeover"))
            .bearer_auth(&app.token)
            .json(&json!({"bot_id":one.id,"enabled":false,"control_id":id}))
            .send()
            .await
            .unwrap();
        assert_eq!(stale.status(), 400);
        assert!(app.db.screen_takeover(slot).unwrap());
        server.abort();
    }
    #[test]
    fn existing_pauses_remain_unknown_and_new_reasons_are_durable() {
        let root = std::env::temp_dir().join(format!("kindred-control-{}", db::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("test.db");
        let bot_id;
        {
            let db = Db::open(path.to_str().unwrap()).unwrap();
            bot_id = crate::tests::bot(&db, "codex").id;
            let slot = db.screen(&bot_id).unwrap();
            db.0.lock()
                .unwrap()
                .execute("UPDATE screens SET takeover=1 WHERE slot=?", [slot])
                .unwrap();
            let old = paused(&db).unwrap();
            assert_eq!(old[0]["reason"], "");
            assert_eq!(old[0]["started"], 0);
            assert!(
                old[0]["control_id"]
                    .as_str()
                    .unwrap()
                    .starts_with("legacy-")
            );
            set(&db, slot, true, "open_app").unwrap();
            let current = paused(&db).unwrap();
            assert_eq!(current[0]["reason"], "open_app");
            assert!(current[0]["started"].as_i64().unwrap() > 0);
            assert!(set(&db, slot, true, "invented").is_err());
            assert_eq!(paused(&db).unwrap(), current);
        }
        {
            let db = Db::open(path.to_str().unwrap()).unwrap();
            let current = paused(&db).unwrap();
            assert_eq!(current.len(), 1);
            assert_eq!(current[0]["bot_id"], bot_id);
            assert_eq!(current[0]["reason"], "open_app");
            let slot = db.screen(&bot_id).unwrap();
            db.screen_set_takeover(slot, false).unwrap();
            assert!(paused(&db).unwrap().is_empty());
            db.screen_set_takeover(slot, true).unwrap();
            assert_eq!(paused(&db).unwrap()[0]["reason"], "manual");
            assert_ne!(
                paused(&db).unwrap()[0]["control_id"],
                current[0]["control_id"]
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
