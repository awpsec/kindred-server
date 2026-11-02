//! Recover a remote draft's review from connector receipts in this task.
//! These fields are display-only: the provider still receives the original draft ID.
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub fn reference(input: &Value) -> Option<&str> {
    ["draftId", "draft_id"]
        .iter()
        .find_map(|k| input[*k].as_str().filter(|s| !s.is_empty()))
}

fn addresses(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_owned());
    }
    if let Some(a) = v.as_array() {
        return a
            .iter()
            .map(addresses)
            .collect::<Option<Vec<_>>>()
            .map(|v| v.join(", "));
    }
    let v = v.get("emailAddress").unwrap_or(v);
    v["address"]
        .as_str()
        .or_else(|| v["email"].as_str())
        .map(str::to_owned)
}

fn attachment_metadata(v: &Value) -> Value {
    json!(
        v.as_array()
            .into_iter()
            .flatten()
            .map(|a| {
                let mut safe = serde_json::Map::new();
                for key in ["filename", "name", "mimeType", "contentType", "size"] {
                    if a.get(key).is_some() {
                        safe.insert(key.into(), a[key].clone());
                    }
                }
                json!(safe)
            })
            .collect::<Vec<_>>()
    )
}

/// Do not use a snippet/bodyPreview as the message being approved.
fn full_email(v: &Value) -> Option<Value> {
    let v = v.get("message").filter(|v| v.is_object()).unwrap_or(v);
    let mut out = serde_json::Map::new();
    for (name, keys) in [
        ("to", &["to", "toRecipients", "recipients"][..]),
        ("cc", &["cc", "ccRecipients"][..]),
        ("bcc", &["bcc", "bccRecipients"][..]),
        ("from", &["from", "sender"][..]),
    ] {
        if let Some(s) = keys.iter().find_map(|k| v.get(*k).and_then(addresses)) {
            out.insert(name.into(), json!(s));
        }
    }
    if let Some(s) = v["subject"].as_str() {
        out.insert("subject".into(), json!(s));
    }
    let body = [
        "plaintextBody",
        "body_text",
        "textBody",
        "body",
        "htmlBody",
        "body_html",
        "html_body",
    ]
    .iter()
    .find_map(|k| {
        v[*k]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(|s| (*k, s))
    })
    .or_else(|| {
        v["body"]["content"].as_str().map(|s| {
            (
                if v["body"]["contentType"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case("html"))
                {
                    "htmlBody"
                } else {
                    "body"
                },
                s,
            )
        })
    })?;
    if body.1.len() > 100_000 {
        return None;
    }
    out.insert(
        if body.0.to_ascii_lowercase().contains("html") {
            "body_html"
        } else {
            "body"
        }
        .into(),
        json!(body.1),
    );
    let mut fields = crate::connector_artifacts::email_fields(&json!(out));
    if !["to", "cc", "bcc"].iter().any(|k| {
        fields[*k]["text"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty())
    }) {
        return None;
    }
    if v["attachments"].is_array() {
        fields["attachments"] = attachment_metadata(&v["attachments"]);
    }
    Some(fields)
}

fn matching(value: &Value, id: &str, depth: usize) -> Option<Value> {
    if depth > 5 {
        return None;
    }
    if value["id"] == id || value["draftId"] == id || value["draft_id"] == id {
        if let Some(email) = full_email(value) {
            return Some(email);
        }
    }
    if let Some(s) = value.as_str().filter(|s| s.len() <= 1_000_000) {
        return serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|v| matching(&v, id, depth + 1));
    }
    if let Some(items) = value.as_array() {
        return items
            .iter()
            .take(100)
            .find_map(|v| matching(v, id, depth + 1));
    }
    ["draft", "drafts", "data", "result", "content", "text"]
        .iter()
        .find_map(|k| value.get(*k).and_then(|v| matching(v, id, depth + 1)))
}

pub fn resolve(c: &Connection, run: &str, args: &Value) -> Result<Option<Value>> {
    let Some(id) = reference(crate::connector_artifacts::input(args)) else {
        return Ok(None);
    };
    // Every receipt must belong to the same task AND verified provider account
    // and connection. Never infer ownership from a subject or an email address.
    let scope: Vec<&str> = ["origin", "account_key", "connector_key"]
        .iter()
        .map(|k| args[*k].as_str().unwrap_or(""))
        .collect();
    if scope.iter().any(|s| s.is_empty()) {
        return Ok(None);
    }
    let mut stmt = c.prepare("SELECT seq,body FROM events WHERE run_id=? AND kind IN ('tool_result','connector_preview_result') AND json_valid(body) AND json_extract(body,'$.args.origin')=? AND json_extract(body,'$.args.account_key')=? AND json_extract(body,'$.args.connector_key')=? ORDER BY seq DESC LIMIT 128")?;
    let mut rows = stmt
        .query_map(params![run, scope[0], scope[1], scope[2]], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.reverse();
    let mut preview: Option<Value> = None;
    for (seq, raw) in rows {
        let event: Value = serde_json::from_str(&raw)?;
        let input = crate::connector_artifacts::input(&event["args"]);
        let targeted = reference(input) == Some(id);
        let tool = crate::connector_artifacts::tool_name(&event["args"])
            .split("__")
            .last()
            .unwrap_or("")
            .to_ascii_lowercase()
            .replace('_', "");
        if event["failed"] == true {
            if targeted {
                preview = None;
            }
            continue;
        }
        if let Some(email) = matching(&event["text"], id, 0) {
            preview = Some(json!({"email":email,"source_event_seq":seq}));
        } else if targeted && tool.contains("update") && tool.contains("draft") {
            // A successful attachment-only patch does not erase a full body read.
            // Unknown patches require a new read, rather than a guessed preview.
            let known = input.as_object().is_some_and(|m| {
                m.keys()
                    .all(|k| matches!(k.as_str(), "draftId" | "draft_id" | "attachments"))
            });
            if !known {
                preview = None;
                continue;
            }
            if let Some(p) = preview.as_mut() {
                if input["attachments"].is_array() {
                    p["email"]["attachments"] = attachment_metadata(&input["attachments"]);
                }
                p["source_event_seq"] = json!(seq);
            }
        } else if targeted && !tool.starts_with("get") && !tool.starts_with("read") {
            preview = None;
        }
    }
    if let Some(p) = preview.as_mut() {
        if let Some(fields) = p["email"].as_object_mut() {
            for field in fields.values_mut().filter(|v| v.is_object()) {
                field["editable"] = json!(false);
            }
        }
    }
    Ok(preview)
}

/// Retain provider-neutral draft evidence without attachment contents.
pub fn save_result(c: &Connection, card: &Value, result: &Value, failed: bool) -> Result<()> {
    if card["kind"] != "email"
        || !card["tool"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains("draft")
    {
        return Ok(());
    }
    fn safe(v: &Value, depth: usize, decode_envelope: bool) -> Value {
        if depth > 8 {
            return Value::Null;
        }
        match v {
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            if k == "attachments" {
                                attachment_metadata(v)
                            } else {
                                safe(v, depth + 1, k == "text")
                            },
                        )
                    })
                    .collect(),
            ),
            Value::Array(a) => json!(
                a.iter()
                    .take(100)
                    .map(|v| safe(v, depth + 1, false))
                    .collect::<Vec<_>>()
            ),
            Value::String(s) if decode_envelope => serde_json::from_str::<Value>(s)
                .ok()
                .filter(|v| v.is_object() || v.is_array())
                .map(|v| safe(&v, depth + 1, false))
                .unwrap_or_else(|| v.clone()),
            _ => v.clone(),
        }
    }
    let mut args = card["review_scope"].clone();
    if !args.is_object() {
        return Ok(());
    }
    args["tool_name"] = card["tool"].clone();
    args["input"] = safe(&card["input"], 0, false);
    c.execute(
        "INSERT INTO events(run_id,kind,body,created) VALUES(?,'connector_preview_result',?,?)",
        params![
            card["run_id"].as_str(),
            json!({"args":args,"text":safe(result,0,true),"failed":failed}).to_string(),
            crate::db::now()
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE events(seq INTEGER PRIMARY KEY,run_id TEXT,kind TEXT,body TEXT);",
        )
        .unwrap();
        c
    }
    fn args() -> Value {
        json!({"origin":"claude-account","account_key":"account-a","connector_key":"gmail-a","input":{"draftId":"draft-1"}})
    }
    fn receipt(c: &Connection, input: Value, tool: &str, output: Value, failed: bool) {
        let mut a = args();
        a["input"] = input;
        a["tool_name"] = json!(tool);
        c.execute(
            "INSERT INTO events(run_id,kind,body) VALUES('run','tool_result',?)",
            [json!({"args":a,"failed":failed,"text":output.to_string()}).to_string()],
        )
        .unwrap();
    }
    fn read(c: &Connection) {
        receipt(
            c,
            json!({"draftId":"draft-1"}),
            "get_draft",
            json!({"id":"draft-1","toRecipients":["review@example.invalid"],"subject":"Exact draft","plaintextBody":"Full message","snippet":"Short"}),
            false,
        );
    }
    #[test]
    fn read_then_attachment_patch_keeps_complete_review_and_original_reference() {
        let c = db();
        read(&c);
        receipt(
            &c,
            json!({"draftId":"draft-1","attachments":[{"filename":"scope.docx","content":"PRIVATE_BYTES"}]}),
            "update_draft",
            json!({"id":"draft-1"}),
            false,
        );
        let a = args();
        let p = resolve(&c, "run", &a).unwrap().unwrap();
        assert_eq!(p["email"]["body"]["text"], "Full message");
        assert_eq!(p["email"]["body"]["editable"], false);
        assert_eq!(p["email"]["attachments"][0]["filename"], "scope.docx");
        assert!(!p.to_string().contains("PRIVATE_BYTES"));
        assert_eq!(a["input"], json!({"draftId":"draft-1"}));
    }
    #[test]
    fn scope_failures_unknown_patches_and_snippets_cannot_supply_review() {
        let c = db();
        read(&c);
        let mut a = args();
        a["account_key"] = json!("other");
        assert!(resolve(&c, "run", &a).unwrap().is_none());
        assert!(resolve(&c, "other-run", &args()).unwrap().is_none());
        receipt(
            &c,
            json!({"draftId":"draft-1","body":"changed"}),
            "update_draft",
            json!({"id":"draft-1"}),
            false,
        );
        assert!(resolve(&c, "run", &args()).unwrap().is_none());
        read(&c);
        receipt(
            &c,
            json!({"draftId":"draft-1"}),
            "get_draft",
            json!({"error":"timeout"}),
            true,
        );
        assert!(resolve(&c, "run", &args()).unwrap().is_none());
        assert!(
            matching(
                &json!({"id":"draft-1","toRecipients":["a@example.invalid"],"snippet":"Partial"}),
                "draft-1",
                0
            )
            .is_none()
        );
    }
    #[test]
    fn bcc_only_and_html_body_are_complete_but_foreign_drafts_are_not() {
        let value = json!({"id":"draft-1","bccRecipients":[{"emailAddress":{"address":"a@example.invalid"}}],"plaintextBody":"","htmlBody":"<p>Full body</p>"});
        assert_eq!(
            matching(&value, "draft-1", 0).unwrap()["body"]["format"],
            "html"
        );
        assert!(matching(&value, "other", 0).is_none());
    }
}
