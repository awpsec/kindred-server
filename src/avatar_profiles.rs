//! Versioned artwork stays outside legacy profile JSON so rollback readers remain valid.
use crate::db::BotProfile;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AvatarProfile {
    pub version: u8,
    pub body: String,
    pub expression: String,
    pub palette: Option<String>,
    pub custom_color: Option<String>,
}
pub fn catalog() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_str(include_str!("../ui/avatar-runtime/catalog.json"))
            .expect("avatar catalog")
    })
}
pub fn hex(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
}
pub fn validate(a: &AvatarProfile) -> Result<()> {
    let c = catalog();
    ensure!(
        a.version == 1
            && c["bodies"].as_array().unwrap().iter().any(|v| v == &a.body)
            && c["expressions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == &a.expression),
        "Unknown avatar body or expression"
    );
    ensure!(
        match (&a.palette, &a.custom_color) {
            (Some(p), None) => c["palettes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["id"] == *p),
            (None, Some(v)) => hex(v),
            _ => false,
        },
        "Choose an avatar palette or six-digit custom color"
    );
    Ok(())
}
pub fn legacy(p: &BotProfile) -> Result<BotProfile> {
    let mut p = p.clone();
    if let Some(a) = p.avatar.take() {
        validate(&a)?;
        let c = catalog();
        p.shape = c["fallbackBodies"][&a.body].as_str().unwrap().into();
        p.eyes = c["fallbackExpressions"][&a.expression]
            .as_str()
            .unwrap()
            .into();
        p.color = if let Some(id) = a.palette {
            if id == "black" {
                "#ffffff".into()
            } else {
                c["palettes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["id"] == id)
                    .unwrap()["light"]
                    .as_str()
                    .unwrap()
                    .into()
            }
        } else {
            a.custom_color.unwrap()
        };
    }
    Ok(p)
}
fn fingerprint(p: &BotProfile) -> String {
    format!("{}|{}|{}", p.shape, p.eyes, p.color)
}
pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS bot_avatars(bot_id TEXT PRIMARY KEY REFERENCES bots(id) ON DELETE CASCADE,fingerprint TEXT NOT NULL,artwork TEXT NOT NULL);
        CREATE TRIGGER IF NOT EXISTS delete_bot_avatar AFTER DELETE ON bots BEGIN DELETE FROM bot_avatars WHERE bot_id=OLD.id; END;")?;
    Ok(())
}
pub fn read(c: &Connection, id: &str, p: &mut BotProfile) -> rusqlite::Result<()> {
    p.avatar = None;
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT fingerprint,artwork FROM bot_avatars WHERE bot_id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((f, v)) = row.filter(|(f, _)| *f == fingerprint(p)) {
        let _ = f;
        if let Ok(a) = serde_json::from_str::<AvatarProfile>(&v) {
            if validate(&a).is_ok() {
                p.avatar = Some(a);
            }
        }
    }
    Ok(())
}
pub fn write(c: &Connection, id: &str, original: &BotProfile, stored: &BotProfile) -> Result<()> {
    if let Some(a) = &original.avatar {
        validate(a)?;
        c.execute("INSERT INTO bot_avatars(bot_id,fingerprint,artwork) VALUES(?,?,?) ON CONFLICT(bot_id) DO UPDATE SET fingerprint=excluded.fingerprint,artwork=excluded.artwork",params![id,fingerprint(stored),serde_json::to_string(a)?])?;
    } else {
        c.execute(
            "DELETE FROM bot_avatars WHERE bot_id=? AND fingerprint<>?",
            params![id, fingerprint(stored)],
        )?;
    }
    Ok(())
}

pub fn resolved(p: &BotProfile) -> AvatarProfile {
    if let Some(a) = p.avatar.as_ref().filter(|a| validate(a).is_ok()) {
        return a.clone();
    }
    let c = catalog();
    let color = if hex(&p.color) {
        p.color.as_str()
    } else {
        "#2A92FE"
    };
    let channels: Vec<u8> = [1, 3, 5]
        .iter()
        .map(|i| u8::from_str_radix(&color[*i..*i + 2], 16).unwrap())
        .collect();
    let adaptive = channels.iter().min().unwrap() >= &210
        && channels.iter().max().unwrap() - channels.iter().min().unwrap() < 22;
    AvatarProfile {
        version: 1,
        body: c["legacyBodies"][&p.shape]
            .as_str()
            .unwrap_or("blob")
            .into(),
        expression: c["legacyExpressions"][&p.eyes]
            .as_str()
            .unwrap_or("neutral")
            .into(),
        palette: adaptive.then(|| "black".into()),
        custom_color: (!adaptive).then(|| color.into()),
    }
}

pub fn default_for_id(id: &str) -> AvatarProfile {
    let hash = id
        .bytes()
        .fold(0u32, |h, c| h.wrapping_mul(31).wrapping_add(u32::from(c)));
    let pool = catalog()["defaults"].as_array().unwrap();
    AvatarProfile {
        version: 1,
        body: pool[hash as usize % pool.len()].as_str().unwrap().into(),
        expression: "neutral".into(),
        palette: Some("blue".into()),
        custom_color: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::{Db, id},
        tests::bot,
    };
    fn artwork(body: &str) -> AvatarProfile {
        AvatarProfile {
            version: 1,
            body: body.into(),
            expression: "happy".into(),
            palette: Some("blue".into()),
            custom_color: None,
        }
    }
    #[test]
    fn all_identities_roundtrip_without_polluting_legacy_profile_json() {
        let root = std::env::temp_dir().join(format!("kindred-avatar-{}", id()));
        let path = root.join("data.db");
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let mut b = bot(&db, "codex");
        for body in catalog()["bodies"].as_array().unwrap() {
            b.profile.avatar = Some(artwork(body.as_str().unwrap()));
            db.save_bot(&b).unwrap();
            let loaded = db.bot(&b.id).unwrap();
            assert_eq!(loaded.profile.avatar, b.profile.avatar);
            let raw: String =
                db.0.lock()
                    .unwrap()
                    .query_row("SELECT profile FROM bots WHERE id=?", [&b.id], |r| r.get(0))
                    .unwrap();
            let v: Value = serde_json::from_str(&raw).unwrap();
            assert!(v.get("avatar").is_none());
            assert!(matches!(
                v["shape"].as_str().unwrap(),
                "round"
                    | "square"
                    | "pebble"
                    | "capsule"
                    | "triangle"
                    | "hexagon"
                    | "cloud"
                    | "drop"
            ));
            assert_eq!(v["local_access"], false);
            assert_eq!(v["notifications"], true);
        }
        drop(db);
        let reopened = Db::open(path.to_str().unwrap()).unwrap();
        assert_eq!(
            reopened.bot(&b.id).unwrap().profile.avatar.unwrap().body,
            "star"
        );
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn old_server_edits_win_and_unrelated_preferences_preserve_artwork() {
        let db = Db::open(":memory:").unwrap();
        let mut b = bot(&db, "codex");
        b.profile.avatar = Some(artwork("bean"));
        db.save_bot(&b).unwrap();
        let raw: String =
            db.0.lock()
                .unwrap()
                .query_row("SELECT profile FROM bots WHERE id=?", [&b.id], |r| r.get(0))
                .unwrap();
        let mut old: BotProfile = serde_json::from_str(&raw).unwrap();
        old.label = "old client label".into();
        b.profile = old;
        db.save_bot(&b).unwrap();
        assert!(db.bot(&b.id).unwrap().profile.avatar.is_some());
        db.0.lock().unwrap().execute("UPDATE bots SET profile=json_set(profile,'$.shape','capsule','$.color','#123456') WHERE id=?",[&b.id]).unwrap();
        let loaded = db.bot(&b.id).unwrap();
        assert!(loaded.profile.avatar.is_none());
        assert_eq!(resolved(&loaded.profile).body, "tablet");
        assert_eq!(
            resolved(&loaded.profile).custom_color.as_deref(),
            Some("#123456")
        );
        b.profile = loaded.profile;
        db.save_bot(&b).unwrap();
        assert_eq!(
            db.0.lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM bot_avatars WHERE bot_id=?",
                    [&b.id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    #[test]
    fn sidecar_failure_rolls_back_profile_and_unrelated_identity() {
        let db = Db::open(":memory:").unwrap();
        let mut b = bot(&db, "codex");
        let previous = db.bot(&b.id).unwrap();
        db.0.lock().unwrap().execute_batch("CREATE TEMP TRIGGER refuse_avatar BEFORE INSERT ON main.bot_avatars BEGIN SELECT RAISE(ABORT,'fixture');END;").unwrap();
        b.name = "should rollback".into();
        b.profile.avatar = Some(artwork("capsule"));
        assert!(db.save_bot(&b).is_err());
        assert_eq!(db.bot(&b.id).unwrap().name, previous.name);
        assert!(db.bot(&b.id).unwrap().profile.avatar.is_none());
    }
    #[test]
    fn invalid_versions_seasonal_and_ambiguous_paints_refuse() {
        let mut a = artwork("bean");
        assert!(validate(&a).is_ok());
        a.body = "pumpkin".into();
        assert!(validate(&a).is_err());
        a.body = "capsule".into();
        a.version = 2;
        assert!(validate(&a).is_err());
        a.version = 1;
        a.custom_color = Some("#123456".into());
        assert!(validate(&a).is_err());
        a.palette = None;
        assert!(validate(&a).is_ok());
        a.custom_color = Some("url(secret)".into());
        assert!(validate(&a).is_err());
    }
    #[test]
    fn deletion_removes_sidecar_after_preserving_safe_usage_identity() {
        let db = Db::open(":memory:").unwrap();
        let mut b = bot(&db, "codex");
        b.profile.avatar = Some(artwork("heart"));
        db.save_bot(&b).unwrap();
        let c = db.0.lock().unwrap();
        c.execute("INSERT INTO provider_usage(run_id,request_id,bot_id,provider,model,input_tokens,output_tokens,cached_tokens,cost_source,created,bot_name,bot_profile) VALUES('usage',1,?,'codex','test',1,1,0,'unknown',1,'old','{}')",[&b.id]).unwrap();
        c.execute("DELETE FROM chats WHERE id=?", [format!("dm-{}", b.id)])
            .unwrap();
        c.execute("DELETE FROM bots WHERE id=?", [&b.id]).unwrap();
        let raw: String = c
            .query_row("SELECT bot_profile FROM provider_usage", [], |r| r.get(0))
            .unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["avatar"]["body"], "heart");
        assert!(v.get("local_access").is_none());
        assert!(v.get("local_device_id").is_none());
        assert_eq!(
            c.query_row("SELECT count(*) FROM bot_avatars", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[cfg(test)]
mod api_tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use serde_json::json;
    use tower::ServiceExt;
    async fn call(
        app: crate::runtime::Shared,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {}", app.token))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = crate::web::router(app).oneshot(req).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 2_000_000).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    #[tokio::test]
    async fn all_bodies_save_reload_through_authenticated_api_and_assets_have_js_mime() {
        let app = crate::tests::app();
        let mut b = crate::tests::bot(&app.db, "codex");
        for body in catalog()["bodies"].as_array().unwrap() {
            b.profile.avatar = Some(AvatarProfile {
                version: 1,
                body: body.as_str().unwrap().into(),
                expression: "surprised".into(),
                palette: None,
                custom_color: Some("#123456".into()),
            });
            let (s, v) = call(
                app.clone(),
                "PUT",
                &format!("/api/bots/{}", b.id),
                serde_json::to_value(&b).unwrap(),
            )
            .await;
            assert_eq!(s, StatusCode::OK, "{v}");
            assert_eq!(v["profile"]["avatar"]["body"], *body);
            assert_eq!(v["profile"]["color"], "#123456");
        }
        let (s, v) = call(app.clone(), "GET", "/api/bots", Value::Null).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v[0]["profile"]["avatar"]["body"], "star");
        for file in [
            "catalog.js",
            "host.js",
            "grok-avatar-engine.mjs",
            "rolldown-runtime.mjs",
        ] {
            let response = crate::web::router(app.clone())
                .oneshot(
                    Request::builder()
                        .uri(format!("/avatar-runtime/{file}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(
                response.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with("text/javascript")
            );
        }
        b.profile.avatar.as_mut().unwrap().body = "pumpkin".into();
        let (s, _) = call(
            app,
            "PUT",
            &format!("/api/bots/{}", b.id),
            serde_json::to_value(&b).unwrap(),
        )
        .await;
        assert!(!s.is_success());
    }
    #[tokio::test]
    async fn new_bot_default_is_seeded_by_saved_id_and_explicit_pick_wins() {
        let app = crate::tests::app();
        let b = crate::tests::bot(&app.db, "codex");
        let mut v = serde_json::to_value(&b).unwrap();
        v["avatar_default"] = json!(true);
        let (s, created) = call(app.clone(), "POST", "/api/bots", v).await;
        assert_eq!(s, StatusCode::OK);
        let id = created["id"].as_str().unwrap();
        assert_eq!(
            created["profile"]["avatar"],
            serde_json::to_value(default_for_id(id)).unwrap()
        );
        let mut color_only = serde_json::to_value(&b).unwrap();
        color_only["avatar_default"] = json!(true);
        color_only["profile"]["avatar"] = json!({"version":1,"body":"blob","expression":"calm","palette":null,"custom_color":"#123456"});
        let (s, custom) = call(app.clone(), "POST", "/api/bots", color_only).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(
            custom["profile"]["avatar"]["body"],
            default_for_id(custom["id"].as_str().unwrap()).body
        );
        assert_eq!(custom["profile"]["avatar"]["expression"], "calm");
        assert_eq!(custom["profile"]["avatar"]["custom_color"], "#123456");
        assert!(custom["profile"]["avatar"]["palette"].is_null());
        let mut v = serde_json::to_value(&b).unwrap();
        v["profile"]["avatar"] = json!({"version":1,"body":"capsule","expression":"calm","palette":"red","custom_color":null});
        let (s, created) = call(app, "POST", "/api/bots", v).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(created["profile"]["avatar"]["body"], "capsule");
        assert_eq!(created["profile"]["shape"], "square");
    }
}
