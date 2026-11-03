//! Small provider-neutral capability index. Execution still uses the ordinary
//! runtime gates, receipts and approvals; discovery never grants permissions.
use crate::{db::{Bot, Run}, runtime::App};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

fn core(name: &str) -> bool {
    matches!(name, "set_task_title" | "task_progress" | "finish_quietly" |
        "kindred_guide" | "remember" | "memory_read" | "memory_compact" |
        "computer_screenshot" | "computer_click" | "computer_type" |
        "computer_key" | "computer_scroll" | "computer_open_url" |
        "guest_exec" | "ask_question" | "request_user_action" | "routine_complete")
}

pub fn available(app: &App, bot: &Bot, run: &Run) -> Vec<Value> {
    let mut specs = crate::runtime::tool_specs_for_run(app, bot, run);
    if crate::voice::session::is_run(&app.db.0.lock().unwrap(), &run.id).unwrap_or(false) {
        for spec in &mut specs { if spec["name"]=="send_to_bot" { spec["description"]=json!("Ask another teammate only when the caller explicitly requests collaboration. Never delegate ordinary call lookups to your chat model. This waits for their result and returns it to the call."); } }
        specs.retain(|s| !matches!(s["name"].as_str(), Some("ask_question" | "set_task_title" | "task_progress" | "finish_quietly" | "command_wait")));
    }
    specs
}

pub fn advertised(app: &App, bot: &Bot, run: &Run) -> Vec<Value> {
    let specs = available(app, bot, run);
    let index = specs.iter().filter(|s| !core(s["name"].as_str().unwrap_or("")))
        .map(|s| s["name"].as_str().unwrap_or("")).collect::<Vec<_>>().join(", ");
    let mut tools: Vec<_> = specs.into_iter().filter(|s| core(s["name"].as_str().unwrap_or(""))).collect();
    tools.push(json!({"type":"function","name":"tool_describe","description":format!("Read complete schemas before calling tool_execute. Choose up to four exact names from this capability index: {index}. Read-only; no model or external action."),"inputSchema":{"type":"object","properties":{"names":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":4}},"required":["names"],"additionalProperties":false}}));
    tools.push(json!({"type":"function","name":"tool_execute","description":"Execute one tool after reading its schema with tool_describe. Use its exact name and arguments. Existing permissions, approvals and ownership still apply. Never repeat an action with an unknown outcome.","inputSchema":{"type":"object","properties":{"name":{"type":"string"},"arguments":{"type":"object"}},"required":["name","arguments"],"additionalProperties":false}}));
    tools
}

pub fn describe(app: &App, bot: &Bot, run: &Run, args: &Value) -> Result<Value> {
    let names = args["names"].as_array().ok_or_else(|| anyhow::anyhow!("Provide names as an array of exact tool names."))?;
    ensure!(!names.is_empty() && names.len() <= 4, "Request between one and four tools.");
    let specs = available(app, bot, run);
    let selected: Vec<_> = names.iter().map(|name| specs.iter().find(|s| s["name"] == *name)
        .cloned().ok_or_else(|| anyhow::anyhow!("Tool unavailable in this task: {name}"))).collect::<Result<_>>()?;
    Ok(json!({"text":json!({"tools":selected,"call_with":"tool_execute"}).to_string()}))
}

pub fn resolve(app: &App, bot: &Bot, run: &Run, args: Value) -> Result<(String, Value)> {
    let name = args["name"].as_str().ok_or_else(|| anyhow::anyhow!("Provide the exact tool name."))?;
    let spec = available(app, bot, run).into_iter().find(|s| s["name"] == name)
        .ok_or_else(|| anyhow::anyhow!("Tool unavailable in this task: {name}"))?;
    crate::composio::validate_arguments(&spec["inputSchema"], &args["arguments"])?;
    Ok((name.to_owned(), args["arguments"].clone()))
}

// Provider fixtures use the public deferred-call envelope, not hidden SDK tools.
#[cfg(test)]
pub fn fixture_function(name: &str, args: Value) -> Value {
    if core(name) { json!({"name":name,"arguments":args.to_string()}) }
    else { json!({"name":"tool_execute","arguments":json!({"name":name,"arguments":args}).to_string()}) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_scoped_capability_is_retrievable_and_compact_across_providers() {
        let app = crate::tests::app();
        for provider in ["codex", "claude-code", "openrouter", "custom-00000000-0000-4000-8000-000000000001"] {
            let bot = crate::tests::bot(&app.db, provider);
            let run = app.db.run(&app.db.queue(&bot.id, "Review my work", 0).unwrap()).unwrap();
            let all = available(&app, &bot, &run);
            let compact = advertised(&app, &bot, &run);
            assert!(json!(compact).to_string().len() < json!(all).to_string().len() * 2 / 3);
            for spec in all {
                let result = describe(&app, &bot, &run, &json!({"names":[spec["name"]]})).unwrap();
                let result: Value = serde_json::from_str(result["text"].as_str().unwrap()).unwrap();
                assert_eq!(result["tools"][0], spec);
            }
            assert!(resolve(&app, &bot, &run, json!({"name":"tool_execute","arguments":{}})).is_err());
            assert!(resolve(&app, &bot, &run, json!({"name":"remember","arguments":{}})).is_err());
            if provider != "codex" {
                assert!(describe(&app, &bot, &run, &json!({"names":["codex_connector"]})).is_err());
            }
        }
    }
    #[tokio::test]
    async fn execution_has_original_receipts_and_validation_before_effects() {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "codex");
        let run = app.db.run(&app.db.queue(&bot.id, "Remember my preference", 0).unwrap()).unwrap();
        let result = crate::runtime::call_tool(&app, &bot, &run, "tool_execute", json!({"name":"remember","arguments":{"text":"Use concise answers"}})).await.unwrap();
        assert_ne!(result["failed"], true);
        assert!(app.db.bot(&bot.id).unwrap().memory.contains("Use concise answers"));
        let events = app.db.events(&run.id).unwrap();
        assert_eq!(events.iter().filter(|e| e["kind"] == "tool_requested" && e["body"]["tool"] == "remember").count(), 1);
    }
}
