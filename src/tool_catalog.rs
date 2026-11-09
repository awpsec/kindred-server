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
    tools.push(json!({"type":"function","name":"tool_describe","description":format!("Read schemas before tool_execute. Example: {{\"names\":[\"chat_read\",\"artifact_list\"]}}. Request at most four at a time; additional names return in remaining. Capability index: {index}. Read-only; no model or external action."),"inputSchema":{"type":"object","properties":{"names":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":4}},"required":["names"],"additionalProperties":false}}));
    tools.push(json!({"type":"function","name":"tool_execute","description":"Execute one tool after reading its schema with tool_describe. Use its exact name and arguments. Existing permissions, approvals and ownership still apply. Never repeat an action with an unknown outcome.","inputSchema":{"type":"object","properties":{"name":{"type":"string"},"arguments":{"type":"object"}},"required":["name","arguments"],"additionalProperties":false}}));
    tools
}

pub fn describe(app: &App, bot: &Bot, run: &Run, args: &Value) -> Result<Value> {
    // Recover read-only discovery formatting mistakes. Execution remains exact,
    // independently validated and permission-gated; no aliases reach resolve().
    let input = args.get("names").or_else(||args.get("name")).or_else(||args.get("query"))
        .ok_or_else(||anyhow::anyhow!("Use names:[\"chat_read\"] to read tool schemas."))?;
    let names: Vec<String> = if let Some(items)=input.as_array() {
        ensure!(items.len()<=64,"Request at most 64 names; schemas are returned four at a time.");
        items.iter().map(|v|v.as_str().map(str::to_owned).ok_or_else(||anyhow::anyhow!("Tool names must be strings"))).collect::<Result<_>>()?
    } else if let Some(text)=input.as_str() {
        ensure!(text.len()<=4096,"Keep discovery requests under 4096 bytes");
        text.split(|c:char|!c.is_ascii_alphanumeric() && c!='_').filter(|s|!s.is_empty()).map(str::to_owned).collect()
    } else {anyhow::bail!("Use names as an array of exact tool names");};
    ensure!(!names.is_empty() && names.len()<=64,"Request one to 64 names; schemas are returned four at a time.");
    let specs = available(app, bot, run);
    let mut selected=Vec::new();let mut remaining=Vec::new();let mut unavailable=Vec::new();let mut corrections=Vec::new();let mut seen=std::collections::HashSet::new();
    for requested in names {
        ensure!(requested.len()<=160,"Tool name is too long");
        let name=if requested=="artifacts_list" {"artifact_list"} else {&requested};
        if !seen.insert(name.to_owned()) {continue;}
        if let Some(spec)=specs.iter().find(|s|s["name"]==name) {
            if name!=requested {corrections.push(json!({"requested":requested,"name":name}));}
            if selected.len()<4 {selected.push(spec.clone());}else{remaining.push(name.to_owned());}
        } else {unavailable.push(requested);}
    }
    ensure!(!selected.is_empty(),"No matching tools in this task. Use exact names from the capability index with names:[...]. Claude account tools (mcp__claude_ai_...) use Claude ToolSearch, not Kindred tool_describe.");
    Ok(json!({"text":json!({"tools":selected,"remaining":remaining,"unavailable":unavailable,"corrections":corrections,"call_with":"tool_execute"}).to_string()}))
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
    #[test]
    fn discovery_recovers_shapes_typos_and_large_batches_without_execution_aliases() {
        let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"claude-code");
        let run=app.db.run(&app.db.queue(&bot.id,"Read schemas",0).unwrap()).unwrap();
        for args in [json!({"name":"chat_read"}),json!({"query":"[\"chat_read\"]"}),json!({"names":"chat_read"}),json!({"names":["chat_read","chat_read"]})] {
            let out=describe(&app,&bot,&run,&args).unwrap();let v:Value=serde_json::from_str(out["text"].as_str().unwrap()).unwrap();
            assert_eq!(v["tools"].as_array().unwrap().len(),1);assert_eq!(v["tools"][0]["name"],"chat_read");
        }
        let out=describe(&app,&bot,&run,&json!({"names":["artifact_read","artifacts_list","chat_read","chat_post","routines_list","codex_connector"]})).unwrap();
        let v:Value=serde_json::from_str(out["text"].as_str().unwrap()).unwrap();
        assert_eq!(v["tools"].as_array().unwrap().len(),4);assert_eq!(v["remaining"],json!(["routines_list"]));
        assert_eq!(v["unavailable"],json!(["codex_connector"]));assert_eq!(v["corrections"][0]["name"],"artifact_list");
        assert!(resolve(&app,&bot,&run,json!({"name":"artifacts_list","arguments":{}})).is_err());
        assert!(describe(&app,&bot,&run,&json!({"names":[1]})).is_err());
        assert!(describe(&app,&bot,&run,&json!({"query":"x".repeat(4097)})).is_err());
        assert!(!app.db.events(&run.id).unwrap().iter().any(|e|e["kind"]=="tool_started"));
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
