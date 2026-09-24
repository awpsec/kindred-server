//! Conservative draft transforms. Only existing, recognized fields are editable.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub fn extend_email_fields(value: &Value, mut fields: Value) -> Value {
    // Microsoft Graph message/sendMail JSON. MIME and unfamiliar recipient
    // objects retain their original input and are never rewritten.
    let (message, prefix) = if value["message"].is_object() {
        (&value["message"], "/message")
    } else {
        (value, "")
    };
    for (label, key) in [
        ("to", "toRecipients"),
        ("cc", "ccRecipients"),
        ("bcc", "bccRecipients"),
    ] {
        if fields.get(label).is_some() {
            continue;
        }
        if let Some(rows) = message[key].as_array() {
            let exact = rows.iter().all(|row| {
                row.as_object().is_some_and(|o| o.len() == 1)
                    && row["emailAddress"].as_object().is_some_and(|o| {
                        o.keys().all(|k| matches!(k.as_str(), "address" | "name"))
                            && o.get("address").is_some_and(Value::is_string)
                            && o.get("name").is_none_or(Value::is_string)
                    })
            });
            if exact {
                let text = rows
                    .iter()
                    .filter_map(|r| r["emailAddress"]["address"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                fields[label] = json!({"key":format!("{prefix}/{key}"),"text":text,"array":true,"encoding":"graph_recipients","editable":true});
            }
        }
    }
    if fields.get("subject").is_none() {
        if let Some(text) = message["subject"].as_str() {
            fields["subject"] =
                json!({"key":format!("{prefix}/subject"),"text":text,"editable":true});
        }
    }
    if fields.get("body").is_none() {
        if let (Some(text), Some(format)) = (
            message["body"]["content"].as_str(),
            message["body"]["contentType"].as_str(),
        ) {
            if matches!(format.to_ascii_lowercase().as_str(), "html" | "text") {
                fields["body"] = json!({"key":format!("{prefix}/body/content"),"text":text,"format":format.to_ascii_lowercase(),"editable":true});
            }
        }
    }
    if fields.get("from").is_none() {
        if let Some(address) = message["from"]["emailAddress"]["address"].as_str() {
            fields["from"] = json!({"key":format!("{prefix}/from/emailAddress/address"),"text":address,"editable":false});
        }
    }
    fields
}

pub fn read_only(card: &Value) -> bool {
    card["read_only"] == true
        || card["display_read_only"] == true
        || crate::connector_artifacts::read_only_hint(&json!({"tool_name":card["tool"]}))
        || card["tool"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase()
            .split('_')
            .any(|part| {
                matches!(
                    part,
                    "get" | "list" | "search" | "fetch" | "read" | "query" | "find"
                )
            })
}

pub fn fields(card: &Value) -> Value {
    let mut out = json!({});
    if read_only(card) {
        return out;
    }
    let keys: &[&str] = match card["kind"].as_str().unwrap_or("") {
        "task" => &[
            "title",
            "name",
            "content",
            "summary",
            "description",
            "notes",
            "desc",
            "item_name",
            "due",
            "due_date",
            "due_on",
            "due_at",
        ],
        "calendar" | "meeting" => &[
            "title",
            "summary",
            "subject",
            "topic",
            "description",
            "agenda",
            "location",
            "start",
            "end",
            "start_time",
            "end_time",
        ],
        "message" => &["text", "content", "message", "body"],
        "repository" | "support" => &["title", "subject", "body", "description"],
        _ => &[],
    };
    for key in keys {
        if let Some(text) = card["input"][key].as_str() {
            let multiline = matches!(
                *key,
                "description"
                    | "notes"
                    | "desc"
                    | "agenda"
                    | "text"
                    | "content"
                    | "message"
                    | "body"
            );
            out[key] = json!({"key":key,"text":text,"label":key.replace('_', " "),"type":if multiline {"textarea"} else {"text"},"editable":true});
        }
    }
    out
}

pub fn apply(card: &Value, edits: &Value) -> Result<Value> {
    ensure!(
        !read_only(card),
        "Read-only connector inputs cannot be edited here"
    );
    let current = &card["input"];
    let email = card["kind"] == "email";
    let known = if email {
        crate::connector_artifacts::email_fields(current)
    } else {
        fields(card)
    };
    let edits = edits.as_object().context("Draft edits must be an object")?;
    ensure!(
        !edits.is_empty() && edits.len() <= 16,
        "Choose supported draft fields to edit"
    );
    let mut next = current.clone();
    for (field, value) in edits {
        let meta = &known[field];
        ensure!(
            meta["editable"] == true && (!email || field != "from"),
            "This draft field is not editable"
        );
        let value = value.as_str().context("Draft edits must be text")?;
        let multiline = (email && field == "body") || meta["type"] == "textarea";
        ensure!(
            value.len() <= if multiline { 100_000 } else { 8_000 },
            "Draft field is too long"
        );
        ensure!(
            multiline || !value.chars().any(char::is_control),
            "Headers and single-line fields cannot contain control characters"
        );
        let key = meta["key"].as_str().context("Missing draft field")?;
        let old = if key.starts_with('/') {
            current.pointer(key)
        } else {
            current.get(key)
        }
        .context("Draft field is no longer present")?;
        let replacement = if meta["encoding"] == "graph_recipients" {
            let rows = old.as_array().context("Recipient list changed")?;
            let mut result = Vec::new();
            for address in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                ensure!(
                    address.contains('@')
                        && !address.chars().any(char::is_whitespace)
                        && !address.contains(['<', '>', ';']),
                    "Use comma-separated email addresses without display names"
                );
                result.push(
                    rows.iter()
                        .find(|r| {
                            r["emailAddress"]["address"]
                                .as_str()
                                .is_some_and(|a| a.eq_ignore_ascii_case(address))
                        })
                        .cloned()
                        .unwrap_or_else(|| json!({"emailAddress":{"address":address}})),
                );
            }
            json!(result)
        } else if meta["array"] == true {
            json!(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            )
        } else {
            json!(value)
        };
        if key.starts_with('/') {
            *next
                .pointer_mut(key)
                .context("Draft field is no longer present")? = replacement;
        } else {
            next[key] = replacement;
        }
    }
    ensure!(
        next.to_string().len() <= 200_000,
        "Connector input too large for review"
    );
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn graph_draft_preserves_envelope_attachments_and_matching_recipient_names() {
        let input = json!({"message":{"subject":"Review","toRecipients":[{"emailAddress":{"address":"old@example.invalid","name":"Old name"}}],"ccRecipients":[],"body":{"contentType":"HTML","content":"<p>Original</p>"},"attachments":[{"name":"review.pdf","contentBytes":"fixture-only"}],"from":{"emailAddress":{"address":"owner@example.invalid"}},"internetMessageHeaders":[{"name":"x-example","value":"keep"}]},"saveToSentItems":false});
        let card = json!({"kind":"email","input":input});
        let meta = crate::connector_artifacts::email_fields(&input);
        assert_eq!(meta["body"]["format"], "html");
        assert_eq!(meta["from"]["editable"], false);
        let edited = apply(&card, &json!({"to":"new@example.invalid, old@example.invalid","cc":"copy@example.invalid","subject":"Updated","body":"<p>Edited</p>"})).unwrap();
        assert_eq!(
            edited["message"]["toRecipients"][1],
            input["message"]["toRecipients"][0]
        );
        assert_eq!(
            edited["message"]["toRecipients"][0],
            json!({"emailAddress":{"address":"new@example.invalid"}})
        );
        assert_eq!(
            edited["message"]["attachments"],
            input["message"]["attachments"]
        );
        assert_eq!(
            edited["message"]["internetMessageHeaders"],
            input["message"]["internetMessageHeaders"]
        );
        assert_eq!(edited["message"]["from"], input["message"]["from"]);
        assert_eq!(edited["message"]["body"]["contentType"], "HTML");
        assert_eq!(edited["message"]["body"]["content"], "<p>Edited</p>");
        assert_eq!(edited["saveToSentItems"], false);
        assert!(apply(&card, &json!({"to":"Name <a@example.invalid>"})).is_err());
        assert!(apply(&card, &json!({"from":"spoof@example.invalid"})).is_err());
        assert!(
            apply(
                &card,
                &json!({"subject":"Header\r\nBcc:bad@example.invalid"})
            )
            .is_err()
        );
    }
    #[test]
    fn unsupported_email_shapes_and_read_queries_cannot_be_rewritten() {
        for value in [
            json!({"raw":"encoded-mime"}),
            json!({"draft_id":"draft-42"}),
            json!({"toRecipients":[{"emailAddress":{"address":"a@example.invalid"},"extra":"retain"}]}),
            json!({"body":{"contentType":"Other","content":"opaque"}}),
        ] {
            let card = json!({"kind":"email","input":value});
            assert!(apply(&card, &json!({"to":"b@example.invalid"})).is_err());
            assert!(apply(&card, &json!({"body":"replacement"})).is_err());
        }
        for tool in [
            "get_message",
            "GMAIL_GET_MESSAGE",
            "mcp__app__listTasks",
            "search_issues",
        ] {
            assert!(
                apply(
                    &json!({"kind":"task","tool":tool,"input":{"title":"Query"}}),
                    &json!({"title":"Other"})
                )
                .is_err()
            );
        }
        assert!(
            apply(
                &json!({"kind":"task","read_only":true,"input":{"title":"Query"}}),
                &json!({"title":"Other"})
            )
            .is_err()
        );
    }
    #[test]
    fn task_and_calendar_edits_preserve_routing_types_and_time_zones() {
        let input = json!({"title":"Draft","description":"Original","due_date":"2026-09-20","assignee_id":42,"labels":["review"],"completed":false,"start":{"dateTime":"2026-09-20T10:00:00-04:00","timeZone":"America/New_York"}});
        let card = json!({"kind":"task","tool":"create_task","input":input});
        let next = apply(
            &card,
            &json!({"title":"Final","description":"Line one\nLine two","due_date":"2026-09-21"}),
        )
        .unwrap();
        for key in ["assignee_id", "labels", "completed", "start"] {
            assert_eq!(next[key], input[key]);
        }
        for edits in [
            json!({"assignee_id":"43"}),
            json!({"title":42}),
            json!({"start":"tomorrow"}),
            json!({"title":"bad\nheader"}),
            json!({"missing":"value"}),
        ] {
            assert!(apply(&card, &edits).is_err());
        }
        let event = json!({"kind":"calendar","input":{"summary":"Meeting","start":"2026-09-20T10:00:00-04:00","end":"2026-09-20T11:00:00-04:00","calendar_id":"primary"}});
        let next = apply(&event, &json!({"summary":"Planning"})).unwrap();
        assert_eq!(next["start"], event["input"]["start"]);
        assert_eq!(next["end"], event["input"]["end"]);
        assert_eq!(next["calendar_id"], "primary");
        assert!(
            apply(
                &json!({"kind":"record","input":{"title":"Unknown"}}),
                &json!({"title":"Changed"})
            )
            .is_err()
        );
    }
}
