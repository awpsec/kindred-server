use crate::{runtime, tests};
use serde_json::json;
fn retry() -> (runtime::Shared,crate::db::Bot,String,String) {
    let app=tests::app();let bot=tests::bot(&app.db,"codex");
    let source=app.db.queue(&bot.id,"Check the synthetic schedule",0).unwrap();
    app.db.0.lock().unwrap().execute("INSERT INTO routine_runs VALUES(?,'synthetic-occurrence',0)",[&source]).unwrap();
    app.db.finish(&source,"failed","","Synthetic provider failure").unwrap();
    app.db.chat_complete(&app.db.run(&source).unwrap()).unwrap();
    let next=app.db.continue_task(&source).unwrap();
    (app,bot,source,next)
}
#[test]
fn recovery_keeps_scheduled_membership_and_provisional_words_private() {
    let (app,_,_,next)=retry();
    app.db.event(&next,"assistant",json!({"text":"Synthetic no-change progress"})).unwrap();
    assert!(!app.db.chat_messages(&app.db.run(&next).unwrap().chat_id).unwrap().iter().any(|m|m["run_id"]==next && m["kind"]=="assistant"));
    let scheduled:bool=app.db.0.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM routine_runs WHERE run_id=?)",[&next],|r|r.get(0)).unwrap();
    assert!(scheduled);
}
#[tokio::test]
async fn retry_can_finish_quietly_without_a_no_change_final_or_notification() {
    let (app,bot,_,next)=retry();
    let run=app.db.claim_bot(&bot.id).unwrap().unwrap();assert_eq!(run.id,next);
    let value=runtime::call_tool(&app,&bot,&run,"finish_quietly",json!({})).await.unwrap();
    assert_eq!(value["finish_quietly"],true);
    app.db.finish(&next,"completed","Synthetic no-change final","").unwrap();
    app.db.event(&next,"run_finished",json!({})).unwrap();
    app.db.chat_complete(&run).unwrap();app.db.chat_complete(&run).unwrap();
    assert!(!app.db.chat_messages(&run.chat_id).unwrap().iter().any(|m|m["run_id"]==next && m["kind"]=="result"));
    assert!(!app.db.notifications(None).unwrap()["items"].as_array().unwrap().iter().any(|v|v["run_id"]==next));
}
