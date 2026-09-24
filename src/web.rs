use crate::{
    db::{self, Bot, Routine},
    rpc::Rpc,
    runtime::{self, Shared},
    vm,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{Request, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};

use subtle::ConstantTimeEq;

pub struct Error(anyhow::Error);
impl<E: Into<anyhow::Error>> From<E> for Error {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":self.0.to_string()})),
        )
            .into_response()
    }
}
type Result<T> = std::result::Result<T, Error>;
macro_rules! require {
    ($condition:expr, $message:literal) => {
        if !$condition {
            return Err(anyhow::anyhow!($message).into());
        }
    };
}

pub fn router(app: Shared) -> Router {
    let api = Router::new()
        .route("/status", get(status))
        .route("/workspace-artifact-folders", get(crate::workspace_artifacts::folder_list).post(crate::workspace_artifacts::folder_create).patch(crate::workspace_artifacts::folder_move))
        .route("/workspace-artifacts", get(crate::workspace_artifacts::list).post(crate::workspace_artifacts::create))
        .route("/workspace-artifacts/{id}/export", get(crate::workspace_artifacts::export))
        .route("/workspace-artifacts/{id}", get(crate::workspace_artifacts::get).patch(crate::workspace_artifacts::update))
        .route("/attention", get(attention))
        .route("/chats/{id}/read", put(mark_chat_read))
        .route("/bots/{id}/identity", put(update_bot_identity))
        .route("/chats", get(chats).post(create_chat))
        .route("/chats/{id}", get(chat).put(update_chat))
        .route("/chats/{id}/messages", post(chat_send))
        .route("/chats/{chat}/panels/{id}/respond", post(workflow_response))
        .route("/chats/{id}/planning", get(crate::plans::list_route))
        .route("/bots/{id}/artifacts", get(crate::artifact_library::list))
        .route(
            "/provider-inbox-routines",
            post(crate::provider_inbox::save),
        )
        .route(
            "/bots/{id}/artifacts/{kind}/{artifact}",
            get(crate::artifact_library::record),
        )
        .route(
            "/chats/{chat}/checklists/{id}",
            axum::routing::patch(crate::plans::checklist_route),
        )
        .route(
            "/chats/{chat}/reminders/{id}",
            axum::routing::patch(crate::plans::reminder_route),
        )
        .route("/chats/{id}/messages/{seq}/reaction", put(message_reaction))
        .route("/chats/{id}/pin", put(pin_chat))
        .route("/bots/{id}/pin", put(pin_bot))
        .route("/computer/resources", get(resources))
        .route(
            "/computer/maintenance",
            get(maintenance).put(save_maintenance).post(request_maintenance),
        )
        .route("/marketplace", get(marketplace))
        .route("/marketplace/{id}", get(marketplace_detail))
        .route("/marketplace/{id}/configs", get(marketplace_configs))
        .route("/marketplace/{id}/tools", get(marketplace_tools))
        .route("/bots", get(bots).post(save_bot))
        .route("/bots/{id}", put(update_bot).delete(delete_archived_bot))
        .route(
            "/bots/{id}/connectors",
            get(crate::connector_policy::get).put(crate::connector_policy::put),
        )
        .route("/bots/{id}/avatar.png", get(bot_avatar))
        .route("/bots/{id}/text", put(update_bot_text))
        .route("/bot-drafts/{id}/create", post(create_drafted_bot))
        .route(
            "/workspace-imports",
            get(workspace_imports)
                .post(workspace_import_start)
                .layer(DefaultBodyLimit::max(12 * 1024 * 1024)),
        )
        .route("/workspace-imports/{id}", get(workspace_import_get))
        .route(
            "/workspace-imports/{id}/apply",
            post(workspace_import_apply).layer(DefaultBodyLimit::max(2 * 1024 * 1024)),
        )
        .route(
            "/workspace-imports/{id}/retry",
            post(workspace_import_retry),
        )
        .route(
            "/workspace-imports/{id}/cancel",
            post(workspace_import_cancel),
        )
        .route("/bots/{id}/workspace-origin", get(workspace_origin))
        .route("/runs", get(runs).post(queue))
        .route("/runs/{id}", get(run))
        .route("/attachments/{id}", get(attachment))
        .route("/deliverables/{id}", get(download_deliverable))
        .route(
            "/uploads",
            post(upload_file).layer(DefaultBodyLimit::max(12 * 1024 * 1024)),
        )
        .route("/uploads/{id}", get(download_upload).delete(remove_upload))
        .route("/notifications", get(notifications))
        .route("/notification-mutes/{kind}/{id}", put(mute_notifications))
        .route("/runs/{id}/cancel", post(cancel))
        .route("/runs/{id}/continue", post(continue_task))
        .route("/runs/{id}/steer", post(steer_run))
        .route("/approvals", get(approvals))
        .route("/approvals/{id}", post(decide))
        .route(
            "/connector-artifacts/{id}",
            post(crate::connector_artifacts::action),
        )
        .route("/user-tasks", get(user_tasks))
        .route("/user-tasks/{id}/complete", post(complete_user_task))
        .route("/user-tasks/{id}/code", post(enter_user_task_code))
        .route("/questions/{id}/answer", post(answer_question))
        .route("/skills", get(skills).post(save_skill))
        .route(
            "/skills/import",
            axum::routing::post(import_skill)
                .layer(axum::extract::DefaultBodyLimit::max(12 * 1024 * 1024)),
        )
        .route("/commands", get(commands))
        .route("/skills/{name}", axum::routing::delete(delete_skill))
        .route("/routines", get(routines).post(save_routine))
        .route(
            "/inbox-monitors",
            get(crate::mail_watch::list).post(crate::mail_watch::save),
        )
        .route("/inbox-monitors/{id}/pause", post(crate::mail_watch::pause))
        .route("/inbox-monitors/{id}/check", post(crate::mail_watch::check))
        .route(
            "/inbox-monitors/{id}",
            axum::routing::delete(crate::mail_watch::remove),
        )
        .route(
            "/routines/{id}",
            axum::routing::delete(delete_routine).patch(crate::routine_updates::route),
        )
        .route("/routines/{id}/run", post(run_routine_now))
        .route("/settings", get(settings).put(save_settings))
        .route(
            "/settings/timezone/initialize",
            post(crate::timezone::initialize_route),
        )
        .route(
            "/settings/timezone/resolve",
            post(crate::timezone::resolve_route),
        )
        .route("/connections", get(connections).put(save_connections))
        .route("/local/devices", get(crate::local_access::devices))
        .route(
            "/local/devices/{id}",
            put(crate::local_access::rename_device),
        )
        .route(
            "/bots/{id}/local-access",
            get(crate::local_access::bot_status),
        )
        .route(
            "/local/poll",
            post(crate::local_access::poll).layer(DefaultBodyLimit::max(12 * 1024 * 1024)),
        )
        .route("/connections/openrouter", post(save_openrouter))
        .route("/opencode/{id}/key", post(crate::opencode::save_key))
        .route("/opencode/{id}/models", get(crate::opencode::models))
        .route(
            "/provider-cli/{id}/{action}",
            post(crate::cli_providers::action),
        )
        .route(
            "/providers",
            get(crate::provider_accounts::list).post(crate::provider_accounts::save),
        )
        .route("/providers/{id}", axum::routing::delete(crate::provider_accounts::remove))
        .route(
            "/providers/{id}/models",
            get(crate::provider_accounts::models).post(crate::provider_accounts::refresh_models),
        )
        .route(
            "/providers/{id}/usage",
            get(crate::provider_accounts::usage),
        )
        .route(
            "/openrouter/models",
            get(crate::providers::openrouter_models),
        )
        .route("/composio", get(composio_status))
        .route("/composio/key", post(composio_key))
        .route("/composio/{id}/{action}", post(composio_action))
        .route("/activity", get(activity))
        .route("/commands/{id}/stop", post(crate::command_jobs::stop_from_ui))
        .route("/devices/link", post(crate::pairing::issue))
        .route("/vm/{action}", post(vm_action))
        .route("/computer", get(screenshot).post(computer))
        .route("/computer/session", post(crate::vnc::ticket))
        .route("/takeover", post(takeover))
        .route("/codex/{action}", post(codex))
        .route_layer(middleware::from_fn_with_state(app.clone(), authenticate));
    Router::new()
        .route(
            "/hooks/gmail/{id}",
            post(gmail_push).layer(DefaultBodyLimit::max(65536)),
        )
        .route("/updates/{filename}", get(release_file))
        .nest("/api", api)
        .route("/device/claim", post(crate::pairing::claim))
        .route("/vnc", get(crate::vnc::upgrade))
        .merge(static_assets())
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(middleware::from_fn(headers))
        .with_state(app)
}
async fn gmail_push(
    State(app): State<Shared>,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    match crate::mail_watch::receive(&app, &id, &headers, &body).await {
        Ok(status) => status.into_response(),
        Err(_) => StatusCode::UNAUTHORIZED.into_response(),
    }
}
pub(crate) async fn headers(req: Request<Body>, next: Next) -> Response {
    let artifact_frame = req.uri().path() == "/artifact-frame.html";
    let mut res = next.run(req).await;
    for (key, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", if artifact_frame { "SAMEORIGIN" } else { "DENY" }),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            if artifact_frame { include_str!("../ui/artifact-frame-policy.txt").trim() } else { include_str!("../ui/app-policy.txt").trim() },
        ),
    ] {
        res.headers_mut().insert(
            header::HeaderName::from_static(key),
            header::HeaderValue::from_static(value),
        );
    }
    res
}
pub fn valid_token(expected: &str, actual: &str) -> bool {
    expected.as_bytes().ct_eq(actual.as_bytes()).into()
}
async fn authenticate(State(app): State<Shared>, req: Request<Body>, next: Next) -> Response {
    if app.account_disabled() {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        if !origin
            .to_str()
            .ok()
            .is_some_and(|v| app.config.allows_origin(v))
        {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !valid_token(&app.token, token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Connect using your server access token."})),
        )
            .into_response();
    }
    next.run(req).await
}
async fn status(
    State(app): State<Shared>,
    Query(screen): Query<ScreenQuery>,
) -> Result<Json<Value>> {
    Ok(Json(
        json!({"version":env!("CARGO_PKG_VERSION"),"vm":app.config.vm.domain,"maintenance":crate::vm_maintenance::state(&app.db)?,"control_pauses":crate::screen_control::paused(&app.db)?,"public_url":app.config.public_url,"takeover":app.db.screen_takeover(screen_slot(&app,&screen.bot_id)?)?,"openrouter_configured":crate::connections::openrouter_key(&app).is_some(),"openrouter_zdr":app.config.openrouter.require_zdr,"max_steps":app.config.max_steps,"screen_bot_id":screen.bot_id,"computer_pointer":app.db.setting(&format!("computer-pointer:{}",screen.bot_id))?,"computer_busy":app.screen_lock(screen_slot(&app,&screen.bot_id)?).try_lock().is_err() || app.db.screen_quiet(screen_slot(&app,&screen.bot_id)?)?>db::now(),"computer_recovering_seconds":(app.db.screen_quiet(screen_slot(&app,&screen.bot_id)?)?-db::now()).max(0)}),
    ))
}
async fn bots(State(app): State<Shared>) -> Result<Json<Vec<Bot>>> {
    Ok(Json(app.db.bots()?))
}
async fn save_bot(State(app): State<Shared>, Json(mut b): Json<Bot>) -> Result<Json<Bot>> {
    b.id = db::id();
    app.db.create_bot(&b)?;
    Ok(Json(b))
}
async fn workspace_imports(State(app): State<Shared>) -> Result<Json<Vec<Value>>> {
    Ok(Json(crate::workspace_import::list(&app.db)?))
}
async fn workspace_import_start(
    State(app): State<Shared>,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    Ok(Json(crate::workspace_import::start(&app.db, &v)?))
}
async fn workspace_import_get(
    State(app): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    Ok(Json(crate::workspace_import::get(&app.db, &id)?))
}
async fn workspace_import_apply(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    Ok(Json(crate::workspace_import::apply(&app.db, &id, &v)?))
}
async fn workspace_import_retry(
    State(app): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    Ok(Json(crate::workspace_import::retry(&app.db, &id)?))
}
async fn workspace_import_cancel(
    State(app): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    Ok(Json(crate::workspace_import::cancel(&app.db, &id)?))
}
async fn workspace_origin(
    State(app): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    Ok(Json(crate::workspace_import::origin(&app.db, &id)?))
}
async fn create_drafted_bot(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(v): Json<Value>,
) -> Result<Json<Bot>> {
    let edited = v
        .get("bot")
        .filter(|b| !b.is_null())
        .map(|b| serde_json::from_value(b.clone()))
        .transpose()?;
    Ok(Json(app.db.create_drafted_bot(&id, edited)?))
}
async fn update_bot(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(v): Json<Value>,
) -> Result<Json<Bot>> {
    app.db.bot(&id)?;
    let mut b: Bot = serde_json::from_value(v.clone())?;
    b.id = id;
    app.db
        .save_bot_preferences(&b, v["preserve_text"] == true)?;
    Ok(Json(app.db.bot(&b.id)?))
}
async fn bot_avatar(State(app): State<Shared>, Path(id): Path<String>) -> Result<Response> {
    let bot = app.db.bot(&id)?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "private, max-age=0"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        crate::avatars::png(&bot.profile, &bot.name)?,
    )
        .into_response())
}
async fn update_bot_text(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(v): Json<Value>,
) -> Result<Json<Bot>> {
    let field = v["field"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing field"))?;
    let value = v["value"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing value"))?;
    let expected = v["expected"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing previous text"))?;
    app.db.save_bot_text(&id, field, value, Some(expected))?;
    Ok(Json(app.db.bot(&id)?))
}
#[derive(Deserialize)]
struct RunQuery {
    bot_id: Option<String>,
}
async fn runs(State(app): State<Shared>, Query(q): Query<RunQuery>) -> Result<Json<Vec<Value>>> {
    let rows = app.db.runs(q.bot_id.as_deref())?.into_iter().map(|run| {
        let mut value = serde_json::to_value(&run)?;
        if !run.chat_id.is_empty() && !run.chat_id.starts_with("dm-") {
            value["activity_started"] = json!(app.db.group_activity_started(&run.id)?);
        }
        Ok(value)
    }).collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Json(rows))
}
async fn composio_status(State(app): State<Shared>) -> Result<Json<Value>> {
    Ok(Json(crate::composio::status(&app)?))
}
async fn composio_key(State(app): State<Shared>, Json(v): Json<Value>) -> Result<Json<Value>> {
    crate::composio::save_key(&app, runtime::string(&v, "key")?).await?;
    Ok(Json(json!({"ok":true})))
}
async fn composio_action(
    State(app): State<Shared>,
    Path((id, action)): Path<(String, String)>,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    let selector = v["account_id"].as_str().unwrap_or("");
    let result = match action.as_str() {
        "connect" => {
            crate::composio::connect_named(
                &app,
                &id,
                runtime::string(&v, "permission")?,
                v["auth_config_id"].as_str().unwrap_or(""),
                v["name"].as_str().unwrap_or("default"),
            )
            .await?
        }
        "check" => crate::composio::check_account(&app, &id, selector).await?,
        "complete-connection" => crate::composio::complete_connection_card(&app, &id, selector, runtime::string(&v, "question_id")?).await?,
        "test" => crate::composio::test_account(&app, &id, selector).await?,
        "rename" => {
            crate::composio::rename_account(&app, &id, selector, runtime::string(&v, "name")?)
                .await?
        }
        "authenticate" => crate::composio::authenticate(&app, &id, selector).await?,
        "disconnect" => {
            crate::composio::disconnect_account(&app, &id, selector).await?;
            json!({"ok":true})
        }
        _ => return Err(anyhow::anyhow!("Unknown connector action").into()),
    };
    Ok(Json(result))
}
async fn activity(State(app): State<Shared>) -> Result<Json<Value>> {
    let mut result = serde_json::Map::new();
    let mut runs = app.db.runs(None)?;
    runs.sort_by_key(|r| {
        !matches!(
            r.status.as_str(),
            "running" | "awaiting_user" | "awaiting_approval" | "cancelling"
        )
    });
    for run in runs {
        if run.status == "steered" {
            continue;
        }
        if result.contains_key(&run.bot_id) {
            continue;
        }
        let events = app.db.activity_events(&run.id)?;
        let last = events.iter().rev().find(|e| {
            matches!(
                e["kind"].as_str(),
                Some("tool_started" | "tool_result" | "tool_requested" | "model_progress")
            )
        });
        let mut display = last.cloned();
        if let Some(e) = display.as_mut().filter(|e| e["kind"] == "tool_result") {
            if let Some(start) = events
                .iter()
                .rev()
                .find(|s| s["kind"] == "tool_started" && s["body"]["tool"] == e["body"]["tool"])
            {
                e["body"]["args"] = start["body"]["args"].clone();
            }
        }
        let (shape, label) = runtime::activity(&run.status, display.as_ref());
        let mut shape = shape.to_owned();
        let mut label = label.to_owned();
        let mut started_at = last
            .and_then(|e| e["created"].as_i64())
            .unwrap_or(run.created);
        let local = crate::local_access::progress(&app, &run.id)?;
        if run.status == "running" {
            if let Some(p) = &local {
                let desktop = p["desktop"].as_str().unwrap_or("desktop");
                if p["desktop_seen"].as_i64().unwrap_or(0) < db::now() - 5 {
                    label = format!("Connection to {desktop} lost");
                    shape = "worry".into();
                } else if p["phase"] == "awaiting_approval" {
                    label = format!("Waiting for permission on {desktop}");
                    shape = "waiting".into();
                    started_at = p["changed"].as_i64().unwrap_or(started_at);
                } else if p["phase"] == "running" {
                    label = format!("{label} on {desktop}");
                    started_at = p["changed"].as_i64().unwrap_or(started_at);
                } else {
                    label = format!("Waiting for {desktop}");
                    shape = "waiting".into();
                }
            }
        }
        let provider_retry = crate::provider_retry::progress(&app, &run)?;
        if let Some(retry) = &provider_retry {
            shape = "worry".into();
            label = format!("Connection issue - retrying ({}/5)", retry["attempt"]);
        }
        let completed_at = events
            .iter()
            .rev()
            .find(|e| e["kind"] == "run_finished")
            .map(|e| e["created"].clone())
            .unwrap_or(Value::Null);
        let commands = crate::command_jobs::active_count(&app.db, &run.bot_id)?;
        if commands>0
            && !matches!(
                run.status.as_str(),
                "running" | "awaiting_user" | "awaiting_approval"
            )
        {
            shape = "waiting".into();
            label = format!(
                "{} command{} running",
                commands,
                if commands == 1 { "" } else { "s" }
            );
        }
        result.insert(run.bot_id,json!({"commands":commands,"run_id":run.id,"status":run.status,"shape":shape,"label":label,"completed_at":completed_at,"started_at":started_at,"run_created_at":run.created,"server_time":db::now(),"local":local,"provider_retry":provider_retry,"last_active_at":completed_at.as_i64().unwrap_or(run.created)}));
    }
    Ok(Json(Value::Object(result)))
}
#[derive(Deserialize)]
struct Queue {
    bot_id: String,
    prompt: String,
}
async fn queue(State(app): State<Shared>, Json(q): Json<Queue>) -> Result<Json<Value>> {
    let id = app.db.queue(&q.bot_id, &q.prompt, 0)?;
    Ok(Json(json!({"id":id})))
}
async fn run(State(app): State<Shared>, Path(id): Path<String>) -> Result<Json<Value>> {
    Ok(Json(
        json!({"run":app.db.run(&id)?,"events":app.db.events(&id)?,"approvals":app.db.run_approvals(&id)?,"attachments":app.db.attachments(&id)?}),
    ))
}
async fn continue_task(State(app): State<Shared>, Path(id): Path<String>) -> Result<Json<Value>> {
    Ok(Json(json!({"run_id":app.db.continue_task(&id)?})))
}
async fn cancel(State(app): State<Shared>, Path(id): Path<String>) -> Result<Json<Value>> {
    app.db.cancel(&id)?;
    Ok(Json(json!({"ok":true})))
}
async fn steer_run(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(input): Json<Value>,
) -> Result<Json<Value>> {
    Ok(Json(app.db.request_steering(
        &id,
        crate::runtime::string(&input, "run_id")?,
    )?))
}
async fn attachment(State(app): State<Shared>, Path(id): Path<String>) -> Result<Response> {
    Ok(match app.db.attachment_png(&id)? {
        Some(png) => (
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "no-store"),
                (
                    header::CONTENT_DISPOSITION,
                    "inline; filename=kindred-screenshot.png",
                ),
            ],
            png,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    })
}
#[derive(Deserialize)]
struct NotificationQuery {
    after: Option<i64>,
}
async fn mute_notifications(
    State(app): State<Shared>,
    Path((kind, id)): Path<(String, String)>,
    Json(value): Json<Value>,
) -> Result<Json<Value>> {
    let seconds = value["seconds"].as_i64().ok_or_else(|| anyhow::anyhow!("Missing mute duration"))?;
    Ok(Json(app.db.mute_notifications(&kind, &id, seconds)?))
}
async fn notifications(
    State(app): State<Shared>,
    Query(q): Query<NotificationQuery>,
) -> Result<Json<Value>> {
    Ok(Json(app.db.notifications(q.after)?))
}
async fn approvals(State(app): State<Shared>) -> Result<Json<Vec<Value>>> {
    Ok(Json(app.db.approvals()?))
}
#[derive(Deserialize)]
struct Decision {
    approved: bool,
    #[serde(default)]
    choice: String,
    #[serde(default)]
    revision: Option<i64>,
}
async fn decide(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(d): Json<Decision>,
) -> Result<Json<Value>> {
    crate::connector_policy::decide_checked(&app.db, &id, d.approved, &d.choice, d.revision, None)?;
    Ok(Json(json!({"ok":true})))
}
async fn import_skill(State(app): State<Shared>, Json(v): Json<Value>) -> Result<Json<Value>> {
    Ok(Json(crate::skill_import::request(&app.db, &v)?))
}
async fn skills(State(app): State<Shared>) -> Result<Json<Vec<Value>>> {
    Ok(Json(app.db.skills()?))
}
async fn commands(State(app): State<Shared>) -> Result<Json<Vec<crate::commands::Command>>> {
    Ok(Json(app.db.commands()?))
}
async fn save_skill(State(app): State<Shared>, Json(v): Json<Value>) -> Result<Json<Value>> {
    Ok(Json(app.db.save_skill(&v)?))
}
async fn delete_skill(State(app): State<Shared>, Path(name): Path<String>) -> Result<Json<Value>> {
    app.db.delete_skill(&name)?;
    Ok(Json(json!({"ok":true})))
}
async fn delete_routine(State(app): State<Shared>, Path(id): Path<String>) -> Result<Json<Value>> {
    Ok(Json(crate::routine_controls::remove_scheduled(
        &app, None, &id,
    )?))
}
async fn settings(State(app): State<Shared>) -> Result<Json<Value>> {
    Ok(Json(db::general_settings(app.db.setting("general")?)))
}
async fn save_settings(State(app): State<Shared>, Json(v): Json<Value>) -> Result<Json<Value>> {
    let prior = db::general_settings(app.db.setting("general")?);
    let name = runtime::string(&v, "name")?;
    // Clients may still hold a sparse settings response from an older server.
    // Omission preserves the saved identity; an explicit invalid value fails.
    let identity = runtime::string(
        if v.get("identity").is_some() {
            &v
        } else {
            &prior
        },
        "identity",
    )?;
    let theme = runtime::string(&v, "theme")?;
    require!(
        !name.trim().is_empty() && name.len() <= 80 && identity.len() <= 16000,
        "Identity name or preferences are invalid"
    );
    require!(
        matches!(theme, "dark" | "light" | "system"),
        "Invalid appearance theme"
    );
    let approval = v["approval_mode"].as_str().unwrap_or("ask");
    require!(crate::db::valid_approval(approval), "Invalid approval mode");
    require!(
        v.get("show_activity").is_none_or(Value::is_boolean),
        "Show activity must be true or false"
    );
    require!(v.get("separate_bot_chats").is_none_or(Value::is_boolean),"Separate bot chats must be true or false");
    let separate_bot_chats=v["separate_bot_chats"].as_bool().unwrap_or(prior["separate_bot_chats"].as_bool().unwrap_or(true));
    let show_activity = v["show_activity"]
        .as_bool()
        .unwrap_or(prior["show_activity"] == true);
    let notifications = v["notifications"]
        .as_str()
        .or_else(|| prior["notifications"].as_str())
        .unwrap_or("all");
    require!(
        matches!(notifications, "all" | "input_needed" | "none"),
        "Invalid notification frequency"
    );
    require!(
        v.get("local_access").is_none_or(Value::is_boolean),
        "Local access must be true or false"
    );
    let local_access = v["local_access"]
        .as_bool()
        .unwrap_or(prior["local_access"] == true);
    let timezone = v["timezone"]
        .as_str()
        .or_else(|| prior["timezone"].as_str())
        .unwrap_or("");
    require!(
        v.get("timezone").is_none_or(Value::is_string),
        "Time zone must be a string"
    );
    if !timezone.is_empty() {
        crate::timezone::parse(timezone)?;
    }
    let timezone_mode = v["timezone_mode"]
        .as_str()
        .or_else(|| prior["timezone_mode"].as_str())
        .unwrap_or("auto");
    require!(
        matches!(timezone_mode, "auto" | "fixed"),
        "Invalid time zone mode"
    );
    let default_provider = v.get("default_provider").unwrap_or(&prior["default_provider"]);
    require!(default_provider.as_str().is_some_and(crate::provider_accounts::valid_id), "Invalid default provider");
    let model_defaults = v.get("model_defaults").unwrap_or(&prior["model_defaults"]);
    let defaults = model_defaults.as_object().ok_or_else(||anyhow::anyhow!("Expected model defaults"))?;
    require!(defaults.len() <= 100, "Too many model defaults");
    for (provider, selection) in defaults {
        require!(crate::provider_accounts::valid_id(provider), "Invalid default model provider");
        let model = runtime::string(selection, "model")?;
        let effort = selection["reasoning_effort"].as_str().unwrap_or("");
        require!(model.len() <= 200 && !model.chars().any(char::is_control) && effort.len() <= 40 && effort.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "Invalid default model selection");
    }
    let settings = json!({"default_provider":default_provider,"model_defaults":model_defaults,"local_access":local_access,"approval_mode":approval,"name":name,"identity":identity,"theme":theme,"reduced_motion":v["reduced_motion"]==true,"notifications":notifications,"show_activity":show_activity,"separate_bot_chats":separate_bot_chats,"timezone":timezone,"timezone_mode":timezone_mode});
    app.db.save_setting("general", &settings)?;
    Ok(Json(settings))
}
async fn connections(State(app): State<Shared>) -> Result<Json<Value>> {
    Ok(Json(
        json!({"apps":app.db.setting("apps")?.unwrap_or_else(crate::connections::default_apps),"openrouter_configured":crate::connections::openrouter_key(&app).is_some()}),
    ))
}
async fn save_connections(State(app): State<Shared>, Json(v): Json<Value>) -> Result<Json<Value>> {
    let apps = v["apps"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Expected apps list"))?;
    require!(apps.len() <= 30, "At most 30 app connections are allowed");
    let mut seen = std::collections::HashSet::new();
    for item in apps {
        let id = runtime::string(item, "id")?;
        let name = runtime::string(item, "name")?;
        require!(
            crate::config::safe_name(id)
                && seen.insert(id)
                && !name.trim().is_empty()
                && name.len() <= 80,
            "Invalid or duplicate app name/id"
        );
        let url = reqwest::Url::parse(runtime::string(item, "url")?)?;
        require!(
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.as_str().len() <= 2000,
            "Apps need an HTTPS address without embedded credentials"
        );
        require!(
            item["description"].as_str().unwrap_or("").len() <= 500,
            "App description is too long"
        );
    }
    app.db.save_setting("apps", &v["apps"])?;
    Ok(Json(json!({"ok":true})))
}
async fn save_openrouter(State(app): State<Shared>, Json(v): Json<Value>) -> Result<Json<Value>> {
    crate::connections::save_openrouter(&app, runtime::string(&v, "key")?)?;
    Ok(Json(
        json!({"configured":crate::connections::openrouter_key(&app).is_some()}),
    ))
}
async fn routines(State(app): State<Shared>) -> Result<Json<Vec<Value>>> {
    Ok(Json(crate::mail_watch::routines(&app.db)?))
}
async fn run_routine_now(State(app): State<Shared>, Path(id): Path<String>) -> Result<Json<Value>> {
    ensure_routine_workspace(&app)?;
    Ok(Json(json!({"run_id":app.db.run_routine_now(&id)?})))
}
async fn answer_question(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(answer): Json<crate::questions::Answer>,
) -> Result<Json<crate::questions::Question>> {
    Ok(Json(app.db.answer_question(&id, answer)?))
}
async fn save_routine(
    State(app): State<Shared>,
    Json(mut r): Json<Routine>,
) -> Result<Json<Routine>> {
    ensure_routine_workspace(&app)?;
    if r.id.is_empty() {
        r.id = db::id();
    }
    let existing = app.db.routines()?.into_iter().find(|old| old.id == r.id);
    r.next_run = if let Some(old) = existing.filter(|old| {
        !(r.enabled && !old.enabled)
            && old.interval_seconds == r.interval_seconds
            && old.schedule == r.schedule
            && old.run_at == r.run_at
    }) {
        old.next_run
    } else {
        if let Some(at) = r.run_at {
            require!(
                !r.enabled || at > db::now(),
                "Choose a future one-time date"
            );
            at
        } else {
            match &r.schedule {
                Some(s) => s.next_after(db::now())?,
                None => db::now()
                    .checked_add(r.interval_seconds)
                    .ok_or_else(|| anyhow::anyhow!("Invalid interval"))?,
            }
        }
    };
    app.db.save_routine(&r)?;
    Ok(Json(r))
}
fn ensure_routine_workspace(app: &crate::runtime::App) -> Result<()> {
    require!(
        !app.account_disabled() && app.db.transfer_status()?.is_null(),
        "This workspace is paused"
    );
    Ok(())
}
async fn vm_action(State(app): State<Shared>, Path(action): Path<String>) -> Result<Json<Value>> {
    if action == "start" && crate::vm_maintenance::busy(&app.db.0.lock().unwrap())? {
        // A host restart can leave a durable update awaiting reconciliation.
        // Starting the same VM restores that check; never reboot a running job.
        let state = vm::control(&app.config.vm, "domstate").await?;
        return Ok(Json(
            json!({"state":if state.trim()=="running" {state} else {vm::control(&app.config.vm,"start").await?}}),
        ));
    }
    computer_ready(&app)?;
    let mut leases = Vec::new();
    for slot in 1..=32 {
        leases.push(app.screen_lock(slot).try_lock_owned().map_err(|_| {
            anyhow::anyhow!("A screen is in use. Return control and stop active tasks first.")
        })?);
    }
    crate::vm_maintenance::touch(&app.db.0.lock().unwrap())?;
    Ok(Json(
        json!({"state":vm::control(&app.config.vm,&action).await?}),
    ))
}
#[derive(Deserialize, Default)]
pub struct ScreenQuery {
    #[serde(default)]
    pub bot_id: String,
}
pub fn screen_slot(app: &Shared, bot_id: &str) -> anyhow::Result<i64> {
    if !bot_id.is_empty() {
        return app.db.screen(bot_id);
    }
    if let Some(bot) = app.db.bots()?.into_iter().find(|b| !b.profile.archived) {
        app.db.screen(&bot.id)
    } else {
        Ok(1)
    }
}
async fn screenshot(
    State(app): State<Shared>,
    Query(q): Query<ScreenQuery>,
) -> Result<Json<Value>> {
    Ok(Json(
        vm::guest_screen(
            &app.config.vm,
            screen_slot(&app, &q.bot_id)?,
            "computer_screenshot",
            json!({}),
        )
        .await?,
    ))
}
#[derive(Deserialize)]
struct Takeover {
    enabled: bool,
    #[serde(default)]
    bot_id: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    control_id: Option<String>,
}
async fn takeover(State(app): State<Shared>, Json(v): Json<Takeover>) -> Result<Json<Value>> {
    let slot = screen_slot(&app, &v.bot_id)?;
    screen_ready(&app, slot)?;
    let check_current = || -> anyhow::Result<()> {
        if !v.enabled
            && let Some(expected) = &v.control_id
        {
            let current = crate::screen_control::paused(&app.db)?
                .into_iter()
                .find(|p| p["bot_id"] == v.bot_id);
            anyhow::ensure!(
                current.is_none_or(|p| p["control_id"] == *expected),
                "Control changed since this notice. Review the current notice before returning control."
            );
        }
        Ok(())
    };
    check_current()?;
    let lock = app.screen_lock(slot);
    if !v.enabled {
        app.vnc.close_controls_for(slot);
    }
    let _lease = if !v.enabled {
        tokio::time::timeout(std::time::Duration::from_secs(15), lock.lock())
            .await
            .map_err(|_| anyhow::anyhow!("Screen is still disconnecting. Retry Return control."))?
    } else {
        lock.try_lock()
            .map_err(|_| anyhow::anyhow!("Stop this bot's active task before taking control."))?
    };
    check_current()?;
    crate::screen_control::set(
        &app.db,
        slot,
        v.enabled,
        v.reason.as_deref().unwrap_or("manual"),
    )?;
    Ok(Json(json!({"enabled":v.enabled,"screen":slot})))
}
async fn computer(State(app): State<Shared>, Json(v): Json<Value>) -> Result<Json<Value>> {
    let slot = screen_slot(&app, v["bot_id"].as_str().unwrap_or(""))?;
    screen_ready(&app, slot)?;
    require!(
        app.db.screen_takeover(slot)?,
        "Take control before sending input"
    );
    let tool = runtime::string(&v, "tool")?;
    require!(
        matches!(
            tool,
            "computer_click"
                | "computer_type"
                | "computer_key"
                | "computer_scroll"
                | "computer_open_url"
        ),
        "Unsupported manual computer action"
    );
    let lock = app.screen_lock(slot);
    if matches!(tool, "computer_open_url" | "computer_type") {
        app.vnc.close_controls_for(slot);
    }
    let _lease = tokio::time::timeout(std::time::Duration::from_secs(15), lock.lock())
        .await
        .map_err(|_| anyhow::anyhow!("Screen has live control; use its keyboard and mouse."))?;
    require!(
        app.db.screen_takeover(slot)?,
        "Take control before sending input"
    );
    Ok(Json(
        vm::guest_screen(&app.config.vm, slot, tool, v["args"].clone()).await?,
    ))
}
fn screen_ready(app: &Shared, slot: i64) -> Result<()> {
    crate::vm_maintenance::available(&app.db)?;
    let remaining = app.db.screen_quiet(slot)? - db::now();
    require!(
        remaining <= 0,
        "This screen is recovering from a stopped command. Try again shortly."
    );
    Ok(())
}
fn computer_ready(app: &Shared) -> Result<()> {
    crate::vm_maintenance::available(&app.db)?;
    require!(
        !app.db.runs(None)?.iter().any(|r| matches!(
            r.status.as_str(),
            "running" | "awaiting_user" | "awaiting_approval" | "cancelling"
        )),
        "A bot is working. Stop active tasks before changing VM state."
    );
    for slot in app.db.screen_ids()? {
        screen_ready(app, slot)?;
        require!(
            !app.db.screen_takeover(slot)?,
            "Return all screens to bot control before changing VM state."
        );
    }
    Ok(())
}
async fn codex(State(app): State<Shared>, Path(action): Path<String>) -> Result<Json<Value>> {
    if action != "login"
        && let Some(status) = vm::provider_unavailable(&app.config.vm, "codex").await?
    {
        return Ok(Json(status));
    }
    if action == "connectors" {
        return Ok(Json(crate::codex_connectors::refresh(&app).await?));
    }
    if matches!(action.as_str(), "login" | "logout") {
        crate::connector_policy::clear_catalogue_for(&app.db, "codex-account")?;
    }
    if action == "login" {
        vm::ensure_running(&app.config.vm).await?;
    }
    if !matches!(
        action.as_str(),
        "login" | "account" | "models" | "logout" | "usage"
    ) {
        return Err(anyhow::anyhow!("unsupported Codex action").into());
    }
    let mut auth = app.auth.lock().await;
    if auth.is_none() {
        *auth = Some(Rpc::connect(&app.config.vm).await?);
    }
    let rpc = auth.as_mut().unwrap();
    let result = match action.as_str() {
        "login" => {
            rpc.request("account/login/start", json!({"type":"chatgptDeviceCode"}))
                .await
        }
        "usage" => {
            async {
                let account = rpc
                    .request("account/read", json!({"refreshToken":false}))
                    .await?;
                if account["account"]["type"] != "chatgpt" {
                    return Ok(json!({"account":account["account"],"limits":null}));
                }
                let limits = rpc.request("account/rateLimits/read", json!({})).await?;
                Ok(json!({"account":account["account"],"limits":limits}))
            }
            .await
        }
        "models" => crate::providers::codex_models(rpc)
            .await
            .map(|data| json!({"data":data,"nextCursor":null})),
        "logout" => rpc.request("account/logout", json!({})).await,
        _ => {
            rpc.request("account/read", json!({"refreshToken":false}))
                .await
        }
    };
    if result.is_err() {
        *auth = None;
    }
    Ok(Json(result?))
}

async fn chats(State(app): State<Shared>) -> Result<Json<Vec<crate::chats::Chat>>> {
    Ok(Json(
        app.db
            .chats()?
            .into_iter()
            .filter(|c| !c.id.starts_with("server-"))
            .collect(),
    ))
}
#[derive(Deserialize)]
struct Pin {
    pinned: bool,
}
async fn pin_chat(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(v): Json<Pin>,
) -> Result<Json<crate::chats::Chat>> {
    app.db.pin_chat(&id, v.pinned)?;
    Ok(Json(app.db.chat(&id)?))
}
async fn pin_bot(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(v): Json<Pin>,
) -> Result<Json<Bot>> {
    app.db.pin_bot(&id, v.pinned)?;
    Ok(Json(app.db.bot(&id)?))
}
async fn attention(State(app): State<Shared>) -> Result<Json<Value>> {
    Ok(Json(app.db.attention()?))
}
#[derive(Deserialize)]
struct ReadChat {
    cursor: i64,
}
async fn mark_chat_read(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(value): Json<ReadChat>,
) -> Result<Json<Value>> {
    app.db.mark_chat_read(&id, value.cursor)?;
    Ok(Json(app.db.attention()?["chats"][&id].clone()))
}
#[derive(Deserialize)]
struct BotIdentity {
    name: String,
    label: String,
    description: String,
    notifications: bool,
}
async fn update_bot_identity(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(value): Json<BotIdentity>,
) -> Result<Json<Bot>> {
    app.db.save_bot_identity(
        &id,
        &value.name,
        &value.label,
        &value.description,
        value.notifications,
    )?;
    Ok(Json(app.db.bot(&id)?))
}
async fn create_chat(
    State(app): State<Shared>,
    Json(mut c): Json<crate::chats::Chat>,
) -> Result<Json<crate::chats::Chat>> {
    c.id = db::id();
    app.db.save_chat(&c)?;
    Ok(Json(app.db.chat(&c.id)?))
}
async fn update_chat(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(value): Json<Value>,
) -> Result<Json<crate::chats::Chat>> {
    let existing = app.db.chat(&id)?;
    let mut c: crate::chats::Chat = serde_json::from_value(value.clone())?;
    if value.get("bot_only").is_none() { c.bot_only = existing.bot_only; }
    if value.get("description").is_none() { c.description = existing.description; }
    c.id = id;
    app.db.save_chat(&c)?;
    Ok(Json(app.db.chat(&c.id)?))
}
#[derive(Deserialize)]
struct MessagePage {
    before: Option<i64>,
    after: Option<i64>,
    limit: Option<usize>,
    #[serde(default)]
    inclusive: bool,
}
async fn chat(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Query(page): Query<MessagePage>,
) -> Result<Json<Value>> {
    if page.inclusive {
        return Ok(Json(app.db.chat_message_window(
            &id,
            page.before,
            page.after,
            page.limit.unwrap_or(50),
            true,
        )?));
    }
    Ok(Json(app.db.chat_message_page(
        &id,
        page.before,
        page.after,
        page.limit.unwrap_or(50),
    )?))
}
#[derive(Deserialize)]
struct ChatMessage {
    prompt: String,
    #[serde(default)]
    mentions: Vec<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    reply_to: Option<i64>,
    #[serde(default)]
    request_id: Option<String>,
}
async fn chat_send(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(v): Json<ChatMessage>,
) -> Result<Json<Value>> {
    Ok(Json(
        json!({"runs":app.db.chat_send_request(&id,&v.prompt,&v.mentions,&v.files,v.reply_to,v.request_id.as_deref())?}),
    ))
}
async fn message_reaction(
    State(app): State<Shared>,
    Path((id, seq)): Path<(String, i64)>,
    Json(v): Json<crate::message_actions::Reaction>,
) -> Result<Json<Value>> {
    app.db.user_react(&id, seq, v.emoji.as_deref())?;
    Ok(Json(json!({"ok":true})))
}
async fn upload_file(State(app): State<Shared>, Json(v): Json<Value>) -> Result<Json<Value>> {
    Ok(Json(app.db.upload_file(
        runtime::string(&v, "chat_id")?,
        runtime::string(&v, "name")?,
        runtime::string(&v, "data")?,
    )?))
}
async fn remove_upload(State(app): State<Shared>, Path(id): Path<String>) -> Result<Json<Value>> {
    app.db.remove_upload(&id)?;
    Ok(Json(json!({"ok":true})))
}
async fn download_deliverable(
    State(app): State<Shared>,
    Path(id): Path<String>,
) -> Result<Response> {
    let (name, bytes) = app.db.file_download(&id)?;
    Ok(download_file_response(name, bytes))
}
async fn download_upload(State(app): State<Shared>, Path(id): Path<String>) -> Result<Response> {
    let (name, bytes) = app.db.upload_bytes(&id)?;
    Ok(download_file_response(name, bytes))
}
pub(crate) fn download_file_response(name: String, bytes: Vec<u8>) -> Response {
    let encoded: String = name.bytes().map(|b| format!("%{b:02X}")).collect();
    (
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename*=UTF-8''{encoded}"),
            ),
            (header::CACHE_CONTROL, "no-store".to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        bytes,
    )
        .into_response()
}
async fn resources(State(app): State<Shared>) -> Result<Json<Value>> {
    Ok(Json(
        vm::guest(&app.config.vm, "computer_resources", json!({})).await?,
    ))
}
#[derive(Deserialize)]
struct MarketQuery {
    #[serde(default)]
    account_id: String,
    #[serde(default)]
    search: String,
    #[serde(default)]
    cursor: String,
}
async fn marketplace(
    State(app): State<Shared>,
    Query(q): Query<MarketQuery>,
) -> Result<Json<Value>> {
    Ok(Json(
        crate::composio::marketplace(&app, &q.search, &q.cursor).await?,
    ))
}

async fn marketplace_detail(
    State(app): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    Ok(Json(crate::composio::marketplace_detail(&app, &id).await?))
}

async fn marketplace_configs(
    State(app): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    Ok(Json(crate::composio::auth_configs(&app, &id).await?))
}

// Signed public release files contain no account or workspace data.
async fn release_file(State(app): State<Shared>, Path(filename): Path<String>) -> Response {
    release_asset(&app.config.database, filename).await
}
pub async fn release_asset(database: &str, filename: String) -> Response {
    let manifest = matches!(filename.as_str(), "stable.json" | "client-stable.json" | "client-linux.json");
    let package = [
        ("kindred-windows-", ".zip"),
        ("kindred-linux-x86_64-", ".AppImage"),
        ("kindred-macos-aarch64-", ".dmg"),
        ("kindred-macos-x86_64-", ".dmg"),
    ]
    .iter()
    .any(|(prefix, suffix)| {
        filename
            .strip_prefix(prefix)
            .and_then(|v| v.strip_suffix(suffix))
            .is_some_and(|v| {
                let p: Vec<_> = v.split('.').collect();
                p.len() == 3
                    && p.iter().all(|n| {
                        !n.is_empty() && n.len() <= 5 && n.bytes().all(|b| b.is_ascii_digit())
                    })
            })
    });
    if !manifest && !package {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(response) = crate::release_github::fetch(&filename).await {
        return response;
    }
    let root = std::path::Path::new(database)
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("releases");
    let path = root.join(&filename);
    let meta = match tokio::fs::symlink_metadata(&path).await {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => meta,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let maximum = if manifest {
        65536
    } else if filename.starts_with("kindred-windows-") {
        67108864
    } else {
        536870912
    };
    if meta.len() > maximum {
        return StatusCode::NOT_FOUND.into_response();
    }
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let mime = if manifest {
        "application/json"
    } else if filename.ends_with(".zip") {
        "application/zip"
    } else {
        "application/octet-stream"
    };
    // Large native packages stream with backpressure; concurrent clients do not
    // allocate a complete AppImage/DMG in server memory for each download.
    Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, meta.len())
        .body(Body::from_stream(tokio_util::io::ReaderStream::new(file)))
        .unwrap()
}

async fn marketplace_tools(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<MarketQuery>,
) -> Result<Json<Value>> {
    Ok(Json(
        crate::composio::catalog_account(&app, &id, &q.account_id, &q.search, &q.cursor).await?,
    ))
}

async fn user_tasks(State(app): State<Shared>) -> Result<Json<Vec<crate::user_tasks::UserTask>>> {
    Ok(Json(app.db.user_tasks()?))
}
// User input goes directly to the paused screen, never through run events or a model tool call.
async fn enter_user_task_code(State(app): State<Shared>, Path(id): Path<String>, Json(v): Json<Value>) -> Result<Json<Value>> {
    let task = app.db.user_task(&id)?;
    require!(v["bot_id"] == task.bot_id && v["run_id"] == task.run_id, "This subtask belongs to another bot or run");
    require!(matches!(task.authentication.as_ref().and_then(|a| a["method"].as_str()), Some("sms" | "email" | "authenticator")), "This subtask does not accept a code");
    let code = v["code"].as_str().unwrap_or("").trim();
    require!((4..=16).contains(&code.len()) && code.bytes().all(|c| c.is_ascii_alphanumeric()), "Enter the 4–16 character verification code");
    if let Some(length) = task.authentication.as_ref().and_then(|a| a["code_length"].as_u64()) {
        require!(code.len() == length as usize, "Enter the complete verification code with the expected character count");
    }
    let slot = app.db.screen(&task.bot_id)?;
    screen_ready(&app, slot)?;
    let lock = app.screen_lock(slot);
    let _lease = tokio::time::timeout(std::time::Duration::from_secs(15), lock.lock()).await
        .map_err(|_| anyhow::anyhow!("Screen is busy. Try again shortly."))?;
    let current = app.db.user_task(&id)?;
    require!(current.status == "pending" && app.db.run(&current.run_id)?.status == "awaiting_user" && !app.db.screen_takeover(slot)?, "This verification step is no longer waiting, or the computer is under manual control");
    app.db.reserve_code_entry(&current)?;
    // Do not return guest output/errors: they may contain the supplied text.
    if vm::guest_screen(&app.config.vm, slot, "computer_type", json!({"text":code})).await.is_err() {
        return Err(anyhow::anyhow!("Could not confirm code entry. Check the computer before trying again.").into());
    }
    if current.authentication.as_ref().and_then(|a| a["submission"].as_str()) != Some("automatic") {
        if vm::guest_screen(&app.config.vm, slot, "computer_key", json!({"key":"Return"})).await.is_err() {
            return Err(anyhow::anyhow!("Code entry may have succeeded, but submission could not be confirmed. Check the computer.").into());
        }
    }
    app.db.ready_user_task(&current, slot, "done")?;
    Ok(Json(json!({"ok":true,"status":"ready"})))
}
async fn complete_user_task(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(v): Json<Value>,
) -> Result<Json<Value>> {
    let task = app.db.user_task(&id)?;
    require!(
        v["bot_id"] == task.bot_id && v["run_id"] == task.run_id,
        "This subtask belongs to another bot or run"
    );
    let outcome = v["outcome"].as_str().unwrap_or("done");
    require!(
        matches!(outcome, "done" | "skipped"),
        "Unknown subtask outcome"
    );
    if matches!(task.status.as_str(), "ready" | "resumed") {
        require!(
            task.outcome == outcome,
            "This subtask already has a different outcome"
        );
        return Ok(Json(json!({"ok":true,"status":task.status})));
    }
    require!(
        task.status == "pending" && app.db.run(&task.run_id)?.status == "awaiting_user",
        "This subtask is no longer waiting"
    );
    let slot = app.db.screen(&task.bot_id)?;
    app.vnc.close_controls_for(slot);
    let lock = app.screen_lock(slot);
    let _lease = tokio::time::timeout(std::time::Duration::from_secs(15), lock.lock())
        .await
        .map_err(|_| anyhow::anyhow!("Screen is still disconnecting. Retry Done with subtask."))?;
    // Recheck after waiting so concurrent repeats cannot complete a new task.
    let current = app.db.user_task(&id)?;
    if matches!(current.status.as_str(), "ready" | "resumed") && current.outcome == outcome {
        return Ok(Json(json!({"ok":true,"status":current.status})));
    }
    app.db.ready_user_task(&current, slot, outcome)?;
    Ok(Json(json!({"ok":true,"status":"ready"})))
}

pub fn assets() -> Router {
    static_assets()
}
fn static_assets<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/artifact-frame.html", get(|| async { Html(include_str!("../ui/artifact-frame.html")) }))
        .route("/artifacts", get(|| async { Html(include_str!("../ui/index.html")) }))
        .route("/artifacts/{id}", get(|| async { Html(include_str!("../ui/index.html")) }))
        .route(
            "/connector-catalog.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/javascript")],
                    include_str!("../ui/connector-catalog.js"),
                )
            }),
        )
        .route(
            "/connector-cards.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/connector-cards.js"),
                )
            }),
        )
        .route("/workspace-artifacts.js", get(|| async { ([(header::CONTENT_TYPE,"text/javascript")], include_str!("../ui/workspace-artifacts.js")) }))
        .route(
            "/artifacts.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/artifacts.js"),
                )
            }),
        )
        .route(
            "/reading-size.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/reading-size.js"),
                )
            }),
        )
        .route("/group-activity.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../ui/group-activity.js")) }))
        .route("/decision-receipts.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../ui/decision-receipts.js")) }))
        .route(
            "/visual-panels.js",
            get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../ui/visual-panels.js")) }),
        )
        .route(
            "/computer-pointer.js",
            get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../ui/computer-pointer.js")) }),
        )
        .route(
            "/document-preview.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/document-preview.js"),
                )
            }),
        )
        .route(
            "/document-vendor.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/document-vendor.js"),
                )
            }),
        )
        .route(
            "/document-worker.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/document-worker.js"),
                )
            }),
        )
        .route(
            "/pdf-worker.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/pdf-worker.js"),
                )
            }),
        )
        .route(
            "/artifact-vendor.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/artifact-vendor.js"),
                )
            }),
        )
        .route(
            "/select-menu.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/select-menu.js"),
                )
            }),
        )
        .route(
            "/select-menu.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../ui/select-menu.css"),
                )
            }),
        )
        .route(
            "/server-chats.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/server-chats.js"),
                )
            }),
        )
        .route(
            "/dictation.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/dictation.js"),
                )
            }),
        )
        .route(
            "/workspace-import.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/workspace-import.js"),
                )
            }),
        )
        .route(
            "/profiles.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/profiles.js"),
                )
            }),
        )
        .route(
            "/commands.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/commands.js"),
                )
            }),
        )
        .route(
            "/composer-text.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/composer-text.js"),
                )
            }),
        )
        .route(
            "/favicon.svg",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../ui/favicon.svg"),
                )
            }),
        )
        .route(
            "/audio/kindred-pop.wav",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "audio/wav")],
                    include_bytes!("../ui/audio/kindred-pop.wav").as_slice(),
                )
            }),
        )
        .route(
            "/updates.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/updates.js"),
                )
            }),
        )
        .route(
            "/claude-mark.svg",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../ui/claude-mark.svg"),
                )
            }),
        )
        .route(
            "/openrouter-mark.svg",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../ui/openrouter-mark.svg"),
                )
            }),
        )
        .route(
            "/kimi-mark.svg",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../ui/kimi-mark.svg"),
                )
            }),
        )
        .route(
            "/openai-white.svg",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../ui/openai-white.svg"),
                )
            }),
        )
        .route(
            "/openai-black.svg",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../ui/openai-black.svg"),
                )
            }),
        )
        .route(
            "/avatar-data.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../ui/avatar-data.js"),
                )
            }),
        )
        .route(
            "/tributes.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/tributes.js"),
                )
            }),
        )
        .route(
            "/characters.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/characters.js"),
                )
            }),
        )
        .route(
            "/",
            get(|| async { Html(include_str!("../ui/index.html")) }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../ui/style.css"),
                )
            }),
        )
        .route(
            "/fonts.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../ui/fonts.css"),
                )
            }),
        )
        .route(
            "/fonts/LiberationMono-Regular.ttf",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "font/ttf")],
                    include_bytes!("../ui/fonts/LiberationMono-Regular.ttf").as_slice(),
                )
            }),
        )
        .route(
            "/fonts/LiberationMono-Bold.ttf",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "font/ttf")],
                    include_bytes!("../ui/fonts/LiberationMono-Bold.ttf").as_slice(),
                )
            }),
        )
        .route(
            "/fonts/LiberationMono-Italic.ttf",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "font/ttf")],
                    include_bytes!("../ui/fonts/LiberationMono-Italic.ttf").as_slice(),
                )
            }),
        )
        .route(
            "/fonts/LiberationMono-BoldItalic.ttf",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "font/ttf")],
                    include_bytes!("../ui/fonts/LiberationMono-BoldItalic.ttf").as_slice(),
                )
            }),
        )
        .route(
            "/fonts/InterVariable.woff2",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "font/woff2")],
                    include_bytes!("../ui/fonts/InterVariable.woff2").as_slice(),
                )
            }),
        )
        .route(
            "/fonts/InterVariable-Italic.woff2",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "font/woff2")],
                    include_bytes!("../ui/fonts/InterVariable-Italic.woff2").as_slice(),
                )
            }),
        )
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route(
            "/vendor.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/vendor.js"),
                )
            }),
        )
}

async fn maintenance(State(app): State<Shared>) -> Result<Json<crate::vm_maintenance::State>> {
    Ok(Json(crate::vm_maintenance::state(&app.db)?))
}
async fn request_maintenance(State(app): State<Shared>) -> Result<Json<crate::vm_maintenance::State>> {
    Ok(Json(crate::vm_maintenance::request(&app.db)?))
}
async fn save_maintenance(
    State(app): State<Shared>,
    Json(value): Json<Value>,
) -> Result<Json<crate::vm_maintenance::State>> {
    let enabled = value["enabled"]
        .as_bool()
        .ok_or_else(|| anyhow::anyhow!("Missing automatic update preference"))?;
    Ok(Json(crate::vm_maintenance::set_enabled(&app.db, enabled)?))
}

async fn workflow_response(State(app): State<Shared>, Path((chat,id)): Path<(String,String)>, Json(v): Json<Value>) -> Result<Json<Value>> {
    Ok(Json(crate::workflow_panels::respond(&app.db,&chat,&id,&v)?))
}

async fn delete_archived_bot(State(app): State<Shared>, Path(id): Path<String>, Json(v): Json<Value>) -> Result<Json<Value>> {
    if v["confirmed"] != true { return Err(anyhow::anyhow!("Confirm permanent deletion first").into()); }
    app.db.delete_archived_bot(&id, runtime::string(&v,"name")?)?;
    Ok(Json(json!({"deleted":true})))
}
