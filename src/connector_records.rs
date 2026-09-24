//! Presentation adapters. These never authorize execution or fetch additional data.
use serde_json::{Value, json};
use std::sync::OnceLock;

pub fn catalog() -> &'static Vec<Value> {
    static ROWS: OnceLock<Vec<Value>> = OnceLock::new();
    ROWS.get_or_init(|| {
        serde_json::from_str(include_str!("../ui/connector-catalog.json"))
            .expect("valid bundled connector catalogue")
    })
}
fn key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
pub fn service(connection: &str, tool: &str) -> String {
    let raw = key(connection.trim_start_matches("claude.ai "));
    let tool = tool.to_ascii_lowercase();
    if raw == "atlassian" {
        return if tool.contains("jira") {
            "jira"
        } else {
            "confluence"
        }
        .into();
    }
    if let Some(row) = catalog()
        .iter()
        .find(|r| r["id"] == raw || r["aliases"].as_array().unwrap().iter().any(|a| a == &raw))
    {
        return row["id"].as_str().unwrap().into();
    }
    if matches!(raw.as_str(), "googleworkspace" | "google") {
        for row in catalog() {
            let id = row["id"].as_str().unwrap();
            if (id.starts_with("google") || id == "gmail") && tool.replace('_', "").contains(id) {
                return id.into();
            }
        }
    }
    raw
}
pub fn profile(service: &str) -> Option<&'static Value> {
    catalog().iter().find(|r| r["id"] == service)
}
pub fn family(service: &str) -> &'static str {
    profile(service)
        .and_then(|r| r["family"].as_str())
        .unwrap_or("record")
}
fn at<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = v;
    for part in path.split('/') {
        current = if let Some(a) = current.as_array() {
            a.get(part.parse::<usize>().ok()?)?
        } else {
            current.get(part)?
        };
    }
    Some(current)
}
fn short(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}
fn display(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return short(s, 4000);
    }
    if v.is_boolean() || v.is_number() {
        return v.to_string();
    }
    if let Some(a) = v.as_array() {
        return a
            .iter()
            .take(20)
            .map(display)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
    }
    for k in [
        "displayName",
        "name",
        "plain_text",
        "text",
        "value",
        "email",
        "address",
        "username",
        "dateTime",
        "date",
        "start",
        "login",
        "title",
    ] {
        if let Some(v) = v.get(k) {
            let s = display(v);
            if !s.is_empty() {
                return s;
            }
        }
    }
    String::new()
}
fn pick(v: &Value, paths: &[&str]) -> String {
    paths
        .iter()
        .find_map(|p| at(v, p).map(display).filter(|s| !s.is_empty()))
        .unwrap_or_default()
}
fn field(fields: &mut Value, label: &str, value: String) {
    if !value.is_empty() {
        fields[label] = json!(value);
    }
}
fn rich(v: &Value, depth: usize, out: &mut String) {
    if depth > 12 || out.len() > 16000 {
        return;
    }
    if let Some(a) = v.as_array() {
        for v in a.iter().take(100) {
            rich(v, depth + 1, out);
        }
        return;
    }
    if let Some(t) = v
        .get("textRun")
        .and_then(|v| v["content"].as_str())
        .or_else(|| v["plain_text"].as_str())
    {
        out.push_str(t);
        return;
    }
    if v["type"] == "text" {
        if let Some(s) = v["text"].as_str().or_else(|| v["text"]["content"].as_str()) {
            out.push_str(s);
            return;
        }
    }
    for k in [
        "body",
        "content",
        "paragraph",
        "elements",
        "table",
        "tableRows",
        "tableCells",
        "tabs",
        "documentTab",
        "childTabs",
        "pageElements",
        "shape",
        "text",
        "textElements",
        "rich_text",
        "children",
        "heading_1",
        "heading_2",
        "heading_3",
        "bulleted_list_item",
        "numbered_list_item",
        "quote",
        "callout",
        "to_do",
        "code",
    ] {
        if let Some(v) = v.get(k) {
            rich(v, depth + 1, out);
            if matches!(
                k,
                "paragraph"
                    | "heading_1"
                    | "heading_2"
                    | "heading_3"
                    | "bulleted_list_item"
                    | "numbered_list_item"
                    | "quote"
                    | "to_do"
            ) && !out.ends_with('\n')
            {
                out.push('\n');
            }
        }
    }
}
fn sheet_table(v: &Value) -> Option<Value> {
    let values = v["values"].as_array()?;
    if values.is_empty() {
        return None;
    }
    let cols = v["majorDimension"] == "COLUMNS";
    let width = if cols {
        values.len()
    } else {
        values
            .iter()
            .filter_map(Value::as_array)
            .map(Vec::len)
            .max()
            .unwrap_or(0)
    };
    let height = if cols {
        values
            .iter()
            .filter_map(Value::as_array)
            .map(Vec::len)
            .max()
            .unwrap_or(0)
    } else {
        values.len()
    };
    if width == 0 {
        return None;
    }
    let rows = (0..height.min(20))
        .map(|r| {
            (0..width.min(8))
                .map(|c| {
                    let (a, b) = if cols { (c, r) } else { (r, c) };
                    values
                        .get(a)
                        .and_then(|v| v.get(b))
                        .map(display)
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    Some(
        json!({"columns":(0..width.min(8)).map(|n|format!("Column {}",n+1)).collect::<Vec<_>>(),"rows":rows,"truncated":height>20||width>8,"range":v["range"]}),
    )
}
fn adapt(service: &str, v: &Value) -> Option<Value> {
    let spec = profile(service)?;
    if !v.is_object() || matches!(service, "gmail" | "outlook") {
        return None;
    }
    let paths = spec["titles"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let mut title = pick(v, &paths);
    let mut fields = json!({});
    let mut excerpt = String::new();
    let mut html = false;
    if service == "hubspot" {
        let name = [
            pick(v, &["properties/firstname"]),
            pick(v, &["properties/lastname"]),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
        if !name.is_empty() {
            title = name;
        }
    }
    if service == "notion" {
        if let Some(props) = v["properties"].as_object() {
            for (name, p) in props {
                if p["type"] == "title" {
                    let mut text = String::new();
                    rich(&p["title"], 0, &mut text);
                    title = text;
                } else if let Some(typ) = p["type"].as_str() {
                    if matches!(
                        typ,
                        "rich_text"
                            | "select"
                            | "multi_select"
                            | "status"
                            | "date"
                            | "number"
                            | "checkbox"
                            | "email"
                            | "phone_number"
                    ) {
                        field(&mut fields, name, display(&p[typ]));
                    }
                }
            }
        }
    }
    if service == "googlesheets" && title.is_empty() {
        if v["values"].is_array() {
            title = "Sheet range".into();
        } else if v.get("spreadsheetId").is_some() {
            title = pick(v, &["spreadsheetId"]);
        }
    }
    if service == "googlecontacts" && title.is_empty() {
        title = pick(v, &["emailAddresses/0/value"]);
    }
    if service == "airtable" {
        if let Some(fields_value) = v["fields"].as_object() {
            if title.is_empty() {
                title = fields_value
                    .get("Name")
                    .or_else(|| fields_value.get("Title"))
                    .map(display)
                    .unwrap_or_default();
            }
            for (name, value) in fields_value.iter().take(24) {
                let lower = name.to_ascii_lowercase();
                let reserved = matches!(name.as_str(), "Record" | "Created" | "Open source")
                    || lower.contains("password")
                    || lower.contains("token")
                    || lower.contains("secret")
                    || lower.contains("credential")
                    || lower.contains("authorization")
                    || lower.starts_with("api_")
                    || lower.starts_with("access_");
                if !reserved {
                    field(&mut fields, name, display(value));
                }
            }
        }
        if title.is_empty() {
            title = pick(v, &["id"]);
        }
    }
    if title.is_empty() {
        return None;
    }
    for row in spec["fields"].as_array().unwrap() {
        let row = row.as_array().unwrap();
        let paths = row
            .iter()
            .skip(1)
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        field(&mut fields, row[0].as_str().unwrap(), pick(v, &paths));
    }
    match service {
        "googledocs" => rich(v, 0, &mut excerpt),
        "googleslides" => {
            if let Some(slides) = v["slides"].as_array() {
                fields["Slides returned"] = json!(slides.len());
                for (i, slide) in slides.iter().take(8).enumerate() {
                    let mut text = String::new();
                    rich(slide, 0, &mut text);
                    if !text.trim().is_empty() {
                        excerpt.push_str(&format!("Slide {}\n{}\n", i + 1, text.trim()));
                    }
                }
            }
        }
        "googleforms" => {
            if let Some(items) = v["items"].as_array() {
                fields["Items returned"] = json!(items.len());
                excerpt = items
                    .iter()
                    .take(20)
                    .filter_map(|i| i["title"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
            }
        }
        "googlesheets" => {
            if let Some(sheets) = v["sheets"].as_array() {
                field(
                    &mut fields,
                    "Tabs",
                    sheets
                        .iter()
                        .filter_map(|s| s["properties"]["title"].as_str())
                        .take(20)
                        .collect::<Vec<_>>()
                        .join(", "),
                );
            }
        }
        "hubspot" => {
            excerpt = pick(
                v,
                &[
                    "properties/description",
                    "properties/hs_note_body",
                    "properties/hs_task_body",
                ],
            )
        }
        "salesforce" => excerpt = pick(v, &["Description"]),
        "slack" | "googlechat" => {
            excerpt = pick(v, &["text"]);
            title = if service == "slack" {
                "Slack message"
            } else {
                "Google Chat message"
            }
            .into();
        }
        "microsoftteams" => {
            excerpt = pick(v, &["body/content"]);
            html = v["body"]["contentType"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("html"));
            if v["subject"].as_str().unwrap_or("").is_empty() {
                title = "Teams message".into();
            }
        }
        "zoom" => excerpt = pick(v, &["agenda"]),
        "googlecalendar" => {
            excerpt = pick(v, &["description"]);
            html = true;
        }
        "asana" => excerpt = pick(v, &["notes"]),
        "trello" => excerpt = pick(v, &["desc"]),
        "monday" => {
            if let Some(cols) = v["column_values"].as_array() {
                for c in cols.iter().take(16) {
                    let label = pick(c, &["column/title", "title", "id"]);
                    if !label.is_empty() {
                        field(&mut fields, &label, pick(c, &["text"]));
                    }
                }
            }
        }
        "jira" => {
            if v["fields"]["description"].is_string() {
                excerpt = pick(v, &["fields/description"]);
            } else {
                rich(&v["fields"]["description"], 0, &mut excerpt);
            }
        }
        "confluence" => {
            excerpt = pick(v, &["body/storage/value", "body/view/value"]);
            html = true;
            if let (Some(base), Some(path)) =
                (v["_links"]["base"].as_str(), v["_links"]["webui"].as_str())
            {
                if path.starts_with('/') && !path.starts_with("//") {
                    field(
                        &mut fields,
                        "Open source",
                        format!("{}{path}", base.trim_end_matches('/')),
                    );
                }
            }
        }
        "github" => excerpt = pick(v, &["body", "description"]),
        "zendesk" => excerpt = pick(v, &["description"]),
        "linear" => excerpt = pick(v, &["description"]),
        "clickup" => excerpt = pick(v, &["description"]),
        "gitlab" => excerpt = pick(v, &["description"]),
        "todoist" => excerpt = pick(v, &["description"]),
        "stripe" => excerpt = pick(v, &["description", "statement_descriptor"]),
        "dropbox" | "box" => excerpt = pick(v, &["description", "path_display"]),
        "notion" => rich(&v["children"], 0, &mut excerpt),
        _ => {}
    }
    let mut result = json!({"title":short(&title,300),"kind":family(service),"fields":fields,"excerpt":short(&excerpt,16000),"excerpt_html":html,"lines":[]});
    if service == "googlesheets" {
        if let Some(table) = sheet_table(v) {
            result["table"] = table;
        }
    }
    if service == "quickbooks" {
        result["lines"]=json!(v["Line"].as_array().into_iter().flatten().filter(|l|l["DetailType"]=="SalesItemLineDetail").take(30).map(|l|json!({"description":l["Description"],"quantity":l["SalesItemLineDetail"]["Qty"],"rate":l["SalesItemLineDetail"]["UnitPrice"],"amount":l["Amount"]})).collect::<Vec<_>>());
    }
    if service == "github" && v.get("number").is_some() {
        result["kind"] = json!("task");
    }
    Some(result)
}
pub fn extract(service: &str, value: &Value) -> Vec<Value> {
    fn walk(service: &str, v: &Value, depth: usize, out: &mut Vec<Value>) {
        if depth > 10 || out.len() >= 30 {
            return;
        }
        if let Some(s) = v.as_str() {
            if s.len() <= 200_000 {
                if let Ok(v) = serde_json::from_str::<Value>(s) {
                    walk(service, &v, depth + 1, out);
                }
            }
            return;
        }
        if let Some(a) = v.as_array() {
            for v in a.iter().take(30) {
                walk(service, v, depth + 1, out);
            }
            return;
        }
        if v["type"] == "text" {
            if let Some(s) = v["text"].as_str() {
                if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                    walk(service, &parsed, depth + 1, out);
                    return;
                }
            }
        }
        if v["isError"] == true || v["successful"] == false || v["success"] == false {
            return;
        }
        if service == "monday" {
            if let Some(items) = v.get("items_page") {
                walk(service, items, depth + 1, out);
                return;
            }
        }
        if let Some(record) = adapt(service, v) {
            out.push(record);
            return;
        }
        for k in [
            "data",
            "result",
            "results",
            "value",
            "items",
            "files",
            "documents",
            "spreadsheets",
            "valueRanges",
            "presentations",
            "events",
            "connections",
            "people",
            "person",
            "tasks",
            "messages",
            "message",
            "spaces",
            "conferenceRecords",
            "meetings",
            "contacts",
            "companies",
            "deals",
            "records",
            "pages",
            "children",
            "issues",
            "nodes",
            "edges",
            "node",
            "boards",
            "items_page",
            "cards",
            "tickets",
            "ticket",
            "Invoice",
            "QueryResponse",
            "invoices",
            "content",
            "text",
            "entries",
        ] {
            if let Some(v) = v.get(k) {
                walk(service, v, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(service, value, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_and_unknown_services_do_not_guess() {
        assert_eq!(
            service("claude.ai Google Contacts", "getPeople"),
            "googlecontacts"
        );
        assert_eq!(service("Microsoft Outlook", "send_email"), "outlook");
        assert_eq!(service("Atlassian", "getJiraIssue"), "jira");
        assert_eq!(
            service("Google Workspace", "GOOGLESHEETS_GET_VALUES"),
            "googlesheets"
        );
        assert_eq!(family("unknown"), "record");
        assert!(extract("unknown", &json!({"name":"plain"})).is_empty());
    }
    #[test]
    fn columns_ragged_and_large_ranges_are_bounded() {
        let rows = extract(
            "googlesheets",
            &json!({"range":"A1:B3","majorDimension":"COLUMNS","values":[["=1+2","b","c"],[42]]}),
        );
        assert_eq!(
            rows[0]["table"]["rows"],
            json!([["=1+2", "42"], ["b", ""], ["c", ""]])
        );
        let large = extract("googlesheets", &json!({"values":vec![vec![1;12];40]}));
        assert_eq!(large[0]["table"]["rows"].as_array().unwrap().len(), 20);
        assert_eq!(large[0]["table"]["columns"].as_array().unwrap().len(), 8);
        assert_eq!(large[0]["table"]["truncated"], true);
    }
    #[test]
    fn wrappers_errors_and_contacts() {
        assert!(extract("slack", &json!({"isError":true,"text":"error"})).is_empty());
        let r = extract(
            "hubspot",
            &json!({"properties":{"firstname":"Casey","lastname":"Morgan","email":"casey@example.invalid"}}),
        );
        assert_eq!(r[0]["title"], "Casey Morgan");
        let r = extract("asana", &json!({"data":vec![json!({"name":"Task"});25]}));
        assert_eq!(r.len(), 25);
        assert_eq!(
            extract("asana", &json!({"data":vec![json!({"name":"Task"});50]})).len(),
            30
        );
        assert_eq!(
            extract("asana", &json!({"data":vec![json!({"name":"Task"});15]})).len(),
            15
        );
    }
    #[test]
    fn additional_provider_shapes_preserve_source_fields() {
        let linear = extract(
            "linear",
            &json!({"issues":{"nodes":[{"identifier":"OPS-7","title":"Policy review","description":"Read the supplied note.","status":{"name":"Todo"},"assignee":{"name":"Casey"}}]}}),
        );
        assert_eq!(linear[0]["title"], "Policy review");
        assert_eq!(linear[0]["fields"]["Assignee"], "Casey");
        let airtable = extract(
            "airtable",
            &json!({"records":[{"id":"rec-1","fields":{"Name":"Vendor","Status":"Open","Count":4}}]}),
        );
        assert_eq!(airtable[0]["title"], "Vendor");
        assert_eq!(airtable[0]["fields"]["Count"], "4");
        let gitlab = extract(
            "gitlab",
            &json!([{"iid":7,"title":"Issue","state":"opened","assignees":[{"name":"Casey"}],"web_url":"https://example.invalid/i/7"}]),
        );
        assert_eq!(gitlab[0]["fields"]["Assignees"], "Casey");
        let stripe = extract(
            "stripe",
            &json!({"data":[{"id":"pi_1","description":"Review","amount":4200,"status":"succeeded"}]}),
        );
        assert_eq!(
            stripe[0]["fields"]["Amount (smallest currency unit)"],
            "4200"
        );
        let clickup = extract(
            "clickup",
            &json!({"tasks":[{"id":"cu-1","name":"Task","assignees":[{"username":"casey"}]}]}),
        );
        assert_eq!(clickup[0]["fields"]["Assignees"], "casey");
        let dropbox = extract(
            "dropbox",
            &json!({"entries":[{"name":"runbook.pdf","path_display":"/runbook.pdf","size":12}]}),
        );
        assert_eq!(dropbox[0]["title"], "runbook.pdf");
        let bx = extract(
            "box",
            &json!({"entries":[{"name":"evidence.txt","type":"file","size":9}]}),
        );
        assert_eq!(bx[0]["title"], "evidence.txt");
        let safe = extract(
            "airtable",
            &json!({"records":[{"id":"rec-1","fields":{"Name":"Vendor","Record":"spoof","api_token":"secret"}}]}),
        );
        assert_eq!(safe[0]["fields"]["Record"], "rec-1");
        assert!(safe[0]["fields"].get("api_token").is_none());
    }
}
