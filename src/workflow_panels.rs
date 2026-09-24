//! Compact workflow views. Only authenticated user responses can record decisions.
use crate::db::{self, Db, Run};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS workflow_responses(panel_id TEXT NOT NULL REFERENCES visual_panels(id),revision INTEGER NOT NULL,body TEXT NOT NULL,run_id TEXT NOT NULL REFERENCES runs(id),PRIMARY KEY(panel_id,revision));")?;
    Ok(())
}
fn text(v: &Value, k: &str, max: usize) -> Result<()> {
    ensure!(
        v[k].as_str()
            .is_some_and(|s| !s.trim().is_empty() && s.len() <= max),
        "{k} is required (maximum {max} bytes)"
    );
    Ok(())
}
fn url(v: &Value, k: &str) -> Result<()> {
    text(v, k, 2000)?;
    let u = reqwest::Url::parse(v[k].as_str().unwrap())?;
    ensure!(
        u.scheme() == "https"
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none(),
        "Use an HTTPS source without credentials"
    );
    Ok(())
}
fn array<'a>(v: &'a Value, k: &str, max: usize) -> Result<&'a Vec<Value>> {
    let a = v[k]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("{k} is required"))?;
    ensure!(!a.is_empty() && a.len() <= max, "Invalid {k} count");
    Ok(a)
}
pub fn validate(v: &Value) -> Result<()> {
    // These fields belong to the server, never to a model-supplied panel.
    for k in ["response", "file", "dependencies", "monitor_state"] {
        ensure!(v.get(k).is_none(), "{k} is server-owned");
    }
    match v["kind"].as_str() {
        Some("project") => {}
        Some("review" | "upload") => {
            text(v, "file_id", 100)?;
            if v["kind"] == "upload" {
                text(v, "destination", 1500)?;
                text(v, "account", 200)?;
                url(v, "destination_url")?;
                filename(v["filename"].as_str().unwrap_or(""))?;
            }
        }
        Some("monitor") => {
            text(v, "routine_id", 100)?;
        }
        Some("sources") => {
            for source in array(v, "sources", 12)? {
                text(source, "title", 200)?;
                url(source, "url")?;
                if source.get("icon_url").is_some() {
                    url(source, "icon_url")?;
                }
            }
        }
        Some("schedule") => {
            let mut option_ids = std::collections::HashSet::new();
            let mut option_kinds = std::collections::HashSet::new();
            for option in array(v, "meeting_options", 6)? {
                text(option, "id", 100)?;
                ensure!(
                    option_ids.insert(option["id"].as_str().unwrap()),
                    "Duplicate meeting option"
                );
                ensure!(
                    option_kinds.insert(option["kind"].as_str().unwrap_or("")),
                    "Offer each meeting format only once for the selected calendar"
                );
                match option["kind"].as_str() {
                    Some("none") => ensure!(
                        option.get("url").is_none() && option.get("location").is_none(),
                        "No conferencing cannot include a link or location"
                    ),
                    Some("in_person") => {
                        text(option, "location", 500)?;
                        ensure!(
                            option.get("url").is_none(),
                            "In-person meetings cannot include a conferencing link"
                        );
                    }
                    Some("google_meet" | "zoom" | "other") => {
                        if option["kind"] == "other" || option.get("url").is_some() {
                            url(option, "url")?;
                        }
                        if let Some(link) = option["url"].as_str() {
                            let u = reqwest::Url::parse(link)?;
                            let host = u.host_str().unwrap_or("");
                            ensure!(
                                option["kind"] != "google_meet" || host == "meet.google.com",
                                "Google Meet requires a meet.google.com URL"
                            );
                            ensure!(
                                option["kind"] != "zoom"
                                    || host == "zoom.us"
                                    || host.ends_with(".zoom.us")
                                    || host == "zoom.com"
                                    || host.ends_with(".zoom.com"),
                                "Zoom requires a Zoom URL"
                            );
                        }
                        ensure!(
                            option.get("location").is_none(),
                            "Use an in-person option for a physical location"
                        );
                    }
                    _ => anyhow::bail!("Choose none, google_meet, zoom, other or in_person"),
                }
            }
            text(v, "calendar", 200)?;
            text(v, "account", 200)?;
            text(v, "timezone", 100)?;
            let _: chrono_tz::Tz = v["timezone"]
                .as_str()
                .unwrap()
                .parse()
                .map_err(|_| anyhow::anyhow!("Use an IANA timezone"))?;
            for a in array(v, "attendees", 20)? {
                ensure!(
                    a.as_str()
                        .is_some_and(|s| !s.trim().is_empty() && s.len() <= 200),
                    "Invalid attendee"
                );
            }
            let mut ids = std::collections::HashSet::new();
            for slot in array(v, "slots", 5)? {
                text(slot, "id", 100)?;
                ensure!(ids.insert(slot["id"].as_str().unwrap()), "Duplicate slot");
                for k in ["start", "end"] {
                    ensure!(
                        slot[k].as_i64().is_some_and(|n| n > 0 && n < 253402300800),
                        "{k} must be Unix seconds"
                    );
                }
                ensure!(
                    slot["end"].as_i64() > slot["start"].as_i64(),
                    "Slot ends before it starts"
                );
            }
        }
        _ => anyhow::bail!("Unknown panel kind"),
    };
    Ok(())
}
fn filename(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty()
            && name.len() <= 240
            && !name.chars().any(|c| c.is_control() || "/\\".contains(c))
            && !matches!(name, "." | ".."),
        "Use a filename without path separators"
    );
    Ok(())
}
pub fn validate_refs(c: &Connection, run: &Run, v: &Value) -> Result<()> {
    if matches!(v["kind"].as_str(), Some("review" | "upload")) {
        let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM deliverables d JOIN runs r ON r.id=d.run_id WHERE d.id=? AND r.chat_id=?)",params![v["file_id"].as_str(),run.chat_id],|r|r.get(0))?;
        ensure!(exists, "Share the file in this conversation first");
    }
    if v["kind"] == "monitor" {
        let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM routines WHERE id=?1 AND bot_id=?2 UNION ALL SELECT 1 FROM mail_watches WHERE id=?1 AND bot_id=?2)",params![v["routine_id"].as_str(),run.bot_id],|r|r.get(0))?;
        ensure!(exists, "Choose this bot's saved routine");
    }
    Ok(())
}
pub fn decorate(c: &Connection, seq: i64, v: &mut Value) -> Result<()> {
    let (chat, bot): (String, String) = c.query_row(
        "SELECT chat_id,bot_id FROM visual_panels WHERE message_seq=?",
        [seq],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    // Ignore model-supplied state even for older records.
    for k in ["response", "file", "dependencies", "monitor_state"] {
        v.as_object_mut().unwrap().remove(k);
    }
    if matches!(v["kind"].as_str(), Some("review" | "upload")) {
        v["file"]=c.query_row("SELECT d.id,d.name,length(d.bytes),d.sha256 FROM deliverables d JOIN runs r ON r.id=d.run_id WHERE d.id=? AND r.chat_id=?",params![v["file_id"].as_str(),chat],|r|Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"size":r.get::<_,i64>(2)?,"sha256":r.get::<_,String>(3)?,"kind":"file"}))).optional()?.unwrap_or(Value::Null);
    }
    if v["kind"] == "project" {
        v["dependencies"]=json!(c.prepare("WITH RECURSIVE edges(child) AS (SELECT child_run_id FROM collaboration_requests WHERE source_chat_id=? UNION SELECT e.child_run_id FROM collaboration_requests e JOIN edges p ON e.parent_run_id=p.child) SELECT p.bot_id,b.name,r.bot_id,h.name,r.chat_id,r.status,e.resolved FROM edges x JOIN collaboration_requests e ON e.child_run_id=x.child JOIN runs p ON p.id=e.parent_run_id JOIN runs r ON r.id=e.child_run_id JOIN bots b ON b.id=p.bot_id JOIN bots h ON h.id=r.bot_id WHERE e.resolved=0 AND p.status NOT IN ('cancelled','cancelling') ORDER BY r.created DESC LIMIT 40")?.query_map([&chat],|r|Ok(json!({"requester_id":r.get::<_,String>(0)?,"requester":r.get::<_,String>(1)?,"bot_id":r.get::<_,String>(2)?,"name":r.get::<_,String>(3)?,"chat_id":r.get::<_,String>(4)?,"status":r.get::<_,String>(5)?,"resolved":r.get::<_,bool>(6)?})))?.collect::<rusqlite::Result<Vec<_>>>()?);
    }
    if v["kind"] == "monitor" {
        v["monitor_state"]=c.query_row("SELECT name,enabled,next_run FROM routines WHERE id=? AND bot_id=?",params![v["routine_id"].as_str(),bot],|r|Ok(json!({"name":r.get::<_,String>(0)?,"enabled":r.get::<_,bool>(1)?,"next_run":r.get::<_,i64>(2)?}))).optional()?.unwrap_or(Value::Null);
    }
    if v["kind"] == "monitor" && v["monitor_state"].is_null() {
        let watch: Option<String> = c
            .query_row(
                "SELECT body FROM mail_watches WHERE id=? AND bot_id=?",
                params![v["routine_id"].as_str(), bot],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(body) = watch {
            let w: Value = serde_json::from_str(&body)?;
            v["monitor_state"] = json!({"name":w["name"],"enabled":w["enabled"],"status":w["status"],"error":w["error"],"checked_at":w["checked_at"]});
        }
    }
    let response: Option<String> = c
        .query_row(
            "SELECT body FROM workflow_responses WHERE panel_id=? AND revision=?",
            params![v["id"].as_str(), v["revision"].as_i64()],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(body) = response {
        v["response"] = serde_json::from_str(&body)?;
    }
    Ok(())
}
pub fn respond(db: &Db, chat_id: &str, id: &str, input: &Value) -> Result<Value> {
    let chat = db.chat(chat_id)?;
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    ensure!(
        !crate::workspace_transfer::frozen(&tx)?,
        "Workspace transfer in progress"
    );
    let (seq, bot, revision): (i64, String, i64) = tx.query_row(
        "SELECT message_seq,bot_id,revision FROM visual_panels WHERE id=? AND chat_id=?",
        params![id, chat_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    ensure!(
        input["revision"].as_i64() == Some(revision),
        "This panel changed. Review the latest version."
    );
    if let Some(body) = tx
        .query_row(
            "SELECT body FROM workflow_responses WHERE panel_id=? AND revision=?",
            params![id, revision],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        let saved: Value = serde_json::from_str(&body)?;
        ensure!(
            saved["input"] == *input,
            "This revision already has a different response"
        );
        return Ok(saved);
    }
    let v = crate::visual_panels::record(&tx, seq)?;
    let action = input["action"].as_str().unwrap_or("");
    let mut response = json!({"input":input,"action":action,"revision":revision,"title":v["title"],"created":db::now()});
    match v["kind"].as_str() {
        Some("review" | "upload" | "schedule") if action == "decline" => {}
        Some("review") => {
            ensure!(
                matches!(action, "approve" | "changes"),
                "Invalid review action"
            );
            ensure!(v["file"].is_object(), "File unavailable");
            response["file"] = v["file"].clone();
            if action == "changes" {
                text(input, "notes", 4000)?;
                response["notes"] = input["notes"].clone();
            }
        }
        Some("upload") => {
            ensure!(action == "upload", "Invalid upload action");
            ensure!(v["file"].is_object(), "File unavailable");
            filename(input["filename"].as_str().unwrap_or(""))?;
            response["file"] = v["file"].clone();
            for k in ["destination", "destination_url", "account"] {
                response[k] = v[k].clone();
            }
            response["filename"] = input["filename"].clone();
        }
        Some("schedule") => {
            ensure!(action == "select", "Invalid scheduling action");
            let slot = v["slots"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["id"] == input["slot_id"])
                .ok_or_else(|| anyhow::anyhow!("Unknown slot"))?;
            ensure!(
                slot["start"].as_i64().unwrap() > db::now(),
                "This slot is in the past. Ask for updated times."
            );
            let options = v["meeting_options"].as_array().ok_or_else(|| {
                anyhow::anyhow!(
                    "Ask the bot to refresh this schedule with explicit meeting options"
                )
            })?;
            let meeting = options
                .iter()
                .find(|o| o["id"] == input["meeting_option_id"])
                .ok_or_else(|| anyhow::anyhow!("Select a meeting format"))?;
            response["meeting"] = meeting.clone();
            response["slot"] = slot.clone();
            for k in ["attendees", "calendar", "account", "timezone"] {
                response[k] = v[k].clone();
            }
        }
        _ => anyhow::bail!("This view has no user decision"),
    };
    let instruction = match action {
        "decline" => "The user declined this proposal. Do not execute, upload or send it. This is a final refusal, not a request for a replacement. Acknowledge briefly.",
        "approve" => {
            "The user approved only the attached immutable file for review, not sending or publishing it."
        }
        "changes" => {
            "The user requested these changes; revise the file and submit a new revision for review."
        }
        "upload" => {
            "The user approved uploading only this immutable file with this filename to this destination/account. Check the destination and conflicts. Do not overwrite an existing file or change permissions without separate approval. Use the normal connector approval policy. Report actual completion; this decision is not proof of upload."
        }
        _ => {
            "The user selected this time for invitation review, not sending. Recheck current availability and prepare the invitation through the actual connected calendar. Honor the exact meeting format in meeting: none means no conferencing and no automatically added Meet/Zoom link; in_person means the supplied physical location; other means the supplied link. For Google Meet or Zoom, reuse the selected URL if supplied, otherwise create a real link using an available authorized integration. Never fabricate a link or silently substitute a different provider. If unsupported, ask for a supported format. Verify the resulting event location/conference fields before requesting send approval. Request review before sending."
        }
    };
    let prompt = format!("{}\n{}", instruction, response);
    ensure!(prompt.len() < 64000, "Response too large");
    let run = crate::chats::insert_run(&tx, &chat, &bot, &prompt, &db::id(), "", 0)?;
    tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,'user',?,'message',?)",params![chat_id,format!("{} · {}",v["title"].as_str().unwrap_or("Workflow"),match action{"decline"=>"Declined","approve"=>"Approved for review","changes"=>"Changes requested","upload"=>"Upload approved",_=>"Time selected for invitation review"}),db::now()])?;
    let message = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO message_replies VALUES(?,?)",
        params![message, seq],
    )?;
    tx.execute(
        "INSERT INTO run_message_sources VALUES(?,?)",
        params![run, message],
    )?;
    tx.execute(
        "INSERT INTO workflow_responses VALUES(?,?,?,?)",
        params![id, revision, response.to_string(), run],
    )?;
    tx.commit()?;
    Ok(response)
}
pub fn schema() -> Value {
    json!({"file_id":{"type":"string","description":"Immutable share_file ID from this conversation"},"routine_id":{"type":"string"},"destination":{"type":"string"},"destination_url":{"type":"string"},"filename":{"type":"string"},"account":{"type":"string"},"calendar":{"type":"string"},"timezone":{"type":"string"},"meeting_options":{"type":"array","minItems":1,"maxItems":6,"description":"Offer one option per format, based on the selected calendar/account conferencing settings and actual available tools, plus no conferencing. Do not list unrelated connected providers. Notion is a calendar client, not a conferencing provider: use its underlying calendar and enabled conferencing connection. Google Meet/Zoom without url means create a real link. Other requires url; in_person requires location. User explicitly selects.","items":{"type":"object","properties":{"id":{"type":"string"},"kind":{"type":"string","enum":["none","google_meet","zoom","other","in_person"]},"url":{"type":"string"},"location":{"type":"string"}},"required":["id","kind"]}},"attendees":{"type":"array","items":{"type":"string"}},"slots":{"type":"array","maxItems":5,"items":{"type":"object","properties":{"id":{"type":"string"},"start":{"type":"integer","description":"Unix seconds"},"end":{"type":"integer"}},"required":["id","start","end"]}},"sources":{"type":"array","maxItems":12,"items":{"type":"object","properties":{"title":{"type":"string"},"url":{"type":"string"},"icon_url":{"type":"string"}},"required":["title","url"]}}})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Db, Run, Value) {
        let db = Db::open(":memory:").unwrap();
        let b = crate::tests::bot(&db, "codex");
        let id = db.queue(&b.id, "Review report", 0).unwrap();
        let run = db.run(&id).unwrap();
        let f = db
            .attach_file(&run, "/workspace/report.txt", b"Report v1")
            .unwrap();
        let p = json!({"key":"review","kind":"review","expected_revision":0,"title":"Review report","source":"Shared file","as_of":"Today","file_id":f["id"]});
        (db, run, p)
    }
    #[test]
    fn review_decisions_are_revision_bound_atomic_and_idempotent() {
        let (db, run, mut p) = setup();
        let first = crate::visual_panels::save(&db, &run, &p).unwrap();
        let id = first["id"].as_str().unwrap();
        let input = json!({"action":"approve","revision":1});
        assert!(respond(&db, "wrong", id, &input).is_err());
        let one = respond(&db, &run.chat_id, id, &input).unwrap();
        assert_eq!(one, respond(&db, &run.chat_id, id, &input).unwrap());
        assert!(one["file"]["sha256"].as_str().unwrap().len() == 64);
        assert!(
            respond(
                &db,
                &run.chat_id,
                id,
                &json!({"action":"changes","revision":1,"notes":"Different"})
            )
            .is_err()
        );
        let count: i64 =
            db.0.lock()
                .unwrap()
                .query_row("SELECT count(*) FROM workflow_responses", [], |r| r.get(0))
                .unwrap();
        assert_eq!(count, 1);
        p["expected_revision"] = json!(1);
        crate::visual_panels::save(&db, &run, &p).unwrap();
        assert!(respond(&db, &run.chat_id, id, &input).is_err());
        assert!(crate::visual_panels::read(&db, &run, "review").unwrap()["response"].is_null());
    }
    #[test]
    fn declined_revision_can_be_revised_without_duplicating_the_panel() {
        let (db, run, mut p) = setup();
        let first = crate::visual_panels::save(&db, &run, &p).unwrap();
        let id = first["id"].as_str().unwrap();
        let input = json!({"action":"decline","revision":1});
        let decision = respond(&db, &run.chat_id, id, &input).unwrap();
        assert_eq!(decision["action"], "decline");
        assert_eq!(decision, respond(&db, &run.chat_id, id, &input).unwrap());
        p["expected_revision"] = json!(1);
        p["title"] = json!("Revised report");
        let updated = crate::visual_panels::save(&db, &run, &p).unwrap();
        assert_eq!(updated["id"], first["id"]);
        assert!(updated["response"].is_null());
        assert!(respond(&db, &run.chat_id, id, &input).is_err());
    }
    #[test]
    fn schedule_selection_rejects_unknown_past_and_invalid_slots() {
        let (db, run, _) = setup();
        let p = json!({"key":"meeting","kind":"schedule","expected_revision":0,"title":"Meet","source":"Calendar","as_of":"Today","calendar":"Work","account":"me","timezone":"America/New_York","meeting_options":[{"id":"no-video","kind":"none"},{"id":"meet","kind":"google_meet"}],"attendees":["person@example.com"],"slots":[{"id":"one","start":db::now()+3600,"end":db::now()+7200},{"id":"past","start":1,"end":2}]});
        let saved = crate::visual_panels::save(&db, &run, &p).unwrap();
        let id = saved["id"].as_str().unwrap();
        for slot in ["missing", "past"] {
            assert!(
                respond(
                    &db,
                    &run.chat_id,
                    id,
                    &json!({"action":"select","revision":1,"slot_id":slot})
                )
                .is_err()
            );
        }
        for format in ["", "unknown"] {
            assert!(respond(&db,&run.chat_id,id,&json!({"action":"select","revision":1,"slot_id":"one","meeting_option_id":format})).is_err());
        }
        let response = respond(
            &db,
            &run.chat_id,
            id,
            &json!({"action":"select","revision":1,"slot_id":"one","meeting_option_id":"no-video"}),
        )
        .unwrap();
        assert_eq!(response["account"], "me");
        assert_eq!(response["meeting"]["kind"], "none");
        let mut invalid_meeting = p.clone();
        invalid_meeting["meeting_options"] =
            json!([{"id":"none","kind":"none","url":"https://meet.google.com/abc"}]);
        assert!(validate(&invalid_meeting).is_err());
        invalid_meeting["meeting_options"] =
            json!([{"id":"meet","kind":"google_meet","url":"https://example.com/fake"}]);
        assert!(validate(&invalid_meeting).is_err());
        invalid_meeting["meeting_options"] =
            json!([{"id":"zoom","kind":"zoom","url":"https://team.zoom.us/j/123"}]);
        assert!(validate(&invalid_meeting).is_ok());
        invalid_meeting["meeting_options"] = json!([{"id":"place","kind":"in_person"}]);
        assert!(validate(&invalid_meeting).is_err());
        invalid_meeting["meeting_options"][0]["location"] = json!("Office, room 2");
        assert!(validate(&invalid_meeting).is_ok());

        let mut duplicates = p.clone();
        duplicates["meeting_options"] = json!([{"id":"one","kind":"google_meet"},{"id":"two","kind":"google_meet","url":"https://meet.google.com/abc-defg-hij"}]);
        assert!(validate(&duplicates).is_err());
        let mut invalid = p.clone();
        invalid["timezone"] = json!("Mars/Olympus");
        assert!(validate(&invalid).is_err());
        invalid = p;
        invalid["slots"][1]["id"] = json!("one");
        assert!(validate(&invalid).is_err());
    }
    #[test]
    fn upload_binds_file_and_destination_and_rejects_forgery() {
        let (db, run, mut p) = setup();
        p["kind"] = json!("upload");
        p["destination"] = json!("Clients/Acme/Reports");
        p["destination_url"] = json!("https://drive.google.com/drive/folders/example");
        p["account"] = json!("Work");
        p["filename"] = json!("final.txt");
        let saved = crate::visual_panels::save(&db, &run, &p).unwrap();
        let id = saved["id"].as_str().unwrap();
        assert!(
            respond(
                &db,
                &run.chat_id,
                id,
                &json!({"revision":1,"action":"upload","filename":"../secret"})
            )
            .is_err()
        );
        let response = respond(
            &db,
            &run.chat_id,
            id,
            &json!({"revision":1,"action":"upload","filename":"reviewed.txt"}),
        )
        .unwrap();
        assert_eq!(response["filename"], "reviewed.txt");
        assert_eq!(response["destination"], p["destination"]);
        p["response"] = json!({"action":"approve"});
        assert!(validate(&p).is_err());
        p.as_object_mut().unwrap().remove("response");
        p["expected_revision"] = json!(1);
        p["file_id"] = json!("missing");
        assert!(crate::visual_panels::save(&db, &run, &p).is_err());
    }
    #[test]
    fn project_states_are_server_derived_and_empty_is_honest() {
        let (db, run, _) = setup();
        let p = json!({"key":"project","kind":"project","expected_revision":0,"title":"Project","source":"Workspace","as_of":"Today"});
        crate::visual_panels::save(&db, &run, &p).unwrap();
        let record = crate::visual_panels::read(&db, &run, "project").unwrap();
        assert_eq!(record["dependencies"], json!([]));
    }
    #[test]
    fn nested_dependencies_follow_real_requests_and_responses_transfer() {
        let (db, run, p) = setup();
        let helper = crate::tests::bot(&db, "codex");
        let child = db
            .chat_handoff(&run, &helper.id, "Finish assessment")
            .unwrap();
        let child_run = db.run(&child).unwrap();
        let third = crate::tests::bot(&db, "codex");
        db.chat_handoff(&child_run, &third.id, "Check evidence")
            .unwrap();
        let project = json!({"key":"project","kind":"project","expected_revision":0,"title":"Project","source":"Workspace","as_of":"Today"});
        crate::visual_panels::save(&db, &run, &project).unwrap();
        let record = crate::visual_panels::read(&db, &run, "project").unwrap();
        assert_eq!(record["dependencies"].as_array().unwrap().len(), 2);
        let saved = crate::visual_panels::save(&db, &run, &p).unwrap();
        respond(
            &db,
            &run.chat_id,
            saved["id"].as_str().unwrap(),
            &json!({"action":"approve","revision":1}),
        )
        .unwrap();
        // Export is tested with no active task; it must preserve the exact decision.
        db.0.lock()
            .unwrap()
            .execute("UPDATE runs SET status='completed'", [])
            .unwrap();
        let package = db.prepare_transfer(&db::id(), "Workflow transfer").unwrap();
        let to = Db::open(":memory:").unwrap();
        to.import_transfer(&package).unwrap();
        assert_eq!(
            crate::visual_panels::read(&to, &run, "review").unwrap()["response"]["action"],
            "approve"
        );
    }
}
