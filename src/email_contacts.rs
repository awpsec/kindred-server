//! Profile-local recipient suggestions and conservative mailbox parsing.
use crate::{
    db::{self, Db},
    runtime::Shared,
};
use anyhow::{Result, ensure};
use axum::{
    Json,
    extract::{Query, State},
};
use serde_json::{Value, json};

pub fn parse(value: &str) -> Result<Vec<Value>> {
    ensure!(
        value.len() <= 8000 && !value.chars().any(char::is_control),
        "Recipient headers cannot contain control characters"
    );
    let mut parts = Vec::new();
    let mut begin = 0;
    let mut quoted = false;
    let mut escaped = false;
    let mut angle = false;
    for (i, c) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' && quoted {
            escaped = true;
            continue;
        }
        if c == '"' {
            quoted = !quoted;
            continue;
        }
        if !quoted {
            match c {
                '<' => {
                    ensure!(!angle, "Invalid recipient");
                    angle = true;
                }
                '>' => {
                    ensure!(angle, "Invalid recipient");
                    angle = false;
                }
                ',' | ';' if !angle => {
                    parts.push(&value[begin..i]);
                    begin = i + c.len_utf8();
                }
                _ => {}
            }
        }
    }
    ensure!(
        !quoted && !angle && !escaped,
        "Complete the recipient address"
    );
    parts.push(&value[begin..]);
    let mut out = Vec::new();
    for part in parts.into_iter().map(str::trim).filter(|s| !s.is_empty()) {
        let (name, email) = if let Some((name, tail)) = part.split_once('<') {
            ensure!(tail.ends_with('>'), "Invalid recipient");
            (
                name.trim()
                    .trim_matches('"')
                    .replace("\\\"", "\"")
                    .replace("\\\\", "\\"),
                tail[..tail.len() - 1].trim(),
            )
        } else {
            (String::new(), part)
        };
        let pieces: Vec<_> = email.split('@').collect();
        ensure!(
            email.len() <= 254
                && pieces.len() == 2
                && !pieces[0].is_empty()
                && !pieces[1].is_empty()
                && email.is_ascii()
                && !email
                    .chars()
                    .any(|c| c.is_whitespace() || "<>(),;:\"\\[]".contains(c)),
            "Enter a complete email address"
        );
        ensure!(
            !pieces[0].starts_with('.')
                && !pieces[0].ends_with('.')
                && !email.contains("..")
                && pieces[1].split('.').all(|label| !label.is_empty()
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-')),
            "Enter a complete email address"
        );
        if !out.iter().any(|v: &Value| {
            v["email"]
                .as_str()
                .is_some_and(|a| a.eq_ignore_ascii_case(email))
        }) {
            out.push(json!({"name":name,"email":email,"text":part}));
        }
    }
    ensure!(out.len() <= 100, "Use at most 100 recipients per field");
    Ok(out)
}
pub fn remember(c: &rusqlite::Connection, input: &Value) -> Result<()> {
    let old: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE key='email_contacts'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let mut contacts: Vec<Value> = old
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let fields = crate::connector_artifacts::email_fields(input);
    for key in ["to", "cc", "bcc"] {
        if let Some(text) = fields[key]["text"].as_str() {
            if let Ok(rows) = parse(text) {
                for row in rows {
                    let index = contacts.iter().position(|v| {
                        v["email"]
                            .as_str()
                            .is_some_and(|a| a.eq_ignore_ascii_case(row["email"].as_str().unwrap()))
                    });
                    let prior = index.map(|i| contacts.remove(i));
                    let name = if row["name"] == "" {
                        prior
                            .as_ref()
                            .map(|v| v["name"].clone())
                            .unwrap_or(json!(""))
                    } else {
                        row["name"].clone()
                    };
                    contacts.push(json!({"email":row["email"],"name":name,"used":db::now()}));
                }
            }
        }
    }
    if contacts.len() > 500 {
        contacts.drain(..contacts.len() - 500);
    }
    c.execute("INSERT INTO settings(key,value) VALUES('email_contacts',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[serde_json::to_string(&contacts)?])?;
    Ok(())
}
use rusqlite::OptionalExtension;
#[derive(serde::Deserialize)]
pub struct Search {
    #[serde(default)]
    q: String,
}
pub async fn search(
    State(app): State<Shared>,
    Query(query): Query<Search>,
) -> Result<Json<Value>, crate::web::Error> {
    if query.q.len() > 200 {
        return Err(anyhow::anyhow!("Search is too long").into());
    }
    Ok(Json(json!({"contacts":suggest(&app.db,&query.q)?})))
}
fn suggest(db: &Db, query: &str) -> Result<Vec<Value>> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(vec![]);
    }
    let contacts = db.setting("email_contacts")?.unwrap_or(json!([]));
    Ok(contacts
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .filter(|v| {
            format!(
                "{} {}",
                v["name"].as_str().unwrap_or(""),
                v["email"].as_str().unwrap_or("")
            )
            .to_lowercase()
            .contains(&needle)
        })
        .take(8)
        .map(|v| json!({"name":v["name"],"email":v["email"]}))
        .collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn contact_search_requires_profile_auth() {
        use std::future::IntoFuture;
        let app = crate::tests::app();
        remember(
            &app.db.0.lock().unwrap(),
            &json!({"to":"Casey <casey@example.com>"}),
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/api/email-contacts?q=Case",
            listener.local_addr().unwrap()
        );
        let server =
            tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
        let client = reqwest::Client::new();
        assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
        let response: Value = client
            .get(&url)
            .bearer_auth(&app.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["contacts"][0]["email"], "casey@example.com");
        server.abort();
    }
    #[test]
    fn parses_named_recipients_and_rejects_injection() {
        let rows = parse("\"Smith, Jane\" <jane@example.com>; other@example.com, JANE@example.com")
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "Smith, Jane");
        for bad in [
            "bad address",
            "one@example.com two@example.com",
            "x@example.com\r\nBcc: y@example.com",
            "Name <x@example.com",
            "a@@example.com",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn suggestions_are_profile_local_and_keep_names() {
        let a = Db::open(":memory:").unwrap();
        let b = Db::open(":memory:").unwrap();
        remember(
            &a.0.lock().unwrap(),
            &json!({"to":["\"Smith, Jane\" <jane@example.com>"]}),
        )
        .unwrap();
        remember(&a.0.lock().unwrap(), &json!({"to":"jane@example.com"})).unwrap();
        assert_eq!(
            suggest(&a, "smith").unwrap()[0]["email"],
            "jane@example.com"
        );
        assert!(suggest(&b, "jane").unwrap().is_empty());
    }
}
