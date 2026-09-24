use super::*;
use std::time::Duration;

async fn peer(mode: &str, log: &std::path::Path) -> Rpc {
    let script = r#"
import sys,json
mode,log=sys.argv[1:]; reads=0
def send(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
 v=json.loads(line); m=v.get('method'); p=v.get('params',{})
 with open(log,'a') as f: f.write(json.dumps(v)+'\n')
 if m=='initialize': result={}
 elif m=='initialized': continue
 elif m=='account/read':
  reads+=1
  result={'account':{'type':'chatgpt','email':'changed@example.invalid' if mode=='account' and reads>=3 else 'fixture@example.invalid'}}
 elif m=='thread/start':
  assert p['ephemeral'] and 'dynamicTools' not in p and 'apps' not in p['config']
  result={'thread':{'id':'aux'}}
 elif m=='app/list':
  send({'id':v['id'],'error':{'code':-32000,'message':'failed to list apps: Request failed with status 403 Forbidden: <html><script>window._cf_chl_opt={secret:"fixture-challenge-token"}</script></html>'}});continue
 elif m=='app/installed':
  assert p['forceRefresh'] and p['threadId']=='aux'
  if mode=='installed-error':
   send({'id':v['id'],'error':{'code':-32000,'message':'Request failed with status 403 Forbidden: <html><script>window._cf_chl_opt={secret:"fixture-challenge-token"}</script></html>'}});continue
  result={'apps':[{'id':'app-mail','runtimeName':'Gmail runtime','enabled':mode!='disabled','callable':not(mode=='uninstalled' and reads>=3)}]}
  if mode=='empty':result['apps']=[]
  if mode=='many':result['apps']=[{'id':'app-'+str(n),'runtimeName':'App '+str(n),'enabled':True,'callable':True} for n in range(101)]
  if mode=='duplicate':result['apps']*=2
 elif m=='mcpServerStatus/list':
  schema={'type':'object','properties':{'to':{'type':'string'},'subject':{'type':'string'},'body':{'type':'string'}},'required':['to','subject','body']}
  if mode=='schema' and reads>=3: schema['required'].append('extra'); schema['properties']['extra']={'type':'string'}
  result={'data':[{'name':'codex_apps','authStatus':'oAuth','runtimeStatus':'connected','tools':{'send_email':{'name':'send_email','inputSchema':schema,'annotations':{'readOnlyHint':False}}}}],'nextCursor':None}
  if mode=='cursor': result['nextCursor']='same'
 elif m=='app/read':
  assert 0<len(p['appIds'])<=100 and p['includeTools']
  if mode=='metadata-error':
   send({'id':v['id'],'error':{'code':-32000,'message':'Request failed with status 403 Forbidden: <html><script>window._cf_chl_opt={secret:"fixture-challenge-token"}</script></html>'}});continue
  result={'apps':[{'id':'app-mail','name':'Gmail','toolSummaries':[{'name':'send_email','isEnabled':True,'isReadOnly':False}]}],'missingAppIds':[]}
  if mode=='missing':result={'apps':[],'missingAppIds':p['appIds']}
  if mode=='ambiguous': result['apps'].append({'id':'another-app','toolSummaries':[{'name':'send_email','isEnabled':True}]})
 elif m=='mcpServer/tool/call':
  assert p['threadId']=='aux' and p['server']=='codex_apps' and p['tool']=='send_email'
  if mode=='challenge':
   send({'id':'native-approval','method':'item/tool/requestUserInput','params':{'questions':[]}}); continue
  if mode=='disconnect': break
  result={'content':[{'type':'text','text':'fixture receipt'}],'isError':mode=='error','structuredContent':{'id':'fixture-receipt'}}
 elif v.get('id')=='native-approval':
  assert 'error' in v; break
 else: raise AssertionError('Forbidden or unexpected method: '+str(m))
 send({'id':v['id'],'result':result})
"#;
    let mut command = tokio::process::Command::new("python3");
    command.args(["-u", "-c", script, mode, log.to_str().unwrap()]);
    Rpc::spawn(command).await.unwrap()
}
fn args() -> Value {
    json!({"app_id":"app-mail","server":"codex_apps","tool_name":"send_email","account_key":fingerprint(&json!({"type":"chatgpt","email":"fixture@example.invalid"})).unwrap(),"arguments":{"to":"recipient@example.invalid","subject":"Fixture draft","body":"Original"}})
}
async fn pending(app: &App) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(a) = app.db.approvals().unwrap().first() {
                break a.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
fn frames(log: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}

#[tokio::test]
async fn codex_gateway_reviews_edits_once_revalidates_and_preserves_failures() {
    for mode in [
        "success",
        "deny",
        "schema",
        "account",
        "uninstalled",
        "cancel",
        "provider",
        "error",
        "challenge",
        "disconnect",
    ] {
        let app = crate::tests::app();
        let mut bot = crate::tests::bot(&app.db, "codex");
        bot.approval_mode = "full".into();
        app.db.save_bot(&bot).unwrap();
        let id = app.db.queue(&bot.id, "Fixture connector task", 0).unwrap();
        app.db.claim().unwrap();
        let run = app.db.run(&id).unwrap();
        let log = std::env::temp_dir().join(format!("kindred-codex-{}.jsonl", crate::db::id()));
        let mut rpc = peer(mode, &log).await;
        let (a, b, r) = (app.clone(), bot.clone(), run.clone());
        let task = tokio::spawn(async move { execute_rpc(&a, &b, &r, &args(), &mut rpc).await });
        let approval = pending(&app).await;
        assert_eq!(approval["tool"], "codex_connector");
        assert_eq!(approval["args"]["origin"], "codex-account");
        assert_eq!(approval["args"]["forced"], true);
        assert!(
            !frames(&log)
                .iter()
                .any(|v| v["method"] == "mcpServer/tool/call")
        );
        let artifact = approval["args"]["artifact_id"].as_str().unwrap();
        let mut card = connector_artifacts::record(&app.db.0.lock().unwrap(), artifact).unwrap();
        assert_eq!(card["source"], "Codex");
        assert!(
            connector_artifacts::update(
                &app,
                artifact,
                &json!({"action":"approve","revision":card["revision"],"choice":"always_allow"})
            )
            .is_err()
        );
        if mode == "success" {
            connector_artifacts::update(&app,artifact,&json!({"action":"edit","revision":card["revision"],"fields":{"body":"Edited by person"}})).unwrap();
            card = connector_artifacts::record(&app.db.0.lock().unwrap(), artifact).unwrap();
        }
        if mode == "cancel" {
            app.db.cancel(&run.id).unwrap();
        } else {
            if mode == "provider" {
                bot.provider = "claude-code".into();
                app.db.save_bot(&bot).unwrap();
            }
            connector_artifacts::update(&app,artifact,&json!({"action":if mode=="deny"{"deny"}else{"approve"},"revision":card["revision"]})).unwrap();
        }
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        let sent = frames(&log);
        assert!(!sent.iter().any(|v| v["method"] == "app/list"));
        let calls: Vec<_> = sent
            .iter()
            .filter(|v| v["method"] == "mcpServer/tool/call")
            .collect();
        let dispatched = matches!(mode, "success" | "error" | "challenge" | "disconnect");
        assert_eq!(calls.len(), usize::from(dispatched), "{mode}");
        assert!(!sent.iter().any(|v| v["method"] == "turn/start"));
        if mode == "success" {
            assert_eq!(calls[0]["params"]["arguments"]["body"], "Edited by person");
            assert_eq!(result.unwrap()["failed"], false);
        } else {
            assert!(
                result.as_ref().map_or(true, |v| v["failed"] == true),
                "{mode}: {result:?}"
            );
        }
        if mode != "cancel" {
            let card = connector_artifacts::record(&app.db.0.lock().unwrap(), artifact).unwrap();
            assert!(
                !matches!(
                    card["status"].as_str(),
                    Some("pending" | "ready" | "executing")
                ),
                "{mode}"
            );
        }
        std::fs::remove_file(log).unwrap();
    }
}

#[tokio::test]
async fn codex_inventory_rejects_ambiguous_bindings_and_repeated_pages() {
    for mode in ["success", "ambiguous", "cursor"] {
        let log = std::env::temp_dir().join(format!("kindred-codex-{}.jsonl", crate::db::id()));
        let mut rpc = peer(mode, &log).await;
        let thread = auxiliary(&mut rpc).await.unwrap();
        let result = inventory(&mut rpc, &thread).await;
        if mode == "cursor" {
            assert!(result.is_err());
        } else {
            let inv = result.unwrap();
            assert_eq!(
                inv["connections"][0]["execution_available"],
                mode == "success"
            );
            assert_eq!(
                inv["connections"][0]["permission_persistence_available"],
                false
            );
            if mode == "success" {
                assert!(binding(&inv, &args()).is_ok());
                for field in ["account_key", "app_id", "server", "tool_name"] {
                    let mut wrong = args();
                    wrong[field] = json!("wrong");
                    assert!(binding(&inv, &wrong).is_err(), "{field}");
                }
            } else {
                assert!(binding(&inv, &args()).is_err());
            }
        }
        drop(rpc);
        assert!(
            !frames(&log)
                .iter()
                .any(|v| v["method"] == "mcpServer/tool/call")
        );
        std::fs::remove_file(log).unwrap();
    }
}

#[tokio::test]
async fn codex_inventory_does_not_require_the_store_and_never_fakes_unverified_tools() {
    for mode in [
        "success",
        "empty",
        "disabled",
        "missing",
        "many",
        "duplicate",
        "metadata-error",
        "installed-error",
    ] {
        let log = std::env::temp_dir().join(format!("kindred-codex-{}.jsonl", crate::db::id()));
        let mut rpc = peer(mode, &log).await;
        let thread = auxiliary(&mut rpc).await.unwrap();
        let result = inventory(&mut rpc, &thread).await;
        if matches!(mode, "duplicate" | "installed-error") {
            let error = result.unwrap_err().to_string();
            if mode == "installed-error" {
                assert!(error.contains("browser security check (HTTP 403)"));
                assert!(
                    error.len() < 200
                        && !error.contains("<html")
                        && !error.contains("fixture-challenge-token")
                );
            }
        } else {
            let value = result.unwrap();
            let rows = value["connections"].as_array().unwrap();
            assert_eq!(
                rows.len(),
                if mode == "empty" {
                    0
                } else if mode == "many" {
                    101
                } else {
                    1
                }
            );
            assert_eq!(binding(&value, &args()).is_ok(), mode == "success");
            if mode == "metadata-error" {
                assert_eq!(rows[0]["display_name"], "Gmail runtime");
                assert_eq!(rows[0]["status"], "unavailable");
                assert!(rows[0]["tool_schemas"].as_array().unwrap().is_empty());
                let warning = value["warning"].as_str().unwrap();
                assert!(warning.contains("HTTP 403") && warning.contains("unavailable"));
                assert!(!warning.contains("<html") && !warning.contains("fixture-challenge-token"));
            }
        }
        drop(rpc);
        let sent = frames(&log);
        assert!(!sent.iter().any(|v| v["method"] == "app/list"
            || v["method"] == "mcpServer/tool/call"
            || v["method"] == "turn/start"));
        if mode == "many" {
            assert_eq!(sent.iter().filter(|v| v["method"] == "app/read").count(), 2);
        }
        if mode == "empty" {
            assert!(
                !sent
                    .iter()
                    .any(|v| v["method"] == "app/read" || v["method"] == "mcpServerStatus/list")
            );
        }
        std::fs::remove_file(log).unwrap();
    }
}
