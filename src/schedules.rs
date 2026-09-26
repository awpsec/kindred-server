//! Wall-clock schedules retain their local time across daylight-saving changes.
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Datelike, Duration, NaiveTime, Offset, TimeZone, Timelike, Utc};
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
            (1..=1440).contains(&self.every_minutes),
            "Repeat must be between 1 and 1440 minutes"
        );
        Ok((zone, start, if end < start { end + 1440 } else { end }))
    }
    /// Convert the wall-clock rule at its next occurrence, retaining cadence and
    /// weekday rollover. Future runs use the destination zone's DST rules.
    pub(crate) fn in_timezone(&self, destination: Tz, at: i64) -> Result<Self> {
        let (source, start, end) = self.parsed()?;
        let instant =
            DateTime::<Utc>::from_timestamp(at, 0).context("Invalid schedule timestamp")?;
        let delta = (instant
            .with_timezone(&destination)
            .offset()
            .fix()
            .local_minus_utc()
            - instant
                .with_timezone(&source)
                .offset()
                .fix()
                .local_minus_utc())
            / 60;
        let shifted = start as i32 + delta;
        let day_shift = shifted.div_euclid(1440);
        let clock = |minute: i32| {
            let m = minute.rem_euclid(1440);
            format!("{:02}:{:02}", m / 60, m % 60)
        };
        let mut converted = self.clone();
        converted.timezone = destination.name().to_owned();
        converted.days = self
            .days
            .iter()
            .map(|d| (*d as i32 - 1 + day_shift).rem_euclid(7) as u32 + 1)
            .collect();
        converted.days.sort_unstable();
        converted.start = clock(shifted);
        converted.end = clock(end as i32 + delta);
        converted.validate()?;
        Ok(converted)
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
        for offset in -1..=8 {
            let day = date
                .checked_add_signed(Duration::days(offset))
                .context("Schedule date is out of range")?;
            if !self.days.contains(&day.weekday().number_from_monday()) {
                continue;
            }
            for minute in (start..=end).step_by(self.every_minutes as usize) {
                let local = day.and_hms_opt(0, 0, 0).unwrap() + Duration::minutes(minute as i64);
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
        let today = time.weekday().number_from_monday();
        let yesterday = (today + 5) % 7 + 1;
        Ok(
            (self.days.contains(&today) && minute >= start && minute <= end)
                || (end >= 1440 && self.days.contains(&yesterday) && minute + 1440 <= end),
        )
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
