//! Provider-independent Gmail monitoring setup; secrets stay out of chat.
use crate::{db::Run,runtime::{App,Shared},questions::{QuestionInput,Answer}};
use anyhow::{Result,Context,ensure};
use serde::Deserialize;
use serde_json::{Value,json};
use rusqlite::{params,OptionalExtension};
use axum::{Json,extract::{State,Path}};
pub fn view(app:&App,bot:Option<&str>)->Result<Value>{Ok(json!({"connections":crate::mail_watch::monitor_accounts(app)?,"items":crate::mail_watch::watches(&app.db)?.into_iter().filter(|w|bot.is_none_or(|id|w.input.bot_id==id)).collect::<Vec<_>>(),"setup":"Connections → Gmail. Gmail monitoring works with every bot provider. Its read-only event authorization is separate from Claude/ChatGPT chat connectors; unchanged checks use no AI."}))}
fn result(app:&App,run:&Run,id:&str)->Result<Value>{
 let q=app.db.question(id)?;
 let instruction=if q.status=="pending"{"Wait for the Gmail connection card. Finish quietly; the server resumes this task after the user connects or chooses Not now. Never request credentials in chat."}else if q.status=="dismissed" || q.selected==Some(1){"Setup was deferred or dismissed. Finish quietly; do not reopen it."}else if q.continuation_run_id==run.id{"Use the exact verified account in the answer. Inspect inbox_monitors_list, preserve existing settings and create the requested monitor with inbox_monitor_save. The connection itself created no monitor or email-send permission."}else{"This setup already has an answer. Only the assigned continuation should act. Inspect saved monitoring before reporting status."};
 Ok(json!({"deferred_question":q.status=="pending","text":json!({"question_id":q.id,"status":q.status,"answer":q.answer,"instruction":instruction,"monitoring":view(app,Some(&run.bot_id))?}).to_string()}))
}
pub fn setup(app:&App,run:&Run,args:&Value)->Result<Value>{
 let key=args["topic_key"].as_str().context("Use a stable setup topic_key")?;
 ensure!(!key.is_empty() && key.len()<=100 && key.bytes().all(|b|b.is_ascii_alphanumeric() || b"._:-".contains(&b)),"Use a stable Gmail setup key");
 let topic=format!("gmail.setup.{key}");
 let prior:Option<String>=app.db.0.lock().unwrap().query_row("SELECT id FROM questions WHERE bot_id=?1 AND (chat_id=?2 OR delivery_chat_id=?2) AND topic_key=?3 ORDER BY created DESC,rowid DESC LIMIT 1",params![run.bot_id,run.chat_id,topic],|r|r.get(0)).optional()?;
 if let Some(id)=prior{return result(app,run,&id)}
 let account=args["account_id"].as_str().unwrap_or("");let name=args["account_name"].as_str().unwrap_or("");
 ensure!(name.len()<=80 && !name.chars().any(char::is_control),"Keep the Gmail account name within 80 characters");
 let accounts=crate::mail_watch::monitor_accounts(app)?;
 if accounts.iter().any(|a|a["status"]=="connected" && (account.is_empty()||a["id"]==account) && (name.is_empty()||a["name"].as_str().is_some_and(|s|s.eq_ignore_ascii_case(name))||a["email"].as_str().is_some_and(|s|s.eq_ignore_ascii_case(name)))){
  return Ok(json!({"text":json!({"status":"ready","monitoring":view(app,Some(&run.bot_id))?,"instruction":"Use the exact requested mailbox; ask if ambiguous. Inspect inbox_monitors_list and preserve existing settings. Save requested event monitoring with inbox_monitor_save; normal approval applies. Use the user's alert criteria. New mail wakes the assigned bot; unchanged checks use no AI. Do not create a recurring model polling task unless specifically requested. Report actual saved health; setup is not evidence of a delivered event."}).to_string()}));
 }
 let requested=app.db.ask_question(run,QuestionInput{topic_key:topic,question:"Connect Gmail monitoring".into(),context:"Connect Gmail in the secure form, then choose its verified mailbox. This read-only background connection works with every bot, including Claude and Codex. Your chat connector and send permissions stay unchanged. Connecting alone does not create a monitor.".into(),options:vec!["Connect Gmail".into(),"Not now".into()]})?;
 let data:Value=serde_json::from_str(requested["text"].as_str().context("Missing setup question")?)?;
 let id=data["question"]["id"].as_str().context("Missing question ID")?;
 app.db.0.lock().unwrap().execute("UPDATE chat_messages SET kind='connection_card',body=? WHERE kind='question' AND body=?",params![json!({"toolkit":"gmail_events","question_id":id,"bot_id":run.bot_id,"account_id":account,"account_name":name}).to_string(),id])?;
 result(app,run,id)
}
#[derive(Deserialize)] #[serde(deny_unknown_fields)] pub struct Complete {account_id:String}
pub async fn complete(State(app):State<Shared>,Path(id):Path<String>,Json(input):Json<Complete>)->Result<Json<Value>,crate::web::Error>{Ok(Json(complete_inner(&app,&id,&input.account_id).await?))}
async fn complete_inner(app:&App,id:&str,account:&str)->Result<Value>{
 let _lock=app.mail_lock.lock().await;
 ensure!(!app.account_disabled() && app.db.transfer_status()?.is_null(),"Workspace unavailable");
 let q=app.db.question(id)?;
 let body:String=app.db.0.lock().unwrap().query_row("SELECT body FROM chat_messages WHERE kind='connection_card' AND json_valid(body) AND json_extract(body,'$.question_id')=?",[id],|r|r.get(0))?;
 let card:Value=serde_json::from_str(&body)?;
 ensure!(card["toolkit"]=="gmail_events" && card["bot_id"]==q.bot_id,"Not a Gmail monitoring setup card");
 if let Some(expected)=card["account_id"].as_str().filter(|s|!s.is_empty()){ensure!(account==expected,"Choose the requested Gmail account");}
 let selected=crate::mail_watch::monitor_accounts(app)?.into_iter().find(|a|a["id"]==account&&a["status"]=="connected").context("Finish connecting Gmail before continuing")?;
 // Verify Google identity now, not merely a browser's success message.
 let profile=crate::mail_watch::account_request(app,account,reqwest::Method::GET,"profile",vec![],None).await?;
 let email=profile["emailAddress"].as_str().filter(|s|s.contains('@')).context("Gmail did not identify the mailbox")?;
 if let Some(expected)=selected["email"].as_str(){ensure!(email.eq_ignore_ascii_case(expected),"Connected Gmail identity changed");}
 let answer=app.db.answer_question(id,Answer{selected:None,custom:Some(format!("Verified Gmail monitoring account_id={account}, mailbox={email}. Continue the original request with this exact account. Preserve existing monitor settings. No monitor or email-send permission was created by connecting."))})?;
 Ok(json!({"status":"connected","continuation_run_id":answer.continuation_run_id}))
}
#[cfg(test)]mod tests{
 use super::*;
 #[test]fn every_provider_gets_scoped_idempotent_setup_without_a_model_polling_routine(){
  for provider in ["claude-code","codex","openrouter"]{let app=crate::tests::app();let bot=crate::tests::bot(&app.db,provider);let id=app.db.queue(&bot.id,"Monitor Gmail",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();assert_eq!(run.id,id);let first=setup(&app,&run,&json!({"topic_key":"mail"})).unwrap();let again=setup(&app,&run,&json!({"topic_key":"mail"})).unwrap();assert_eq!(first,again);assert_eq!(first["deferred_question"],true);assert!(app.db.routines().unwrap().is_empty());assert!(crate::mail_watch::watches(&app.db).unwrap().is_empty());}
 }
}
