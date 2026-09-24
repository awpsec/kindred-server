//! Wall-clock schedules retain their local time across daylight-saving changes.
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Datelike, Days, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeeklySchedule {
    pub timezone: String,
    /// ISO weekdays: Monday = 1, Sunday = 7.
    pub days: Vec<u32>,
    pub start: String,
    pub end: String,
    pub every_minutes: u32,
}
impl WeeklySchedule {
    fn parsed(&self) -> Result<(Tz, u32, u32)> {
        let zone = self
            .timezone
            .parse::<Tz>()
            .context("Choose a valid IANA time zone, such as America/New_York")?;
        ensure!(
            !self.days.is_empty()
                && self.days.len() <= 7
                && self.days.iter().all(|d| (1..=7).contains(d)),
            "Choose one or more weekdays"
        );
        let mut days = self.days.clone();
        days.sort();
        days.dedup();
        ensure!(days.len() == self.days.len(), "Weekdays must not repeat");
        let minutes = |s: &str| -> Result<u32> {
            ensure!(
                s.len() == 5 && s.as_bytes()[2] == b':',
                "Use HH:MM for schedule times"
            );
            let t = NaiveTime::parse_from_str(s, "%H:%M").context("Use a valid 24-hour time")?;
            Ok(t.hour() * 60 + t.minute())
        };
        let (start, end) = (minutes(&self.start)?, minutes(&self.end)?);
        ensure!(
            end >= start,
            "The last run must be at or after the first run on the same day"
        );
        ensure!(
            (1..=1440).contains(&self.every_minutes),
            "Repeat must be between 1 and 1440 minutes"
        );
        Ok((zone, start, end))
    }
    pub fn validate(&self) -> Result<()> {
        self.parsed().map(|_| ())
    }
    pub fn next_after(&self, timestamp: i64) -> Result<i64> {
        let (zone, start, end) = self.parsed()?;
        let date = DateTime::<Utc>::from_timestamp(timestamp, 0)
            .context("Invalid schedule timestamp")?
            .with_timezone(&zone)
            .date_naive();
        for offset in 0..=8 {
            let day = date
                .checked_add_days(Days::new(offset))
                .context("Schedule date is out of range")?;
            if !self.days.contains(&day.weekday().number_from_monday()) {
                continue;
            }
            for minute in (start..=end).step_by(self.every_minutes as usize) {
                let local = day.and_hms_opt(minute / 60, minute % 60, 0).unwrap();
                // Skip nonexistent spring times; execute a repeated fall time only once.
                if let Some(time) = zone.from_local_datetime(&local).earliest() {
                    if time.timestamp() > timestamp {
                        return Ok(time.timestamp());
                    }
                }
            }
        }
        anyhow::bail!("No valid scheduled time found in the next week")
    }
    pub fn inside_window(&self, timestamp: i64) -> Result<bool> {
        let (zone, start, end) = self.parsed()?;
        let time = DateTime::<Utc>::from_timestamp(timestamp, 0)
            .context("Invalid schedule timestamp")?
            .with_timezone(&zone);
        let minute = time.hour() * 60 + time.minute();
        Ok(self.days.contains(&time.weekday().number_from_monday())
            && minute >= start
            && minute <= end)
    }
}

pub fn migrate(c: &rusqlite::Connection) -> Result<()> {
    let columns = c
        .prepare("PRAGMA table_info(routines)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|name| name == "schedule") {
        c.execute_batch("ALTER TABLE routines ADD COLUMN schedule TEXT;")?;
    }
    if !columns.iter().any(|name| name == "run_at") {
        c.execute_batch("ALTER TABLE routines ADD COLUMN run_at INTEGER;")?;
    }
    c.execute_batch("CREATE TABLE IF NOT EXISTS routine_runs(run_id TEXT PRIMARY KEY REFERENCES runs(id),routine_id TEXT NOT NULL,quiet INTEGER NOT NULL DEFAULT 0);")?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS routine_run_requests(request_id TEXT PRIMARY KEY,run_id TEXT NOT NULL REFERENCES runs(id));")?;
    Ok(())
}

impl crate::db::Db {
    pub fn run_routine_now(&self, id: &str) -> Result<String> {
        self.run_routine_now_request(id, None)
    }
    pub fn run_routine_now_request(&self, id: &str, request_id: Option<&str>) -> Result<String> {
        let routine = self
            .routines()?
            .into_iter()
            .find(|r| r.id == id)
            .context("Routine not found")?;
        let bot = self.bot(&routine.bot_id)?;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        ensure!(
            !crate::workspace_transfer::frozen(&tx)?,
            "This workspace is paused"
        );
        if let Some(existing) = crate::routine_controls::receipt(&tx, request_id)? {
            return Ok(existing);
        }
        // Read the current definition while holding the queue transaction.
        let (prompt, owner): (String, String) =
            tx.query_row("SELECT prompt,bot_id FROM routines WHERE id=?", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        ensure!(owner == routine.bot_id, "Routine ownership changed");
        let active:Option<String>=tx.query_row("SELECT runs.id FROM runs JOIN routine_runs ON routine_runs.run_id=runs.id WHERE routine_runs.routine_id=? AND runs.status IN ('queued','running','awaiting_user','awaiting_approval','cancelling') ORDER BY runs.created LIMIT 1",[id],|r|r.get(0)).optional()?;
        if let Some(existing) = active {
            if let Some(key) = request_id {
                tx.execute(
                    "INSERT INTO routine_run_requests VALUES(?,?)",
                    rusqlite::params![key, existing],
                )?;
            }
            tx.commit()?;
            return Ok(existing);
        }
        let chat_id = format!("dm-{}", bot.id);
        tx.execute(
            "INSERT OR IGNORE INTO chats(id,name,members) VALUES(?,?,?)",
            rusqlite::params![chat_id, bot.name, serde_json::to_string(&vec![&bot.id])?],
        )?;
        let archived: bool =
            tx.query_row("SELECT archived FROM chats WHERE id=?", [&chat_id], |r| {
                r.get(0)
            })?;
        let chat = crate::chats::Chat {
            bot_only: false,
            description: String::new(),
            id: chat_id,
            name: bot.name,
            members: vec![bot.id.clone()],
            archived,
            pinned: false,
            last_message: None,
        };
        let run = crate::chats::insert_run(&tx, &chat, &bot.id, &prompt, &crate::db::id(), "", 0)?;
        tx.execute(
            "INSERT INTO routine_runs(run_id,routine_id) VALUES(?,?)",
            rusqlite::params![run, id],
        )?;
        crate::commands::snapshot_routine(&tx, &run, &prompt)?;
        crate::provider_inbox::snapshot(&tx, &run, id)?;
        // Running a one-time reminder early consumes that single occurrence.
        tx.execute(
            "UPDATE routines SET enabled=0 WHERE id=? AND run_at IS NOT NULL",
            [id],
        )?;
        if let Some(key) = request_id {
            tx.execute(
                "INSERT INTO routine_run_requests VALUES(?,?)",
                rusqlite::params![key, run],
            )?;
        }
        tx.commit()?;
        Ok(run)
    }
}
