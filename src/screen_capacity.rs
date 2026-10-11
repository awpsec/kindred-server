//! Reservation identity outlives bot deletion. Guest state is never inferred from SQL holes.
use crate::db::{self, Db};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub slot: i64,
    pub owner: String,
    pub generation: String,
    pub legacy: bool,
}
#[derive(Clone, Debug)]
pub struct Transition {
    pub previous: Option<Identity>,
    pub next: Identity,
}
pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS screen_ownership(slot INTEGER PRIMARY KEY CHECK(slot BETWEEN 1 AND 32),owner_id TEXT NOT NULL,generation TEXT NOT NULL,legacy INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS screen_reassignments(slot INTEGER PRIMARY KEY,new_owner TEXT UNIQUE NOT NULL,new_generation TEXT NOT NULL,old_owner TEXT,old_generation TEXT,created INTEGER NOT NULL);")?;
    let missing=c.prepare("SELECT slot,bot_id FROM screens WHERE NOT EXISTS(SELECT 1 FROM screen_ownership o WHERE o.slot=screens.slot)")?.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (slot, owner) in missing {
        c.execute(
            "INSERT INTO screen_ownership VALUES(?,?,?,1)",
            params![slot, owner, db::id()],
        )?;
    }
    Ok(())
}
fn identity(c: &Connection, slot: i64) -> Result<Option<Identity>> {
    Ok(c.query_row(
        "SELECT slot,owner_id,generation,legacy FROM screen_ownership WHERE slot=?",
        [slot],
        |r| {
            Ok(Identity {
                slot: r.get(0)?,
                owner: r.get(1)?,
                generation: r.get(2)?,
                legacy: r.get(3)?,
            })
        },
    )
    .optional()?)
}
fn idle(c: &Connection, slot: i64) -> Result<bool> {
    // A partially imported/corrupt mapping is occupied, never an allocator hole.
    let mapped = c
        .query_row("SELECT bot_id FROM screens WHERE slot=?", [slot], |r| {
            r.get::<_, String>(0)
        })
        .optional()?;
    if let Some(mapped) = mapped {
        if identity(c, slot)?.as_ref().map(|i| i.owner.as_str()) != Some(mapped.as_str()) {
            return Ok(false);
        }
    }
    if crate::vm_maintenance::busy(c)? {
        return Ok(false);
    }
    let quiet = c
        .query_row(
            "SELECT value FROM settings WHERE key='quiet_until'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    if quiet > db::now() {
        return Ok(false);
    }
    Ok(c.query_row("SELECT NOT EXISTS(SELECT 1 FROM screen_ownership o LEFT JOIN bots b ON b.id=o.owner_id LEFT JOIN screens s ON s.slot=o.slot WHERE o.slot=?1 AND ((b.id IS NOT NULL AND COALESCE(json_extract(b.profile,'$.archived'),0)=0) OR COALESCE(s.takeover,0)<>0 OR COALESCE(s.quiet_until,0)>?2 OR EXISTS(SELECT 1 FROM runs r WHERE r.bot_id=o.owner_id AND r.status IN ('queued','running','awaiting_user','awaiting_approval','cancelling')) OR EXISTS(SELECT 1 FROM command_jobs j WHERE j.bot_id=o.owner_id AND j.status IN ('starting','running'))))",params![slot,db::now()],|r|r.get(0))?)
}
impl Db {
    pub fn existing_screen(&self, bot: &str) -> Result<Option<i64>> {
        self.bot(bot)?;
        Ok(self
            .0
            .lock()
            .unwrap()
            .query_row("SELECT slot FROM screens WHERE bot_id=?", [bot], |r| {
                r.get(0)
            })
            .optional()?)
    }
    pub fn screen_identity(&self, bot: &str, slot: i64) -> Result<Identity> {
        let c = self.0.lock().unwrap();
        ensure!(
            !c.query_row(
                "SELECT EXISTS(SELECT 1 FROM screen_reassignments WHERE slot=?)",
                [slot],
                |r| r.get::<_, bool>(0)
            )?,
            "This computer reservation is being verified. Try again shortly."
        );
        let actual = identity(&c, slot)?
            .ok_or_else(|| anyhow::anyhow!("Computer ownership is unavailable"))?;
        ensure!(
            actual.owner == bot
                && c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM screens WHERE slot=? AND bot_id=?)",
                    params![slot, bot],
                    |r| r.get::<_, bool>(0)
                )?,
            "This computer belongs to another bot"
        );
        Ok(actual)
    }
    pub fn empty_screen_candidates(&self) -> Result<Vec<i64>> {
        let c = self.0.lock().unwrap();
        let mut slots = vec![];
        for slot in 2..=32 {
            if !c.query_row(
                "SELECT EXISTS(SELECT 1 FROM screen_reassignments WHERE slot=?)",
                [slot],
                |r| r.get::<_, bool>(0),
            )? && idle(&c, slot)?
            {
                slots.push(slot)
            }
        }
        Ok(slots)
    }
    pub fn begin_empty_reassignment(&self, bot: &str, slot: i64) -> Result<Transition> {
        ensure!(
            (2..=32).contains(&slot),
            "The primary desktop cannot be reclaimed"
        );
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM bots WHERE id=? AND COALESCE(json_extract(profile,'$.archived'),0)=0)",[bot],|r|r.get::<_,bool>(0))?,"Choose an active bot for this computer");
        ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM screens WHERE bot_id=?) OR EXISTS(SELECT 1 FROM screen_reassignments WHERE new_owner=?)",params![bot,bot],|r|r.get::<_,bool>(0))?,"This bot already has a computer reservation");
        ensure!(idle(&tx, slot)?, "This screen is occupied");
        let previous = identity(&tx, slot)?;
        let next = Identity {
            slot,
            owner: bot.into(),
            generation: db::id(),
            legacy: false,
        };
        tx.execute(
            "INSERT INTO screen_reassignments VALUES(?,?,?,?,?,?)",
            params![
                slot,
                bot,
                next.generation,
                previous.as_ref().map(|v| &v.owner),
                previous.as_ref().map(|v| &v.generation),
                db::now()
            ],
        )?;
        tx.commit()?;
        Ok(Transition { previous, next })
    }
    pub fn finish_empty_reassignment(&self, transition: &Transition) -> Result<i64> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let next = &transition.next;
        ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM screen_reassignments WHERE slot=? AND new_owner=? AND new_generation=? AND old_owner IS ? AND old_generation IS ?)",params![next.slot,next.owner,next.generation,transition.previous.as_ref().map(|v| &v.owner),transition.previous.as_ref().map(|v| &v.generation)],|r|r.get::<_,bool>(0))?,"This computer reservation changed");
        ensure!(
            identity(&tx, next.slot)? == transition.previous,
            "Previous computer ownership changed"
        );
        ensure!(idle(&tx, next.slot)?, "This screen is no longer idle");
        ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM bots WHERE id=? AND COALESCE(json_extract(profile,'$.archived'),0)=0)",[&next.owner],|r|r.get::<_,bool>(0))?,"This bot is no longer active");
        tx.execute("DELETE FROM screens WHERE slot=?", [next.slot])?;
        tx.execute(
            "INSERT INTO screens(bot_id,slot) VALUES(?,?)",
            params![next.owner, next.slot],
        )?;
        tx.execute("INSERT INTO screen_ownership VALUES(?,?,?,0) ON CONFLICT(slot) DO UPDATE SET owner_id=excluded.owner_id,generation=excluded.generation,legacy=0",params![next.slot,next.owner,next.generation])?;
        tx.execute("DELETE FROM screen_reassignments WHERE slot=?", [next.slot])?;
        tx.commit()?;
        Ok(next.slot)
    }
}

pub const EXHAUSTED: &str = "No safely reusable computer slot is available within the 32-slot limit. Chats still work. Existing browser profiles are preserved; try again after a computer task finishes, or use a bot that already has a computer.";

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
pub fn command(identity: &Identity, mode: &str, previous: Option<&Identity>, tail: &str) -> String {
    let source = include_str!("../deploy/screen-ownership.py");
    let digest = ring::digest::digest(&ring::digest::SHA256, source.as_bytes());
    let hash = digest
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let script = source.replace(
        "EXPECTED_SOURCE_SHA256 = None",
        &format!("EXPECTED_SOURCE_SHA256 = '{}'", hash),
    );
    let mut args = format!(
        "python3 -c {} --slot {} --owner {} --generation {} --mode {}",
        quote(&script),
        identity.slot,
        quote(&identity.owner),
        quote(&identity.generation),
        mode
    );
    if identity.legacy {
        args.push_str(" --legacy")
    }
    if let Some(old) = previous {
        args.push_str(&format!(
            " --previous-owner {} --previous-generation {}",
            quote(&old.owner),
            quote(&old.generation)
        ))
    }
    if !tail.is_empty() {
        args.push_str(" -- ");
        args.push_str(tail)
    }
    args
}

async fn verify_guest_support(app: &crate::runtime::App) -> Result<()> {
    let support = crate::vm::guest(
        &app.config.vm,
        "screen_ownership_support",
        serde_json::json!({}),
    )
    .await?;
    validate_guest_support(&support)
}

pub(crate) fn validate_guest_support(support: &serde_json::Value) -> Result<()> {
    ensure!(
        support["protocol"] == 2
            && support["launcher_hashes"] == crate::vm_maintenance::managed_launcher_hashes(),
        "Update this computer runtime and launchers before verifying unused screen reservations"
    );
    Ok(())
}

async fn claim_guest(app: &crate::runtime::App, transition: &Transition) -> Result<()> {
    verify_guest_support(app).await?;
    let mut cmd = crate::vm::ssh(&app.config.vm);
    cmd.arg(command(
        &transition.next,
        "claim-empty",
        transition.previous.as_ref(),
        "",
    ));
    let bytes = crate::vm::capture(cmd, None, 15, 1024).await?;
    let receipt: serde_json::Value = serde_json::from_slice(&bytes)?;
    ensure!(
        receipt["verified_claim"] == true
            && receipt["slot"] == transition.next.slot
            && receipt["owner"] == transition.next.owner
            && receipt["generation"] == transition.next.generation,
        "Guest computer ownership receipt did not match"
    );
    Ok(())
}

pub async fn reserve(app: &crate::runtime::App, bot: &str) -> Result<i64> {
    if let Some(slot) = app.db.existing_screen(bot)? {
        app.db.screen_identity(bot, slot)?;
        ensure!(
            !app.db.bot(bot)?.profile.archived,
            "Restore this bot before using its computer"
        );
        return Ok(slot);
    }
    // Fresh reservations also require guest evidence. SQL holes may retain a
    // deleted bot's profile from before the ownership ledger was introduced.
    // Slot one is the legacy desktop and is never adopted for a different bot.
    let _allocator=app.screen_allocations.clone().try_write_owned().map_err(|_|anyhow::anyhow!("Computer allocation is busy. Your chats still work; try the computer again shortly."))?;
    if let Some(transition) = app.db.pending_screen(bot)? {
        let _ownership = app
            .screen_ownership_lock(transition.next.slot)
            .try_write_owned()
            .map_err(|_| anyhow::anyhow!("This computer connection is still active"))?;
        ensure!(
            !app.vnc.has_pending(transition.next.slot),
            "A computer connection is pending"
        );
        let _lease = app
            .screen_lock(transition.next.slot)
            .try_lock_owned()
            .map_err(|_| anyhow::anyhow!("This computer is busy"))?;
        claim_guest(app, &transition).await?;
        return app.db.finish_empty_reassignment(&transition);
    }
    let candidates = app.db.empty_screen_candidates()?;
    if !candidates.is_empty() {
        verify_guest_support(app).await?;
    }
    for slot in candidates {
        let Ok(_ownership) = app.screen_ownership_lock(slot).try_write_owned() else {
            continue;
        };
        if app.vnc.has_pending(slot) {
            continue;
        }
        let Ok(_lease) = app.screen_lock(slot).try_lock_owned() else {
            continue;
        };
        if app.db.screen_quiet(slot)? > db::now() {
            continue;
        }
        let old = { identity(&app.db.0.lock().unwrap(), slot)? };
        // Probe under the same guest lock, without making any ownership change.
        let probe = Identity {
            slot,
            owner: bot.into(),
            generation: db::id(),
            legacy: false,
        };
        let mut cmd = crate::vm::ssh(&app.config.vm);
        cmd.arg(command(&probe, "probe-empty", old.as_ref(), ""));
        let Ok(bytes) = crate::vm::capture(cmd, None, 15, 1024).await else {
            continue;
        };
        let Ok(receipt) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if receipt["empty"] != true || receipt["slot"] != slot {
            continue;
        }
        let transition = app.db.begin_empty_reassignment(bot, slot)?;
        // Unknown receipt/timeouts preserve pending state; never undo a guest claim by guessing.
        claim_guest(app, &transition).await?;
        return app.db.finish_empty_reassignment(&transition);
    }
    anyhow::bail!(EXHAUSTED)
}

impl Db {
    pub fn pending_screen(&self, bot: &str) -> Result<Option<Transition>> {
        let c = self.0.lock().unwrap();
        let pending=c.query_row("SELECT slot,new_owner,new_generation,old_owner,old_generation FROM screen_reassignments WHERE new_owner=?",[bot],|r|Ok((Identity{slot:r.get(0)?,owner:r.get(1)?,generation:r.get(2)?,legacy:false},r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?))).optional()?;
        pending
            .map(|(next, owner, generation)| {
                let previous = match (owner, generation) {
                    (Some(owner), Some(generation)) => Some(Identity {
                        slot: next.slot,
                        owner,
                        generation,
                        legacy: true,
                    }),
                    (None, None) => None,
                    _ => anyhow::bail!("Incomplete pending computer ownership"),
                };
                let actual = identity(&c, next.slot)?;
                ensure!(
                    actual.as_ref().map(|i| (&i.owner, &i.generation))
                        == previous.as_ref().map(|i| (&i.owner, &i.generation)),
                    "Previous computer ownership changed"
                );
                Ok(Transition {
                    previous: actual,
                    next,
                })
            })
            .transpose()
    }
}
