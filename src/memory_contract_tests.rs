use super::*;
use std::sync::Arc;

/// Exercise the provider protocol, common tool dispatcher, SQLite commit,
/// Activity receipt and fresh conversation context together, without a model.
#[tokio::test]
async fn codex_memory_receipts_match_disk_state_after_restart() {
    for failure in ["none", "storage", "oversized"] {
        let directory = std::env::temp_dir().join(format!("kindred-memory-{}", crate::db::id()));
        let path = directory.join("kindred.db").to_string_lossy().into_owned();
        let mut app = crate::tests::app();
        Arc::get_mut(&mut app).unwrap().db = crate::db::Db::open(&path).unwrap();
        let bot = crate::tests::bot(&app.db, "codex");
        let teammate = crate::tests::bot(&app.db, "codex");
        app.db
            .save_bot_text(&bot.id, "memory", "Existing preference.", None)
            .unwrap();
        app.db
            .save_bot_text(&teammate.id, "memory", "Teammate private memory.", None)
            .unwrap();
        if failure == "storage" {
            app.db.0.lock().unwrap().execute_batch(
                "CREATE TEMP TRIGGER reject_memory BEFORE UPDATE OF memory ON bots BEGIN SELECT RAISE(ABORT,'fixture storage failure'); END;"
            ).unwrap();
        }
        let saved_payload = format!("Existing preference. My ongoing responsibility is inbox management. {}", "Retained context. ".repeat(2000));
        let saved = saved_payload.as_str();
        let script = r#"
import sys,json
failure,saved=sys.argv[1:]
def send(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
    v=json.loads(line);m=v.get('method')
    if m=='initialize': send({'id':v['id'],'result':{}})
    elif m=='initialized': pass
    elif m=='account/read': send({'id':v['id'],'result':{'account':{'type':'chatgpt'}}})
    elif m=='model/list': send({'id':v['id'],'result':{'data':[{'model':'test/model','isDefault':True,'defaultReasoningEffort':'medium','supportedReasoningEfforts':[{'reasoningEffort':'medium'}]}],'nextCursor':None}})
    elif m=='config/read': send({'id':v['id'],'result':{'config':{}}})
    elif m=='app/installed': send({'id':v['id'],'result':{'apps':[]}})
    elif m=='mcpServerStatus/list': send({'id':v['id'],'result':{'data':[],'nextCursor':None}})
    elif m=='thread/start':
        assert any(t['name']=='remember' for t in v['params']['dynamicTools'])
        send({'id':v['id'],'result':{'thread':{'id':'memory-contract'}}})
    elif m=='turn/start':
        send({'id':v['id'],'result':{}})
        send({'id':'save-memory','method':'item/tool/call','params':{'tool':'remember','arguments':{'text':'x'*64001 if failure=='oversized' else saved}}})
    elif v.get('id')=='save-memory':
        assert v['result']['success']==(failure=='none'),v
        receipt=v['result']['contentItems'][0]['text']
        assert ('Memory saved' in receipt)==(failure=='none'),v
        send({'method':'item/completed','params':{'item':{'type':'agentMessage','text':'Saved.' if failure=='none' else 'Save failed.'}}})
        send({'method':'turn/completed','params':{'turn':{'status':'completed'}}})
        break
    else: raise AssertionError('Unexpected frame')
"#;
        let mut command = tokio::process::Command::new("python3");
        command.args(["-u", "-c", script, failure, saved]);
        let mut rpc = Rpc::spawn(command).await.unwrap();
        app.db
            .queue(&bot.id, "Remember your ongoing role", 0)
            .unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            codex_rpc(&app, &bot, &run, &mut rpc),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            output,
            if failure == "none" {
                "Saved."
            } else {
                "Save failed."
            }
        );
        app.db.finish(&run.id, "completed", &output, "").unwrap();
        drop(rpc);
        drop(app);

        let mut reopened = crate::tests::app();
        Arc::get_mut(&mut reopened).unwrap().db = crate::db::Db::open(&path).unwrap();
        let current = reopened.db.bot(&bot.id).unwrap();
        let expected = if failure == "none" {
            saved
        } else {
            "Existing preference."
        };
        assert_eq!(current.memory, expected);
        assert_eq!(
            reopened.db.bot(&teammate.id).unwrap().memory,
            "Teammate private memory."
        );
        let events = reopened.db.events(&run.id).unwrap();
        let requests: Vec<_> = events
            .iter()
            .filter(|e| e["kind"] == "tool_requested" && e["body"]["tool"] == "remember")
            .collect();
        let receipts: Vec<_> = events
            .iter()
            .filter(|e| e["kind"] == "tool_result" && e["body"]["tool"] == "remember")
            .collect();
        assert_eq!(requests.len(), 1);
        assert_eq!(receipts.len(), 1);
        assert_eq!(
            requests[0]["body"]["call_id"],
            receipts[0]["body"]["call_id"]
        );
        assert_eq!(receipts[0]["body"]["failed"], failure != "none");
        let next = reopened
            .db
            .queue(&bot.id, "What is your responsibility?", 0)
            .unwrap();
        let context =
            runtime::instructions(&reopened, &current, &reopened.db.run(&next).unwrap()).unwrap();
        assert!(context.contains(expected));
        assert!(!context.contains("Teammate private memory."));
        if failure != "none" {
            assert!(!context.contains(saved));
        }
        drop(reopened);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
