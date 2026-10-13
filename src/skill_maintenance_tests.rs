use crate::{db::Db, runtime, tests::{app, bot}};
use serde_json::json;

#[test]
fn skill_maintenance_all_provider_catalogues_expose_guarded_tools() {
    let app=app();
    for provider in ["codex","claude-code","kimi-code","openrouter"] {
        let b=bot(&app.db,provider);
        let specs=runtime::tool_specs_for(&app,&b);
        for name in ["skill_rename","skill_delete"] {
            let spec=specs.iter().find(|s|s["name"]==name).expect("maintenance tool must be supplied");
            assert!(spec["inputSchema"]["required"].as_array().unwrap().contains(&json!("expected_current_hash")));
        }
    }
}

#[tokio::test]
async fn skill_maintenance_dispatch_renames_and_deletes_exact_entry() {
    let app=app();let b=bot(&app.db,"codex");let id=app.db.queue(&b.id,"Rename then remove my workflow",0).unwrap();let run=app.db.run(&id).unwrap();
    app.db.save_skill(&json!({"name":"Old","body":"Keep {{item}}","command":"old-flow","description":"Owned","parameters":[{"name":"item"}]})).unwrap();
    let hash=app.db.skills().unwrap()[0]["current_hash"].clone();
    let renamed=runtime::call_tool(&app,&b,&run,"skill_rename",json!({"name":"Old","new_name":"New","expected_current_hash":hash})).await.unwrap();
    assert_ne!(renamed["failed"],true,"{renamed}");
    assert_eq!(renamed["skill"]["old_name"],"Old");assert_eq!(renamed["skill"]["command"],"old-flow");
    let hash=app.db.skills().unwrap()[0]["current_hash"].clone();
    let deleted=runtime::call_tool(&app,&b,&run,"skill_delete",json!({"name":"New","expected_current_hash":hash})).await.unwrap();
    assert_ne!(deleted["failed"],true,"{deleted}");assert!(app.db.skills().unwrap().is_empty());
    assert!(!app.db.commands().unwrap().iter().any(|c|c.name=="old-flow"));
    let again=runtime::call_tool(&app,&b,&run,"skill_delete",json!({"name":"New","expected_current_hash":hash})).await.unwrap();assert_eq!(again["failed"],true);
}

fn hash(db:&Db,name:&str)->serde_json::Value {
    db.skills().unwrap().into_iter().find(|s|s["name"]==name).unwrap()["current_hash"].clone()
}
fn state(db:&Db,name:&str)->serde_json::Value {
    crate::commands::skill_state(&db.0.lock().unwrap(),name).unwrap()
}
#[test]
fn skill_maintenance_rename_preserves_row_content_and_queued_snapshot() {
    let db=Db::open(":memory:").unwrap();let b=bot(&db,"codex");
    db.save_skill(&json!({"name":"Original","body":"Run {{input}}","command":"original","description":"Keep description","parameters":[{"name":"input","description":"Keep parameter"}]})).unwrap();
    let id=db.queue(&b.id,"/original value",0).unwrap();let run=db.run(&id).unwrap();let before=state(&db,"Original");
    let result=db.rename_skill(&json!({"name":"Original","new_name":"Renamed","expected_current_hash":hash(&db,"Original")})).unwrap();
    assert_eq!(result["command"],"original");let after=state(&db,"Renamed");
    for field in ["row_id","body","parameters","description","import_data","command"] {assert_eq!(after[field],before[field],"{field}");}
    assert_eq!(db.run(&id).unwrap().prompt,run.prompt);
    assert_eq!(db.read_command("original").unwrap()["skill_name"],"Renamed");
    db.remove_skill(&json!({"name":"Renamed","expected_current_hash":hash(&db,"Renamed")})).unwrap();
    assert_eq!(db.run(&id).unwrap().prompt,run.prompt);assert!(db.read_command("original").is_err());
    assert!(db.queue(&b.id,"/original value",0).is_err());
}
#[test]
fn skill_maintenance_collision_reserved_and_missing_leave_exact_state() {
    let db=Db::open(":memory:").unwrap();
    for n in ["one","two"] {db.save_skill(&json!({"name":n,"body":"Keep","command":n})).unwrap();}
    let before=db.skills().unwrap();let fingerprint=hash(&db,"one");
    for extra in [json!({"new_name":"two"}),json!({"new_name":"new","command":"two"}),json!({"new_name":"new","command":"send-email"}),json!({"new_name":"new","command":"Bad Name"}),json!({"new_name":""}),json!({"new_name":"new\nname"})] {
        let mut args=json!({"name":"one","expected_current_hash":fingerprint});args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        assert!(db.rename_skill(&args).is_err());assert_eq!(db.skills().unwrap(),before);
    }
    assert!(db.rename_skill(&json!({"name":"absent","new_name":"new","expected_current_hash":fingerprint})).is_err());
    assert!(db.remove_skill(&json!({"name":"absent","expected_current_hash":fingerprint})).is_err());
    assert!(db.delete_skill("send-email").is_err());assert_eq!(db.skills().unwrap(),before);
    db.rename_skill(&json!({"name":"one","new_name":"new","command":"/new-command","expected_current_hash":fingerprint})).unwrap();
    assert!(db.read_command("one").is_err());assert_eq!(db.read_command("new-command").unwrap()["skill_name"],"new");
}
#[test]
fn skill_maintenance_hash_guards_every_field_and_two_db_profiles() {
    let a=Db::open(":memory:").unwrap();let b=Db::open(":memory:").unwrap();
    for db in [&a,&b] {db.save_skill(&json!({"name":"same","command":"same","body":"Keep"})).unwrap();}
    let before_b=b.skills().unwrap();
    for field in ["body","description","parameters","import_data"] {
        let fingerprint=hash(&a,"same");
        a.0.lock().unwrap().execute(&format!("UPDATE skills SET {field}=? WHERE name='same'"),[match field {"parameters"=>"[ ]","import_data"=>"{\"provenance\":\"new\"}",_=>"changed"}]).unwrap();
        assert!(a.rename_skill(&json!({"name":"same","new_name":"other","expected_current_hash":fingerprint})).is_err());
        assert!(a.remove_skill(&json!({"name":"same","expected_current_hash":fingerprint})).is_err());
    }
    assert!(a.rename_skill(&json!({"name":"same","new_name":"other"})).is_err());
    a.remove_skill(&json!({"name":"same","expected_current_hash":hash(&a,"same")})).unwrap();
    assert_eq!(b.skills().unwrap(),before_b);
}
#[test]
fn skill_maintenance_concurrent_changes_allow_only_one_writer() {
    let db=std::sync::Arc::new(Db::open(":memory:").unwrap());db.save_skill(&json!({"name":"one","body":"same","command":"one"})).unwrap();let fingerprint=hash(&db,"one");
    let barrier=std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads:Vec<_>=(0..2).map(|i|{let db=db.clone();let barrier=barrier.clone();let fingerprint=fingerprint.clone();std::thread::spawn(move||{barrier.wait();db.rename_skill(&json!({"name":"one","new_name":format!("new{i}"),"expected_current_hash":fingerprint})).is_ok()})}).collect();
    let successes=threads.into_iter().map(|t|t.join().unwrap()).filter(|v|*v).count();assert_eq!(successes,1);assert_eq!(db.skills().unwrap().len(),1);
}
#[test]
fn skill_maintenance_imported_package_and_origin_are_preserved() {
    use base64::{Engine,engine::general_purpose::STANDARD};
    let db=Db::open(":memory:").unwrap();let raw=json!({"name":"Local","entry":"SKILL.md","source":"/original/Local/SKILL.md","files":{"SKILL.md":STANDARD.encode("Local instructions"),"scripts/helper.py":STANDARD.encode("print('keep')")}});
    let origin=json!({"device_id":"exact-device","path":"/original/Local/SKILL.md"});
    let preview=crate::skill_import::request_with_origin(&db,&json!({"action":"preview","package":raw}),Some(&origin)).unwrap();
    crate::skill_import::request_with_origin(&db,&json!({"action":"import","package":raw,"expected_hash":preview["import"]["hash"]}),Some(&origin)).unwrap();
    let before=state(&db,"Local");let imported:serde_json::Value=serde_json::from_str(before["import_data"].as_str().unwrap()).unwrap();
    db.rename_skill(&json!({"name":"Local","new_name":"Named","command":"named","expected_current_hash":hash(&db,"Local")})).unwrap();
    let mut after=state(&db,"Named");let mut package:serde_json::Value=serde_json::from_str(after["import_data"].as_str().unwrap()).unwrap();
    assert_eq!(package["name"],"Named");assert_eq!(package["command"],"named");package["name"]=imported["name"].clone();package["command"]=imported["command"].clone();assert_eq!(package,imported);
    after["name"]=before["name"].clone();after["command"]=before["command"].clone();after["import_data"]=before["import_data"].clone();assert_eq!(after,before);
    assert_eq!(crate::skill_refresh::catalog(&db).unwrap()["workflows"][0]["name"],"Named");
    let reviewed=crate::skill_refresh::review(&db,"Named",&raw,&origin,&json!({"action":"preview"})).unwrap();assert_eq!(reviewed["command"],"named");assert_eq!(reviewed["status"],"unchanged");
    db.remove_skill(&json!({"name":"Named","expected_current_hash":hash(&db,"Named")})).unwrap();assert!(crate::skill_refresh::review(&db,"Named",&raw,&origin,&json!({"action":"preview"})).is_err());
}
#[tokio::test]
async fn skill_maintenance_runtime_refuses_foreign_task_disabled_archived_and_finished() {
    let app=app();let b=bot(&app.db,"codex");let other=bot(&app.db,"codex");let id=app.db.queue(&b.id,"Maintain",0).unwrap();let run=app.db.run(&id).unwrap();app.db.save_skill(&json!({"name":"one","body":"Keep"})).unwrap();let args=json!({"name":"one","expected_current_hash":hash(&app.db,"one")});
    let wrong=runtime::call_tool(&app,&other,&run,"skill_delete",args.clone()).await.unwrap();assert_eq!(wrong["failed"],true);
    app.db.save_setting("_account_disabled",&json!(true)).unwrap();assert_eq!(runtime::call_tool(&app,&b,&run,"skill_delete",args.clone()).await.unwrap()["failed"],true);app.db.save_setting("_account_disabled",&json!(false)).unwrap();
    // Simulate a stale invocation after another lifecycle path archived the bot.
    app.db.0.lock().unwrap().execute("UPDATE bots SET profile=json_set(profile,'$.archived',json('true')) WHERE id=?",[&b.id]).unwrap();assert_eq!(runtime::call_tool(&app,&b,&run,"skill_delete",args.clone()).await.unwrap()["failed"],true);app.db.save_bot(&b).unwrap();
    app.db.finish(&id,"completed","Done","").unwrap();assert_eq!(runtime::call_tool(&app,&b,&run,"skill_delete",args).await.unwrap()["failed"],true);assert_eq!(app.db.skills().unwrap().len(),1);
}

#[tokio::test]
async fn skill_maintenance_explicit_only_is_listed_and_all_providers_dispatch() {
    let app=app();
    app.db.save_skill(&json!({"name":"hidden","body":"Never volunteer these instructions","command":"hidden"})).unwrap();
    app.db.0.lock().unwrap().execute("UPDATE skills SET import_data=? WHERE name='hidden'",[json!({"disable_model_invocation":true,"user_invocable":true}).to_string()]).unwrap();
    for provider in ["codex","claude-code","kimi-code","openrouter"] {
        let b=bot(&app.db,provider);let id=app.db.queue(&b.id,"Maintain",0).unwrap();let run=app.db.run(&id).unwrap();
        let listed=runtime::call_tool(&app,&b,&run,"skills_list",json!({})).await.unwrap();let text=listed["text"].as_str().unwrap();assert!(text.contains("hidden"));assert!(!text.contains("Never volunteer"));assert!(crate::skill_import::model_catalog(&app.db).unwrap().is_empty());
        let name=format!("owned-{provider}");app.db.save_skill(&json!({"name":name,"body":"Keep"})).unwrap();
        let result=runtime::call_tool(&app,&b,&run,"skill_rename",json!({"name":name,"new_name":format!("new-{provider}"),"expected_current_hash":hash(&app.db,&name)})).await.unwrap();assert_ne!(result["failed"],true);
        let name=format!("new-{provider}");let result=runtime::call_tool(&app,&b,&run,"skill_delete",json!({"name":name,"expected_current_hash":hash(&app.db,&name)})).await.unwrap();assert_ne!(result["failed"],true);
    }
}
