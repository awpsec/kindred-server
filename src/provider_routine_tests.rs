//! Synthetic provider transports exercise the real tool dispatcher and durable
//! routine publication path. No provider accounts, network calls or VM are used.
use super::*;
use crate::{db, routine_completion, tests};

fn scheduled(app: &App, provider: &str) -> (Bot, Run) {
    let bot = tests::bot(&app.db, provider);
    let routine = db::Routine {
        id: db::id(), bot_id: bot.id.clone(), name: "Synthetic check".into(),
        prompt: "Check synthetic state and classify the result".into(),
        interval_seconds: 86400, next_run: db::now() + 86400,
        enabled: true, run_at: None, schedule: None,
    };
    app.db.save_routine(&routine).unwrap();
    app.db.run_routine_now(&routine.id).unwrap();
    let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
    assert_eq!(routine_completion::root(&app.db.0.lock().unwrap(), &run.id).unwrap(), Some(run.id.clone()));
    (bot, run)
}

const SCRIPT: &str = r#"
import json,sys
def read():
 line=sys.stdin.readline()
 if not line:sys.exit(0)
 return json.loads(line)
def send(value):print(json.dumps(value),flush=True)
calls=0
if provider=='codex':
 while True:
  value=read();method=value.get('method')
  if method=='initialized':continue
  if method=='initialize':result={}
  elif method=='account/read':result={'account':{'type':'chatgpt'}}
  elif method=='model/list':result={'data':[{'model':'test/model','isDefault':True,'defaultReasoningEffort':'medium','supportedReasoningEfforts':[{'reasoningEffort':'medium'}]}],'nextCursor':None}
  elif method=='config/read':result={'config':{}}
  elif method=='thread/start':
   names=[tool['name'] for tool in value['params']['dynamicTools']]
   assert 'finish_quietly' in names and 'routine_complete' in names
   result={'thread':{'id':'routine-fixture'}}
  elif method=='app/installed':result={'apps':[]}
  elif method=='mcpServerStatus/list':result={'data':[],'nextCursor':None}
  elif method=='turn/start':
   send({'id':value['id'],'result':{'turn':{'id':'routine-turn'}}});break
  else:raise AssertionError('Unexpected RPC frame: '+str(value))
  send({'id':value['id'],'result':result})
else:
 start=read()
 assert start['provider']==provider
 names=[tool['name'] for tool in start['tools']]
 assert 'finish_quietly' in names and 'routine_complete' in names
 send({'type':'ready','protocol':1,'provider':provider,'model':start['model'],
       'reasoning_effort':start['reasoning_effort'],'sdk_version':'0.85.1'})
def tool(name,args,terminal=False):
 global calls
 calls+=1;identity='tool-'+str(calls)
 if provider=='codex':send({'id':identity,'method':'item/tool/call','params':{'tool':name,'arguments':args}})
 else:send({'type':'tool_call','id':identity,'name':name,'args':args})
 reply=read()
 assert not terminal,'A persisted completion or decision must stop before another model step'
 assert reply['id']==identity
 if provider=='codex':return {'failed':not reply['result']['success'],'text':reply['result']['contentItems'][0]['text']}
 return reply['result']
def commentary():
 if provider=='codex':send({'method':'item/completed','params':{'item':{'type':'agentMessage','phase':'commentary','text':'PRIVATE_ROUTINE_SCRATCH'}}})
 else:send({'type':'assistant','text':'PRIVATE_ROUTINE_SCRATCH','phase':'commentary'})
def empty():
 if provider=='codex':
  send({'method':'item/completed','params':{'item':{'type':'agentMessage','phase':'final_answer','text':''}}})
  send({'method':'turn/completed','params':{'turn':{'id':'routine-turn','status':'completed'}}})
 else:send({'type':'complete','output':''})
if mode in ('finish_quietly','routine_quiet'):
 commentary()
 assert not tool('commands_list',{}).get('failed')
 if mode=='finish_quietly':tool('finish_quietly',{},True)
 else:tool('routine_complete',{'outcome':'quiet'},True)
elif mode=='decision':
 commentary()
 tool('ask_question',{'topic_key':'synthetic-routine-choice','question':'Continue this check?',
      'context':'Synthetic check needs your choice.','options':['Continue','Leave it']},True)
elif mode=='empty':empty()
elif mode=='provider_notice':
 assert provider=='claude-code'
 send({'type':'assistant','is_error':True,'text':'Synthetic Claude connector access was denied.'})
 quiet=tool('finish_quietly',{})
 assert quiet['failed'] and 'report it instead' in quiet['text']
 empty()
else:
 failed=tool('command_read',{'name':'missing-fixture-command'})
 assert failed['failed'] and 'Command not found' in failed['text']
 if mode=='reported_error':
  tool('routine_complete',{'outcome':'error','report':'The saved command is unavailable; restore it before retrying.'},True)
 else:
  quiet=tool('finish_quietly',{})
  assert quiet['failed'] and 'report it instead' in quiet['text']
  if mode=='failed_empty':empty()
  elif mode=='provider_error':
   if provider=='codex':send({'method':'turn/completed','params':{'turn':{'id':'routine-turn','status':'failed','error':{'message':'Synthetic context capacity exceeded','codexErrorInfo':'contextWindowExceeded'}}}})
   elif provider=='claude-code':send({'type':'error','code':'provider_configuration'})
   else:send({'type':'error','code':'provider_http','status':403})
  else:raise AssertionError('Unexpected scenario: '+mode)
"#;

async fn protocol(app: &App, bot: &Bot, run: &Run, mode: &str) -> Result<String> {
    let script = format!("provider={}\nmode={}\n{SCRIPT}", json!(bot.provider), json!(mode));
    let work = async {
        match bot.provider.as_str() {
            "codex" => {
                let mut command = tokio::process::Command::new("python3");
                command.args(["-u", "-c", &script]);
                let mut rpc = Rpc::spawn(command).await?;
                codex_rpc(app, bot, run, &mut rpc).await
            }
            "claude-code" => crate::cli_providers::routine_protocol_fixture(app, bot, run, &script).await,
            "openrouter" => crate::pi::routine_protocol_fixture(app, bot, run, &script).await,
            _ => unreachable!(),
        }
    };
    tokio::time::timeout(Duration::from_secs(8), work).await.context("Routine protocol fixture timed out")?
}

fn finish(app: &App, run: &Run, result: Result<String>) {
    match result {
        Ok(output) => app.db.finish(&run.id, "completed", &output, "").unwrap(),
        Err(error) => app.db.finish(&run.id, "failed", "", &error.to_string()).unwrap(),
    }
    app.db.event(&run.id, "run_finished", json!({})).unwrap();
    app.db.chat_complete(run).unwrap();
    app.db.chat_complete(run).unwrap(); // Publication remains idempotent.
}

#[tokio::test]
async fn routine_provider_protocols_quiet_completion_has_no_bubble_or_alert() {
    for provider in ["codex", "claude-code", "openrouter"] {
        for mode in ["finish_quietly", "routine_quiet"] {
            let app = tests::app();
            let (bot, run) = scheduled(&app, provider);
            let before = app.db.chat_messages(&run.chat_id).unwrap();
            let last_message = app.db.chat(&run.chat_id).unwrap().last_message;
            let attention = app.db.attention().unwrap()["chats"][&run.chat_id].clone();
            let cursor = app.db.notifications(None).unwrap()["cursor"].as_i64().unwrap();
            let result = protocol(&app, &bot, &run, mode).await;
            assert_eq!(result.as_ref().unwrap(), "", "{provider}/{mode}");
            finish(&app, &run, result);
            let saved = app.db.run(&run.id).unwrap();
            assert_eq!(saved.status, "completed", "{provider}/{mode}");
            assert!(saved.output.is_empty() && saved.error.is_empty());
            assert_eq!(app.db.chat_messages(&run.chat_id).unwrap(), before);
            assert_eq!(app.db.chat(&run.chat_id).unwrap().last_message, last_message);
            assert_eq!(app.db.attention().unwrap()["chats"][&run.chat_id], attention);
            assert!(app.db.notifications(Some(cursor)).unwrap()["items"].as_array().unwrap().is_empty());
            let c = app.db.0.lock().unwrap();
            let outcome: String = c.query_row("SELECT outcome FROM routine_completion WHERE run_id=?", [&run.id], |r| r.get(0)).unwrap();
            assert_eq!(outcome, "quiet");
        }
    }
}

#[tokio::test]
async fn routine_provider_protocols_failed_tool_keeps_original_reason_after_refused_quiet() {
    for provider in ["codex", "claude-code", "openrouter"] {
        let app = tests::app();
        let (bot, run) = scheduled(&app, provider);
        let cursor = app.db.notifications(None).unwrap()["cursor"].as_i64().unwrap();
        let result = protocol(&app, &bot, &run, "failed_empty").await;
        let error = result.as_ref().unwrap_err().to_string();
        assert!(error.contains("command_read") && error.contains("Command not found"), "{provider}: {error}");
        assert!(!error.contains("report it instead"), "The later quiet refusal must not replace the original failure");
        finish(&app, &run, result);
        let saved = app.db.run(&run.id).unwrap();
        assert_eq!(saved.status, "failed");
        assert_eq!(saved.error, error);
        let messages = app.db.chat_messages(&run.chat_id).unwrap();
        assert_eq!(messages.iter().filter(|m| m["kind"] == "result" && m["text"] == error).count(), 1);
        assert_eq!(app.db.notifications(Some(cursor)).unwrap()["items"].as_array().unwrap().len(), 1);
        let c = app.db.0.lock().unwrap();
        assert_eq!(c.query_row("SELECT count(*) FROM routine_completion WHERE run_id=?", [&run.id], |r| r.get::<_,i64>(0)).unwrap(), 0);
        assert!(!c.query_row("SELECT quiet FROM routine_runs WHERE run_id=?", [&run.id], |r| r.get::<_,bool>(0)).unwrap());
    }
}

#[tokio::test]
async fn routine_provider_protocols_unclassified_empty_is_never_quiet_success() {
    for provider in ["codex", "claude-code", "openrouter"] {
        let app = tests::app();
        let (bot, run) = scheduled(&app, provider);
        let result = protocol(&app, &bot, &run, "empty").await;
        assert!(result.as_ref().unwrap_err().to_string().contains("without a result"));
        finish(&app, &run, result);
        assert_eq!(app.db.run(&run.id).unwrap().status, "failed");
        assert_eq!(app.db.chat_messages(&run.chat_id).unwrap().iter().filter(|m| m["kind"] == "result").count(), 1);
    }
}

#[tokio::test]
async fn routine_provider_protocols_preserve_provider_failures_and_structured_errors() {
    for provider in ["codex", "claude-code", "openrouter"] {
        for mode in ["provider_error", "reported_error"] {
            let app = tests::app();
            let (bot, run) = scheduled(&app, provider);
            let result = protocol(&app, &bot, &run, mode).await;
            let expected = if mode == "reported_error" {
                assert_eq!(result.as_ref().unwrap(), "");
                "The saved command is unavailable; restore it before retrying."
            } else {
                assert!(result.is_err());
                match provider {
                    "codex" => "Synthetic context capacity exceeded",
                    "claude-code" => "selected subscription model",
                    _ => "OpenRouter returned HTTP 403",
                }
            };
            finish(&app, &run, result);
            let saved = app.db.run(&run.id).unwrap();
            assert_eq!(saved.status, "failed");
            assert!(saved.error.contains(expected), "{provider}/{mode}: {}", saved.error);
            assert!(!saved.error.contains("command_read"));
        }
    }
}

#[tokio::test]
async fn routine_provider_protocols_pending_choices_remain_visible() {
    for provider in ["codex", "claude-code", "openrouter"] {
        let app = tests::app();
        let (bot, run) = scheduled(&app, provider);
        let result = protocol(&app, &bot, &run, "decision").await;
        assert_eq!(result.as_ref().unwrap(), "");
        finish(&app, &run, result);
        assert_eq!(app.db.run(&run.id).unwrap().status, "completed");
        assert_eq!(app.db.pending_questions().unwrap().len(), 1);
        let messages = app.db.chat_messages(&run.chat_id).unwrap();
        assert_eq!(messages.iter().filter(|m| m["kind"] == "question").count(), 1);
        assert!(!messages.iter().any(|m| m["kind"] == "result" || m["text"] == "PRIVATE_ROUTINE_SCRATCH"));
        assert!(routine_completion::complete(&app.db.0.lock().unwrap(), &run.id, &json!({"outcome":"quiet"})).is_err());
    }
}

#[tokio::test]
async fn routine_provider_protocols_claude_error_notice_cannot_finish_quietly() {
    let app = tests::app();
    let (bot, run) = scheduled(&app, "claude-code");
    let result = protocol(&app, &bot, &run, "provider_notice").await;
    assert!(result.as_ref().unwrap_err().to_string().contains("Synthetic Claude connector access was denied."));
    finish(&app, &run, result);
    assert_eq!(app.db.run(&run.id).unwrap().status, "failed");
    assert_eq!(app.db.chat_messages(&run.chat_id).unwrap().iter().filter(|m| m["kind"] == "result").count(), 1);
    let c = app.db.0.lock().unwrap();
    assert_eq!(c.query_row("SELECT count(*) FROM routine_completion WHERE run_id=?", [&run.id], |r| r.get::<_,i64>(0)).unwrap(), 0);
}

#[test]
fn routine_empty_diagnostic_is_bounded_scoped_and_prefers_work_failure() {
    let app = tests::app();
    let (_bot, run) = scheduled(&app, "codex");
    let (other_bot, other) = scheduled(&app, "codex");
    app.db.event(&other.id, "tool_result", json!({"tool":"private_other_tool","failed":true,"text":"OTHER_RUN_PRIVATE_FAILURE"})).unwrap();
    assert!(routine_empty_error(&app, &run).unwrap().is_none());
    app.db.event(&run.id, "tool_result", json!({"tool":"connector_tools_list","failed":true,"text":format!("The schema exceeds the size limit. {}", "界".repeat(4000))})).unwrap();
    app.db.event(&run.id, "tool_result", json!({"tool":"finish_quietly","failed":true,"text":"Routine cannot finish quietly after an error"})).unwrap();
    let error = routine_empty_error(&app, &run).unwrap().unwrap();
    assert!(error.contains("connector_tools_list") && error.contains("schema exceeds the size limit"));
    assert!(!error.contains("OTHER_RUN") && !error.contains("Routine cannot finish"));
    assert!(error.len() <= 2240);
    let direct = app.db.queue(&other_bot.id, "Interactive task", 0).unwrap();
    app.db.event(&direct, "tool_result", json!({"tool":"connector_tools_list","failed":true,"text":"Interactive failure"})).unwrap();
    assert!(routine_empty_error(&app, &app.db.run(&direct).unwrap()).unwrap().is_none());
    let (_bot, completed) = scheduled(&app, "codex");
    routine_completion::complete(&app.db.0.lock().unwrap(), &completed.id, &json!({"outcome":"error","report":"Explicit routine error"})).unwrap();
    app.db.event(&completed.id, "tool_result", json!({"tool":"connector_tools_list","failed":true,"text":"Late tool error"})).unwrap();
    assert!(routine_empty_error(&app, &completed).unwrap().is_none());
}
