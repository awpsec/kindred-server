//! One profile time zone for bot context and newly created schedules.
use crate::{db::Db, runtime::Shared};
use anyhow::{Context, Result, ensure};
use axum::{Json, extract::State};
use chrono::{LocalResult, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

pub fn parse(value: &str) -> Result<Tz> {
    value
        .parse()
        .context("Choose a valid IANA time zone, such as America/New_York")
}

pub fn current(db: &Db) -> Result<Option<Tz>> {
    let general = db.setting("general")?.unwrap_or_default();
    general["timezone"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(parse)
        .transpose()
}

pub fn context(db: &Db, at: i64) -> Result<Value> {
    if let Some(zone) = current(db)? {
        let time = zone
            .timestamp_opt(at, 0)
            .single()
            .context("Invalid current time")?;
        Ok(json!({"name":zone.name(),"local_time":time.to_rfc3339(),
            "guidance":"Use this profile time zone for dates, relative times, reminders, and new routines unless the user explicitly names another zone. Existing routines retain their saved time zone. Unix timestamps remain UTC."}))
    } else {
        Ok(
            json!({"name":null,"local_time":null,"guidance":"The profile time zone has not been set. Ask before interpreting an ambiguous local date or reminder; do not assume UTC is the user's zone."}),
        )
    }
}

fn initialize(db: &Db, zone: &str) -> Result<Value> {
    let zone = parse(zone)?;
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    let existing: Option<String> = tx
        .query_row("SELECT value FROM settings WHERE key='general'", [], |r| {
            r.get(0)
        })
        .optional()?;
    let mut general =
        crate::db::general_settings(existing.map(|s| serde_json::from_str(&s)).transpose()?);
    if general["timezone"].as_str().is_none_or(str::is_empty) || general["timezone_mode"] == "auto"
    {
        general["timezone"] = json!(zone.name());
        general["timezone_mode"] = json!("auto");
        tx.execute("INSERT INTO settings(key,value) VALUES('general',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![general.to_string()])?;
    }
    tx.commit()?;
    Ok(general)
}

pub async fn initialize_route(
    State(app): State<Shared>,
    Json(v): Json<Value>,
) -> Result<Json<Value>, crate::web::Error> {
    Ok(Json(initialize(
        &app.db,
        v["timezone"].as_str().context("Missing time zone")?,
    )?))
}

pub(crate) fn resolve(local: &str, zone: Tz) -> Result<i64> {
    ensure!(local.len() == 16, "Enter a complete local date and time");
    let date = NaiveDateTime::parse_from_str(local, "%Y-%m-%dT%H:%M")
        .context("Enter a valid local date and time")?;
    match zone.from_local_datetime(&date) {
        LocalResult::Single(at) => Ok(at.timestamp()),
        LocalResult::None => anyhow::bail!(
            "That time does not exist because the clock moves forward. Choose a time outside the daylight-saving transition."
        ),
        LocalResult::Ambiguous(_, _) => anyhow::bail!(
            "That time occurs twice when the clock moves back. Choose a time outside the daylight-saving transition."
        ),
    }
}

pub async fn resolve_route(
    State(app): State<Shared>,
    Json(v): Json<Value>,
) -> Result<Json<Value>, crate::web::Error> {
    let zone = match v["timezone"].as_str() {
        Some(zone) => parse(zone)?,
        None => current(&app.db)?.context("Set the bot time zone in General settings first")?,
    };
    let at = resolve(
        v["local"].as_str().context("Missing local date and time")?,
        zone,
    )?;
    Ok(Json(json!({"run_at":at,"timezone":zone.name()})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_device_sets_a_shared_zone_without_overwriting_saved_preferences() {
        let app = crate::tests::app();
        app.db
            .save_setting(
                "general",
                &json!({"name":"Person","local_access":true,"identity":"Keep this"}),
            )
            .unwrap();
        let first = initialize(&app.db, "America/New_York").unwrap();
        assert_eq!(first["timezone"], "America/New_York");
        assert_eq!(first["identity"], "Keep this");
        assert_eq!(first["local_access"], true);
        assert_eq!(
            initialize(&app.db, "Asia/Tokyo").unwrap()["timezone"],
            "Asia/Tokyo"
        );
        let mut fixed = first.clone();
        fixed["timezone_mode"] = json!("fixed");
        app.db.save_setting("general", &fixed).unwrap();
        assert_eq!(initialize(&app.db, "Asia/Tokyo").unwrap(), fixed);
        assert!(initialize(&app.db, "Invalid/Zone").is_err());
        let winter = context(&app.db, 1767276000).unwrap();
        assert_eq!(winter["name"], "America/New_York");
        assert!(winter["local_time"].as_str().unwrap().ends_with("-05:00"));
    }
    #[test]
    fn scheduling_uses_the_profile_zone_and_rejects_ambiguous_or_missing_clock_times() {
        let ny = parse("America/New_York").unwrap();
        let winter = resolve("2027-01-01T09:00", ny).unwrap();
        assert_eq!(
            chrono::DateTime::from_timestamp(winter, 0)
                .unwrap()
                .to_rfc3339(),
            "2027-01-01T14:00:00+00:00"
        );
        assert_eq!(
            resolve("2027-07-01T09:00", ny).unwrap(),
            resolve("2027-07-01T13:00", parse("UTC").unwrap()).unwrap()
        );
        assert!(resolve("2027-03-14T02:30", ny).is_err());
        assert!(resolve("2027-11-07T01:30", ny).is_err());
        assert!(resolve("2027-07-01T09:00Z", ny).is_err());
    }
}
