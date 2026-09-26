//! One profile time zone for bot context and existing schedules.
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
            "guidance":"Use this profile time zone for dates, relative times, reminders, and new routines unless the user explicitly names another zone. Existing routines convert when the profile time zone changes. Unix timestamps remain UTC."}))
    } else {
        Ok(
            json!({"name":null,"local_time":null,"guidance":"The profile time zone has not been set. Ask before interpreting an ambiguous local date or reminder; do not assume UTC is the user's zone."}),
        )
    }
}

/// Called under the same transaction as the settings change, so a scheduler
/// cannot observe converted rules paired with the old profile zone.
pub(crate) fn convert_routines(c: &rusqlite::Connection, general: &Value) -> Result<()> {
    let Some(name) = general["timezone"].as_str().filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let destination = parse(name)?;
    let previous: Option<String> = c
        .query_row("SELECT value FROM settings WHERE key='general'", [], |r| {
            r.get(0)
        })
        .optional()?;
    let previous: Value = previous
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default();
    if previous["timezone"] == name {
        return Ok(());
    }
    let rows = c
        .prepare("SELECT id,schedule,next_run FROM routines WHERE schedule IS NOT NULL")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, raw, next) in rows {
        let schedule: crate::schedules::WeeklySchedule = serde_json::from_str(&raw)?;
        let now = crate::db::now();
        let at = if next >= now {
            next
        } else {
            schedule.next_after(now)?
        };
        let converted = schedule.in_timezone(destination, at)?;
        c.execute(
            "UPDATE routines SET schedule=? WHERE id=?",
            params![serde_json::to_string(&converted)?, id],
        )?;
    }
    // Interval cadence and one-time UTC timestamps must not move.
    Ok(())
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
        convert_routines(&tx, &general)?;
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
    fn profile_changes_convert_existing_rules_without_moving_due_times() {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "codex");
        app.db
            .save_setting(
                "general",
                &json!({"timezone":"America/New_York","timezone_mode":"auto"}),
            )
            .unwrap();
        let at = resolve("2030-01-07T09:00", parse("America/New_York").unwrap()).unwrap();
        let schedule = crate::schedules::WeeklySchedule {
            timezone: "America/New_York".into(),
            days: vec![1],
            start: "09:00".into(),
            end: "18:00".into(),
            every_minutes: 60,
        };
        let mut routine = crate::db::Routine {
            id: "weekly".into(),
            bot_id: bot.id,
            name: "Check".into(),
            prompt: "Check".into(),
            interval_seconds: 3600,
            next_run: at,
            enabled: false,
            run_at: None,
            schedule: Some(schedule.clone()),
        };
        app.db.save_routine(&routine).unwrap();
        routine.id = "once".into();
        routine.schedule = None;
        routine.run_at = Some(at);
        app.db.save_routine(&routine).unwrap();
        routine.id = "interval".into();
        routine.run_at = None;
        app.db.save_routine(&routine).unwrap();
        initialize(&app.db, "Asia/Tokyo").unwrap();
        let routines = app.db.routines().unwrap();
        assert!(routines.iter().all(|r| r.next_run == at && !r.enabled));
        assert_eq!(
            routines.iter().find(|r| r.id == "once").unwrap().run_at,
            Some(at)
        );
        let shifted = routines
            .iter()
            .find(|r| r.id == "weekly")
            .unwrap()
            .schedule
            .as_ref()
            .unwrap();
        assert_eq!(
            (&shifted.start, &shifted.end, shifted.days.as_slice()),
            (&"23:00".to_string(), &"08:00".to_string(), &[1][..])
        );
        assert_eq!(shifted.next_after(at - 1).unwrap(), at);
        assert_eq!(shifted.next_after(at).unwrap(), at + 3600);
        assert!(shifted.inside_window(at + 9 * 3600).unwrap());
        assert!(!shifted.inside_window(at + 10 * 3600).unwrap());
        app.db
            .save_setting(
                "general",
                &json!({"timezone":"America/Los_Angeles","timezone_mode":"fixed"}),
            )
            .unwrap();
        let converted = app
            .db
            .routines()
            .unwrap()
            .into_iter()
            .find(|r| r.id == "weekly")
            .unwrap()
            .schedule
            .unwrap();
        assert_eq!(converted.start, "06:00");
        assert_eq!(converted.end, "15:00");
        assert_eq!(converted.days, vec![1]);
        assert_eq!(converted.next_after(at - 1).unwrap(), at);
        // An ordinary preference save must not convert schedules a second time.
        app.db
            .save_setting(
                "general",
                &json!({"timezone":"America/Los_Angeles","theme":"light"}),
            )
            .unwrap();
        assert_eq!(
            app.db
                .routines()
                .unwrap()
                .into_iter()
                .find(|r| r.id == "weekly")
                .unwrap()
                .schedule
                .unwrap(),
            converted
        );
    }
    #[test]
    fn conversion_rolls_weekdays_and_uses_destination_daylight_saving() {
        let source = crate::schedules::WeeklySchedule {
            timezone: "America/New_York".into(),
            days: vec![1],
            start: "00:30".into(),
            end: "00:30".into(),
            every_minutes: 1440,
        };
        let at = resolve("2030-01-07T00:30", parse("America/New_York").unwrap()).unwrap();
        let shifted = source
            .in_timezone(parse("America/Los_Angeles").unwrap(), at)
            .unwrap();
        assert_eq!(shifted.days, vec![7]);
        assert_eq!(shifted.start, "21:30");
        assert_eq!(shifted.next_after(at - 1).unwrap(), at);
        let summer = resolve("2030-07-07T21:30", parse("America/Los_Angeles").unwrap()).unwrap();
        assert_eq!(shifted.next_after(summer - 1).unwrap(), summer);
        let half_hour = source
            .in_timezone(parse("Asia/Kolkata").unwrap(), at)
            .unwrap();
        assert_eq!(half_hour.start, "11:00");
        assert_eq!(half_hour.next_after(at - 1).unwrap(), at);
    }
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
