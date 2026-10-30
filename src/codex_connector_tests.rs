use super::*;
use std::time::Duration;

#[test]
fn compact_discovery_preserves_chatgpt_apps_without_kindred_accounts() {
    let inv = json!({"provider_source":"codex","provider_account_key":"account-bound","configured":false,"apps":[],"provider_connections":[{"app_id":"slack-app","server":"codex_apps","display_name":"Slack","execution_available":true,"requires_approval":true,"tools":["slack.search"],"tool_schemas":[{"name":"slack.search","inputSchema":{"type":"object","description":"x".repeat(100000)}}]}]});
    let compact = discovery_view(inv.clone());
    assert_eq!(compact["chatgpt_connected"], true);
    assert!(compact.get("configured").is_none());
    assert_eq!(compact["kindred_configured"], false);
    assert_eq!(compact["provider_connections"][0]["tool_count"], 1);
    assert!(compact.to_string().len() < 2000);
    assert_eq!(
        inv["provider_connections"][0]["tool_schemas"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn focused_discovery_returns_complete_schemas_and_invalidates_changed_account_cursors() {
    let tools=(0..11).map(|i|json!({"name":format!("slack.search_{i:02}"),"description":"Search messages","inputSchema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}})).collect::<Vec<_>>();
    let mut inv = json!({"provider_account_key":"account-bound","provider_connections":[{"app_id":"slack-app","server":"codex_apps","display_name":"Slack","execution_available":true,"requires_approval":true,"tool_schemas":tools}]});
    let mut cursor = Value::Null;
    let mut names = std::collections::HashSet::new();
    loop {
        let page = tool_page(
            &inv,
            &json!({"app_id":"slack-app","query":"search messages","cursor":cursor}),
        )
        .unwrap();
        assert_eq!(page["account_key"], "account-bound");
        for tool in page["tools"].as_array().unwrap() {
            assert_eq!(tool["inputSchema"]["required"], json!(["query"]));
            assert!(names.insert(tool["name"].as_str().unwrap().to_owned()));
        }
        cursor = page["next_cursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(names.len(), 11);
    let page = tool_page(&inv, &json!({"app_id":"slack-app"})).unwrap();
    inv["provider_account_key"] = json!("replaced");
    assert!(
        tool_page(
            &inv,
            &json!({"app_id":"slack-app","cursor":page["next_cursor"]})
        )
        .is_err()
    );
    inv["provider_connections"][0]["execution_available"] = json!(false);
    assert!(tool_page(&inv, &json!({"app_id":"slack-app"})).is_err());
}
#[test]
fn luna_and_sol_expose_the_same_chatgpt_discovery_path() {
    let app = crate::tests::app();
    let mut bot = crate::tests::bot(&app.db, "codex");
    for model in ["gpt-6-luna", "gpt-6.1-sol"] {
        bot.model = model.into();
        let specs = runtime::tool_specs_for(&app, &bot);
        for name in [
            "connectors_list",
            "codex_connector_tools",
            "codex_connector",
        ] {
            assert!(specs.iter().any(|v| v["name"] == name));
        }
    }
    bot.provider = "openrouter".into();
    assert!(
        !runtime::tool_specs_for(&app, &bot)
            .iter()
            .any(|v| v["name"] == "codex_connector_tools")
    );
}
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
 elif m=='config/read':
  assert p.get('cwd')=='/workspace'
  result={'config':{'apps':{'_default':{'default_tools_approval_mode':'prompt'}}} if mode=='verified-policy' else {}}
 elif m=='configRequirements/read': result={'requirements':{'allowedLoginMethods':['chatgpt'],'allowedApprovalPolicies':None,'allowedApprovalsReviewers':None,'application':None}}
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
  if mode.startswith('verified'):
   name='send_email' if mode=='verified-email' else 'get_profile'
   result['data'][0]['tools']={'gmail.'+name:{'name':'gmail.'+name,'inputSchema':schema if name=='send_email' else {'type':'object','properties':{}},'annotations':{'readOnlyHint':name!='send_email'},'_meta':{'connector_id':'app-mail','link_id':'link-new' if mode=='verified-relink' and reads>=3 else 'link-primary'}}}
 elif m=='app/read':
  assert 0<len(p['appIds'])<=100 and p['includeTools']
  if mode=='metadata-error':
   send({'id':v['id'],'error':{'code':-32000,'message':'Request failed with status 403 Forbidden: <html><script>window._cf_chl_opt={secret:"fixture-challenge-token"}</script></html>'}});continue
  result={'apps':[{'id':'app-mail','name':'Gmail','toolSummaries':[{'name':'send_email','isEnabled':True,'isReadOnly':False}]}],'missingAppIds':[]}
  if mode.startswith('verified'):result['apps'][0]['toolSummaries']=[{'name':'send_email' if mode=='verified-email' else 'get_profile','isEnabled':True,'isReadOnly':mode!='verified-email'}]
  if mode=='missing':result={'apps':[],'missingAppIds':p['appIds']}
  if mode=='ambiguous': result['apps'].append({'id':'another-app','toolSummaries':[{'name':'send_email','isEnabled':True}]})
 elif m=='mcpServer/tool/call':
  assert p['threadId']=='aux' and p['server']=='codex_apps' and p['tool']==('gmail.send_email' if mode=='verified-email' else 'gmail.get_profile' if mode.startswith('verified') else 'send_email')
  if mode in ('challenge','verified-challenge'):
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

fn qualified_fixture() -> (Value, Value, Vec<Value>) {
    let metadata = json!({"apps":[{"id":"app-mail","name":"Gmail","toolSummaries":[{"name":"get_profile","isEnabled":true,"isReadOnly":true}]}]});
    let installed =
        json!({"apps":[{"id":"app-mail","runtimeName":"Gmail","enabled":true,"callable":true}]});
    let servers = vec![
        json!({"name":"codex_apps","authStatus":"bearerToken","runtimeStatus":"connected","tools":{
            "gmail.get_profile":{"name":"gmail.get_profile","inputSchema":{"type":"object","properties":{"link_id":{"type":"string","enum":["link-primary"]}},"required":["link_id"]},"annotations":{"readOnlyHint":true},"_meta":{"connector_id":"app-mail","link_id":"link-primary"}}
        }}),
    ];
    (metadata, installed, servers)
}

#[test]
fn qualified_codex_tools_use_runtime_provenance_and_account_scoped_permissions() {
    let (metadata, installed, mut servers) = qualified_fixture();
    let rows = connections(&metadata, &installed, &servers).unwrap();
    assert_eq!(rows[0]["tools"], json!(["gmail.get_profile"]));
    assert_eq!(rows[0]["permission_persistence_available"], true);
    let key = rows[0]["connector_key"].clone();
    servers[0]["tools"]["gmail.get_profile"]["_meta"]["link_id"] = json!("link-reconnected");
    assert_ne!(
        connections(&metadata, &installed, &servers).unwrap()[0]["connector_key"],
        key
    );
    servers[0]["tools"]["gmail.get_profile"]["_meta"]["link_id"] = json!("link-primary");
    servers[0]["tools"]["gmail.get_profile"]["inputSchema"]["properties"]["link_id"]["enum"] =
        json!(["link-primary", "link-secondary"]);
    assert_ne!(
        connections(&metadata, &installed, &servers).unwrap()[0]["connector_key"],
        key
    );
    servers[0]["tools"]["gmail.get_profile"]["_meta"]["connector_id"] = json!("other-app");
    assert_eq!(
        connections(&metadata, &installed, &servers).unwrap()[0]["execution_available"],
        false
    );
}

#[test]
fn qualified_codex_runtime_does_not_need_public_tool_summaries() {
    let (_, mut installed, servers) = qualified_fixture();
    let metadata = json!({"apps":[]});
    assert_eq!(
        connections(&metadata, &installed, &servers).unwrap()[0]["execution_available"],
        true
    );
    installed["apps"][0]["enabled"] = json!(false);
    assert_eq!(
        connections(&metadata, &installed, &servers).unwrap()[0]["execution_available"],
        false
    );
}

#[tokio::test]
async fn verified_codex_calls_follow_full_access_but_preserve_review_boundaries() {
    for mode in [
        "verified",
        "verified-ask",
        "verified-policy",
        "verified-email",
        "verified-relink",
        "verified-challenge",
    ] {
        let app = crate::tests::app();
        let mut bot = crate::tests::bot(&app.db, "codex");
        bot.approval_mode = if mode == "verified-ask" || mode == "verified-relink" {
            "ask"
        } else {
            "full"
        }
        .into();
        app.db.save_bot(&bot).unwrap();
        let id = app.db.queue(&bot.id, "Fixture connector task", 0).unwrap();
        app.db.claim().unwrap();
        let run = app.db.run(&id).unwrap();
        let log = std::env::temp_dir().join(format!("kindred-codex-{}.jsonl", crate::db::id()));
        let mut rpc = peer(mode, &log).await;
        let mut input = args();
        input["tool_name"] = json!(if mode == "verified-email" {
            "gmail.send_email"
        } else {
            "gmail.get_profile"
        });
        if mode != "verified-email" {
            input["arguments"] = json!({});
        }
        let (a, b, r) = (app.clone(), bot.clone(), run.clone());
        let task = tokio::spawn(async move { execute_rpc(&a, &b, &r, &input, &mut rpc).await });
        if matches!(
            mode,
            "verified-ask" | "verified-policy" | "verified-email" | "verified-relink"
        ) {
            let approval = pending(&app).await;
            assert_eq!(approval["args"]["forced"], mode == "verified-policy");
            assert!(
                !frames(&log)
                    .iter()
                    .any(|v| v["method"] == "mcpServer/tool/call")
            );
            let id = approval["args"]["artifact_id"].as_str().unwrap();
            let card = connector_artifacts::record(&app.db.0.lock().unwrap(), id).unwrap();
            connector_artifacts::update(
                &app,
                id,
                &json!({"action":"approve","revision":card["revision"]}),
            )
            .unwrap();
        }
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let calls = frames(&log)
            .into_iter()
            .filter(|v| v["method"] == "mcpServer/tool/call")
            .count();
        assert_eq!(calls, usize::from(mode != "verified-relink"), "{mode}");
        assert_eq!(
            result["failed"],
            matches!(mode, "verified-relink" | "verified-challenge"),
            "{mode}"
        );
        std::fs::remove_file(log).unwrap();
    }
}

#[test]
fn codex_chatgpt_login_requirement_does_not_force_connector_review() {
    let config = json!({"config":{"apps":null,"approval_policy":null,"approvals_reviewer":null}});
    let requirements = json!({"requirements":{
        "allowedLoginMethods":["chatgpt"], "allowedApprovalPolicies":null,
        "allowedApprovalsReviewers":null, "application":null, "network":null
    }});
    assert!(provider_default_policy(Some(&config), Some(&requirements)));
    for (field, value) in [
        ("allowedApprovalPolicies", json!(["on-request"])),
        ("allowedApprovalsReviewers", json!(["user"])),
        ("application", json!({"some_policy":true})),
        ("network", json!({"enabled":false})),
        ("futurePolicy", json!({})),
        ("allowedLoginMethods", json!(["api_key"])),
        ("allowedLoginMethods", json!(["chatgpt", "api_key"])),
        ("allowedLoginMethods", json!([])),
    ] {
        let mut restricted = requirements.clone();
        restricted["requirements"][field] = value;
        assert!(!provider_default_policy(Some(&config), Some(&restricted)), "{field}");
    }
    for malformed in [json!({}), json!({"requirements":false}), json!({"requirements":[]})] {
        assert!(!provider_default_policy(Some(&config), Some(&malformed)));
    }
}

#[test]
fn codex_nondefault_or_unknown_provider_policy_never_auto_allows() {
    assert!(!provider_default_policy(
        Some(&json!({"config":{"approval_policy":"on-request"}})),
        Some(&json!({"requirements":null}))
    ));
    assert!(provider_default_policy(
        Some(&json!({"config":{}})),
        Some(&json!({"requirements":null}))
    ));
    assert!(!provider_default_policy(
        None,
        Some(&json!({"requirements":null}))
    ));
    assert!(!provider_default_policy(Some(&json!({"config":{}})), None));
    assert!(!provider_default_policy(
        Some(&json!({"config":{"apps":{"app-mail":{"default_tools_approval_mode":"prompt"}}}})),
        Some(&json!({"requirements":null}))
    ));
    assert!(!provider_default_policy(
        Some(&json!({"config":{}})),
        Some(&json!({"requirements":{"allowedApprovalPolicies":["on-request"]}}))
    ));
}

#[test]
fn codex_qualified_email_actions_keep_send_reviews() {
    for tool in [
        "gmail.send_email",
        "gmail.send_draft",
        "gmail.forward_emails",
        "gmail.reply",
        "outlook.send_mail",
        "outlook.reply_all",
    ] {
        assert!(
            connector_artifacts::email_send(
                &json!({"connection":if tool.starts_with("gmail") {"Gmail"}else{"Outlook"},"tool_name":tool})
            ),
            "{tool}"
        );
    }
    for tool in [
        "gmail.create_draft",
        "gmail.get_profile",
        "gmail.batch_read_email",
    ] {
        assert!(
            !connector_artifacts::email_send(&json!({"connection":"Gmail","tool_name":tool})),
            "{tool}"
        );
    }
}

#[test]
fn codex_existing_ask_survives_verified_binding_and_read_revocation_is_checked() {
    use rusqlite::params;
    let app = crate::tests::app();
    let mut bot = crate::tests::bot(&app.db, "codex");
    bot.approval_mode = "full".into();
    app.db.save_bot(&bot).unwrap();
    let (meta, installed, servers) = qualified_fixture();
    let rows = connections(&meta, &installed, &servers).unwrap();
    let key = rows[0]["connector_key"].as_str().unwrap();
    let account = "a".repeat(64);
    crate::connector_policy::catalogue_for(&app.db, "codex-account", &account, &json!(rows))
        .unwrap();
    let c = app.db.0.lock().unwrap();
    c.execute(
        "INSERT INTO connector_grants VALUES(?,?,?,?,?)",
        params![bot.id, "codex-account", account, "app-mail", "ask"],
    )
    .unwrap();
    drop(c);
    assert_eq!(
        crate::connector_policy::inventory(&app, &bot).unwrap()["provider_connections"][0]["always_allow"],
        false
    );
    let mut args = json!({"origin":"codex-account","app_id":"app-mail","account_key":account,"connector_key":key,"connection":"Gmail","tool_name":"gmail.get_profile","arguments":{"link_id":"link-primary"},"permission_persistence_available":true,"read_only":true});
    assert_eq!(
        crate::connector_policy::approval_override(&app.db, &bot.id, &args).unwrap(),
        Some(false)
    );
    app.db
        .0
        .lock()
        .unwrap()
        .execute("DELETE FROM connector_grants WHERE bot_id=?", [&bot.id])
        .unwrap();
    assert_eq!(
        crate::connector_policy::approval_override(&app.db, &bot.id, &args).unwrap(),
        Some(true)
    );
    let id = app.db.queue(&bot.id, "Read my profile", 0).unwrap();
    app.db.claim().unwrap();
    let run = app.db.run(&id).unwrap();
    let card = connector_artifacts::create(&app.db, &run, &args).unwrap();
    args["artifact_id"] = json!(card);
    connector_artifacts::set_status(&app.db, &card, "ready").unwrap();
    bot.approval_mode = "ask".into();
    app.db.save_bot(&bot).unwrap();
    assert!(connector_artifacts::dispatch(&app.db, &run, &args).is_err());
}

// Opt-in, read-only smoke test of the actual gateway. Uses an isolated Kindred
// database and never enqueues a task or approval in the owner's live profile.
#[tokio::test]
#[ignore = "requires an explicitly selected authenticated Codex runtime"]
async fn codex_live_read_only_gateway() {
    // The supplied SSH transport ends at the guest host. Append the same
    // guest launcher as Rpc::connect, including authentication restrictions.
    // A hand-built Codex command previously hid a production-only review bug.
    let mut command: Vec<String> = serde_json::from_str(
        &std::env::var("KINDRED_CODEX_LIVE_SSH_COMMAND_JSON").expect("explicit live SSH transport"),
    )
    .unwrap();
    assert!(command.len() >= 3, "SSH transport must include a remote guest command");
    let remote = command.last_mut().unwrap();
    remote.push_str(" '");
    remote.push_str(&crate::rpc::guest_command("/usr/local/bin/codex").replace('\'', "'\"'\"'"));
    remote.push('\'');
    let mut child = tokio::process::Command::new(&command[0]);
    child.args(&command[1..]);
    let mut rpc = Rpc::spawn(child).await.unwrap();
    let thread = auxiliary(&mut rpc).await.unwrap();
    let inv = inventory(&mut rpc, &thread).await.unwrap();
    let rows = inv["connections"].as_array().unwrap();
    assert!(rows.iter().all(|r| r["execution_available"] == true));
    println!(
        "Live discovery: {} apps, {} tools",
        rows.len(),
        rows.iter()
            .map(|r| r["tools"].as_array().unwrap().len())
            .sum::<usize>()
    );
    for (name, arguments) in [
        ("github.list_installed_accounts", json!({})),
        ("google_calendar.list_calendars", json!({"max_results":1})),
        ("google_drive.search", json!({"query":"Aldabra","topn":1})),
    ] {
        let row = rows
            .iter()
            .find(|r| r["tools"].as_array().unwrap().contains(&json!(name)))
            .unwrap();
        let input = json!({"app_id":row["app_id"],"server":"codex_apps","tool_name":name,"account_key":inv["account_key"],"arguments":arguments});
        let resolved = binding(&inv, &input).unwrap();
        assert_eq!(resolved["tool"]["read_only"], true);
        assert_eq!(resolved["permission_persistence_available"], true);
        let app = crate::tests::app();
        let mut bot = crate::tests::bot(&app.db, "codex");
        bot.approval_mode = "full".into();
        app.db.save_bot(&bot).unwrap();
        let id = app
            .db
            .queue(&bot.id, "Read-only connector verification", 0)
            .unwrap();
        app.db.claim().unwrap();
        let run = app.db.run(&id).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(90),
            execute_rpc(&app, &bot, &run, &input, &mut rpc),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["failed"], false, "{name}: {}", result["text"]);
        assert!(app.db.approvals().unwrap().is_empty());
        println!("Verified {name}: completed without redundant review");
    }
}
