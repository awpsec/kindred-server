//! Profile-owned portable workflow packages. Imports only save data. Scripts run
//! later through the ordinary bot tools, under their ordinary action policy.
use crate::{
    db::{Bot, Db, Run},
    runtime::App,
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::digest::{SHA256, digest};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[path = "import_arguments.rs"]
pub(crate) mod arguments;

const SHELL_WARNING: &str = "Dynamic !`shell` context is retained as instructions, not executed during import or expansion. Gather required context through normal permission-checked tools on the correct source computer. A failed command, unavailable computer or missing permission is a visible blocker, never empty successful output.";
fn warnings(package: &Value) -> Value {
    let mut result = package["warnings"].as_array().cloned().unwrap_or_default();
    result.retain(|v| {
        !v.as_str().is_some_and(|s| {
            s.starts_with("Dynamic !`shell`") || s.starts_with("Legacy positional arguments:")
        })
    });
    if arguments::dynamic_shell(package["body"].as_str().unwrap_or("")) {
        result.push(json!(SHELL_WARNING));
    }
    if arguments::inferred_legacy(package) {
        result.push(json!("Legacy positional arguments: this command uses $1 for the first input. Change Positional arguments in Settings → Skills if its author intended modern $0 indexing."));
    }
    json!(result)
}

pub(crate) fn argument_conventions(package: &Value) -> &'static str {
    let path = package["source"]
        .as_str()
        .unwrap_or("")
        .replace('\\', "/")
        .to_lowercase();
    if package["argument_index"] == 0 {
        "This workflow uses zero-based $0 / $ARGUMENTS[0] for the first parsed input. $ARGUMENTS preserves all raw input. Use the persisted placeholder bindings, not a guessed order."
    } else if arguments::inferred_legacy(package) || package["argument_index"] == 1 {
        "This workflow uses one-based $1, $2, etc. $ARGUMENTS preserves all raw input; $ARGUMENTS[N] is always zero-based. Use the persisted placeholder bindings, not a guessed order."
    } else if path.split('/').any(|p| p == ".pi") {
        "Pi prompt templates use one-based $1, $2, etc.; $@ and $ARGUMENTS include all arguments. ${1:-default}, ${@:-default}, ${ARGUMENTS:-default}, ${@:N} and ${@:N:L} retain Pi defaults/slicing semantics. A SKILL.md uses the supplied arguments as user context."
    } else if path.split('/').any(|p| p == ".codex") {
        "Codex prompt templates use one-based $1 through $9 and $ARGUMENTS for all arguments. Preserve named placeholders and ask for missing required inputs. A SKILL.md uses the supplied arguments as user context."
    } else if path.split('/').any(|p| p == ".claude")
        || package["body"]
            .as_str()
            .is_some_and(|s| s.contains("CLAUDE_") || s.contains("$ARGUMENTS["))
    {
        "Claude commands use $ARGUMENTS for raw input and zero-based $0 / $ARGUMENTS[0] for the first parsed input. ${CLAUDE_SKILL_DIR} means the directory returned by skill_load."
    } else {
        "Preserve the source workflow's documented argument conventions. Use the supplied argument list as user context; ask about ambiguous placeholders instead of assuming a harness."
    }
}
fn hash(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 400
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != ".." && !s.starts_with('.'))
}
pub(crate) fn parse(package: &Value) -> Result<Value> {
    parse_with_limit(package, 48000)
}
pub(crate) fn parse_workspace(package: &Value) -> Result<Value> {
    // The converter can condense a large source entry before the ordinary saved
    // workflow/message limits apply. Preserve source text for its review.
    parse_with_limit(package, 256 * 1024)
}
fn parse_with_limit(package: &Value, instruction_limit: usize) -> Result<Value> {
    let entry = package["entry"].as_str().context("Missing entry file")?;
    ensure!(
        safe_path(entry) && entry.ends_with(".md"),
        "Choose a command .md file or SKILL.md"
    );
    let source = package["source"].as_str().unwrap_or(entry);
    ensure!(
        source.len() <= 4000 && !source.chars().any(char::is_control),
        "Invalid source path"
    );
    let input = package["files"]
        .as_object()
        .context("Missing package files")?;
    ensure!(
        !input.is_empty() && input.len() <= 128,
        "A package needs 1–128 files"
    );
    let mut files = BTreeMap::new();
    let mut total = 0;
    let mut body = None;
    for (path, data) in input {
        ensure!(safe_path(path), "Invalid package path: {path}");
        let name = path.rsplit('/').next().unwrap().to_lowercase();
        ensure!(
            !matches!(
                name.as_str(),
                "credentials.json" | "auth.json" | "id_rsa" | "id_ed25519"
            ) && !name.ends_with(".pem")
                && !name.ends_with(".key"),
            "Credential files are not part of an imported workflow"
        );
        let encoded = data.as_str().context("File content must be base64")?;
        ensure!(
            encoded.len() <= 3 * 1024 * 1024,
            "An imported file exceeds 2 MiB"
        );
        let bytes = STANDARD.decode(encoded).context("Invalid file encoding")?;
        ensure!(
            bytes.len() <= 2 * 1024 * 1024,
            "An imported file exceeds 2 MiB"
        );
        total += bytes.len();
        ensure!(total <= 8 * 1024 * 1024, "A package exceeds 8 MiB");
        if path == entry {
            body = Some(String::from_utf8(bytes.clone()).context("Instructions must be UTF-8")?);
        }
        files.insert(path.clone(), STANDARD.encode(bytes));
    }
    let raw = body.context("Entry file is not in package")?;
    ensure!(
        raw.len() <= instruction_limit && !raw.contains('\0'),
        "Instructions must fit in {instruction_limit} bytes"
    );
    let normalized = raw.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let (metadata, body) = if let Some(rest) = normalized.strip_prefix("---\n") {
        let (front, body) = rest
            .split_once("\n---")
            .context("Unclosed YAML frontmatter")?;
        ensure!(
            body.is_empty() || body.starts_with('\n'),
            "Frontmatter must end on its own line"
        );
        let yaml: Value = serde_yaml::from_str(front).context("Invalid YAML frontmatter")?;
        ensure!(
            yaml.is_object() || yaml.is_null(),
            "Frontmatter must be a mapping"
        );
        (yaml, body.trim_start_matches('\n').to_owned())
    } else {
        (json!({}), normalized)
    };
    ensure!(!body.trim().is_empty(), "Skill instructions are empty");
    let default_name = entry
        .strip_suffix("/SKILL.md")
        .and_then(|s| s.rsplit('/').next())
        .or_else(|| entry.strip_suffix(".md"))
        .unwrap_or("imported-skill");
    let name = metadata["name"]
        .as_str()
        .or(package["name"].as_str())
        .unwrap_or(default_name);
    ensure!(
        !name.trim().is_empty() && name.len() <= 100,
        "Skill name must fit in 100 bytes"
    );
    let mut command = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    command = command.trim_matches('-').chars().take(42).collect();
    if !crate::commands::slug(&command) || crate::commands::reserved(&command) {
        command = format!("skill-{command}");
    }
    let mut warnings = Vec::new();
    for field in [
        "allowed-tools",
        "model",
        "context",
        "agent",
        "hooks",
        "effort",
    ] {
        if metadata.get(field).is_some() {
            warnings.push(format!("{field}: retained as source metadata. Kindred uses the bot's configured tools, model and action permissions; this source setting is not applied automatically."));
        }
    }
    if arguments::dynamic_shell(&body) {
        warnings.push(SHELL_WARNING.into());
    }
    if body.contains("$CLAUDE_PROJECT_DIR")
        || body.contains("${CLAUDE_PROJECT_DIR}")
        || body.contains("${CLAUDE_SESSION_ID}")
        || body.contains("${CLAUDE_PLUGIN_ROOT}")
    {
        warnings.push("This workflow references a Claude project, plugin or session variable. Supply the equivalent project location or adapt it before use.".into());
    }
    if body.contains("mcp__") || body.contains("mcp:") {
        warnings.push("This workflow references an MCP tool. The selected bot needs an equivalent connected tool or an adapted step.".into());
    }
    let disable = metadata["disable-model-invocation"]
        .as_bool()
        .unwrap_or(false);
    let user = metadata["user-invocable"].as_bool().unwrap_or(true);
    let fingerprint = hash(&serde_json::to_vec(&json!({"entry":entry,"files":files}))?);
    let description = metadata["description"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            body.lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .chars()
                .take(180)
                .collect()
        });
    ensure!(
        description.len() <= 600,
        "Description must fit in 600 bytes"
    );
    Ok(
        json!({"version":1,"name":name,"command":command,"description":description,"body":body,"entry":entry,"source":source,"files":files,"hash":fingerprint,"bytes":total,"metadata":metadata,"warnings":warnings,"disable_model_invocation":disable,"user_invocable":user,"argument_hint":metadata["argument-hint"].as_str().unwrap_or("")}),
    )
}
pub fn summary(v: &Value) -> Value {
    if v.is_null() {
        return Value::Null;
    }
    json!({"source":v["source"],"origin":v["origin"],"source_hash":v["source_hash"],"refreshable":v["origin"]["device_id"].is_string(),"hash":v["hash"],"files":v["files"].as_object().map(|f|f.len()).unwrap_or(0),"bytes":v["bytes"],"warnings":warnings(v),"argument_index":arguments::index(v),"user_invocable":v["user_invocable"],"disable_model_invocation":v["disable_model_invocation"],"argument_hint":v["argument_hint"],"edited":v["edited"],"file_names":v["files"].as_object().map(|f|f.keys().collect::<Vec<_>>()).unwrap_or_default()})
}
pub fn load(c: &Connection, name: &str) -> Result<Value> {
    let data: Option<String> = c
        .query_row("SELECT import_data FROM skills WHERE name=?", [name], |r| {
            r.get(0)
        })
        .optional()?;
    let mut package: Value = data
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    if package.is_object() {
        package["warnings"] = warnings(&package);
    }
    Ok(package)
}

pub(crate) fn set_argument_index(c: &Connection, name: &str, value: &Value) -> Result<()> {
    ensure!(
        matches!(value.as_u64(), Some(0 | 1)),
        "Choose $0 or $1 as the first positional argument"
    );
    let mut package = load(c, name)?;
    ensure!(
        package.is_object(),
        "Positional indexing applies to imported workflows"
    );
    package["argument_index"] = value.clone();
    package["warnings"] = warnings(&package);
    c.execute(
        "UPDATE skills SET import_data=? WHERE name=?",
        params![package.to_string(), name],
    )?;
    Ok(())
}
pub fn sync_edit(c: &Connection, name: &str, body: &str) -> Result<()> {
    let mut package = load(c, name)?;
    if package.is_null() {
        return Ok(());
    }
    if !package.is_null() && package["body"] != body {
        package["body"] = json!(body);
        package["edited"] = json!(true);
        let entry = package["entry"]
            .as_str()
            .context("Missing imported entry")?
            .to_owned();
        package["files"][entry] = json!(STANDARD.encode(body.as_bytes()));
        package["hash"] = json!(hash(&serde_json::to_vec(
            &json!({"entry":package["entry"],"files":package["files"]})
        )?));
    }
    let (command, description): (String, String) = c.query_row(
        "SELECT command,description FROM skills WHERE name=?",
        [name],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    package["command"] = json!(command);
    package["description"] = json!(description);
    c.execute(
        "UPDATE skills SET import_data=? WHERE name=?",
        params![package.to_string(), name],
    )?;
    Ok(())
}
pub fn request(db: &Db, v: &Value) -> Result<Value> {
    request_with_origin(db, v, None)
}
pub(crate) fn request_with_origin(db: &Db, v: &Value, origin: Option<&Value>) -> Result<Value> {
    let mut package = parse(&v["package"])?;
    package["source_hash"] = package["hash"].clone();
    package["source_description"] = package["description"].clone();
    package["origin"] = origin.cloned().unwrap_or(Value::Null);
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    let name = v["name"]
        .as_str()
        .unwrap_or(package["name"].as_str().unwrap())
        .trim()
        .to_owned();
    let command = v["command"]
        .as_str()
        .unwrap_or(package["command"].as_str().unwrap())
        .trim()
        .trim_start_matches('/')
        .to_owned();
    ensure!(
        !name.is_empty() && name.len() <= 100 && !name.chars().any(char::is_control),
        "Invalid skill name"
    );
    ensure!(
        crate::commands::slug(&command) && !crate::commands::reserved(&command),
        "Choose a lowercase command name, up to 48 characters, that is not built in"
    );
    package["name"] = json!(name);
    package["command"] = json!(command);
    let old = load(&tx, &name)?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM skills WHERE name=?)",
        [&name],
        |r| r.get(0),
    )?;
    let conflict: Option<String> = tx
        .query_row(
            "SELECT name FROM skills WHERE command=? AND name!=?",
            params![command, name],
            |r| r.get(0),
        )
        .optional()?;
    let mut preview = json!({"name":name,"command":command,"body":package["body"],"description":package["description"],"import":summary(&package),"files":package["files"].as_object().unwrap().keys().collect::<Vec<_>>(),"parameters":arguments::parameters(&package,vec![crate::commands::Parameter{name:"arguments".into(),description:String::new(),required:false,rest:true}]),"existing":exists,"existing_hash":old["hash"],"command_conflict":conflict});
    if v["action"] == "preview" {
        return Ok(preview);
    }
    ensure!(v["action"] == "import", "Choose preview or import");
    ensure!(
        v["expected_hash"] == package["hash"],
        "Files changed since review. Preview again before importing"
    );
    ensure!(
        conflict.is_none(),
        "Another skill uses that command. Choose another command name"
    );
    if exists && old["origin"].is_object() && origin.is_some() {
        ensure!(
            old["origin"] == package["origin"],
            "This workflow is linked to a different source. Refresh its saved source or choose a new import name; do not silently rebind it."
        );
    }
    if exists && old["hash"] == package["hash"] && old["command"] == command {
        if old["origin"].is_null() && origin.is_some() {
            let mut linked = old.clone();
            linked["origin"] = package["origin"].clone();
            linked["source_hash"] = package["hash"].clone();
            linked["source_description"] = package["description"].clone();
            tx.execute(
                "UPDATE skills SET import_data=? WHERE name=?",
                params![linked.to_string(), name],
            )?;
            tx.commit()?;
        }
        preview["status"] = json!("unchanged");
        return Ok(preview);
    }
    ensure!(
        !exists
            || (!old.is_null()
                && v["replace_hash"]
                    .as_str()
                    .is_some_and(|s| Some(s) == old["hash"].as_str())),
        "A skill with this name already exists. Rename the import or review and explicitly replace the current imported version"
    );
    ensure!(
        exists
            || tx.query_row("SELECT count(*) FROM skills", [], |r| r.get::<_, i64>(0))?
                < crate::commands::MAX_WORKFLOWS as i64,
        "This profile has reached its 256-workflow limit"
    );
    let parameters = json!([{"name":"arguments","description":package["argument_hint"],"required":false,"rest":true}]);
    tx.execute("INSERT INTO skills(name,body,command,description,parameters,import_data) VALUES(?,?,?,?,?,?) ON CONFLICT(name) DO UPDATE SET body=excluded.body,command=excluded.command,description=excluded.description,parameters=excluded.parameters,import_data=excluded.import_data",params![name,package["body"].as_str().unwrap(),command,package["description"].as_str().unwrap(),parameters.to_string(),package.to_string()])?;
    tx.commit()?;
    preview["status"] = json!("imported");
    Ok(preview)
}
pub fn model_catalog(db: &Db) -> Result<Vec<Value>> {
    Ok(db
        .skills()?
        .into_iter()
        .filter(|s| s["import"]["disable_model_invocation"] != true)
        .map(|mut s| {
            if !s["import"].is_null() {
                s.as_object_mut().unwrap().remove("body");
            }
            s
        })
        .collect())
}
pub fn message_command(c: &Connection, seq: i64) -> Result<Value> {
    let receipt:Option<String>=c.query_row("SELECT rc.receipt FROM run_commands rc JOIN run_message_sources s ON s.run_id=rc.run_id WHERE s.message_seq=? LIMIT 1",[seq],|r|r.get(0)).optional()?;
    let v: Value = receipt
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    Ok(if v["command"].is_string() {
        json!({"command":v["command"],"source":v["source"],"skill_name":v["skill_name"],"hash":v["import_bundle"]["hash"]})
    } else {
        Value::Null
    })
}
pub async fn local(app: &App, bot: &Bot, run: &Run, args: &Value) -> Result<Value> {
    let result = crate::local_access::call(
        app,
        bot,
        run,
        "local_skill_bundle",
        json!({"path":args["path"],"device_id":args["device_id"]}),
    )
    .await?;
    ensure!(
        result["failed"] != true,
        "{}",
        result["text"]
            .as_str()
            .unwrap_or("Local package read failed")
    );
    let mut input = args.clone();
    input["package"] = result;
    let origin = json!({"kind":"local_desktop","device_id":input["package"]["kindred_device_id"],"path":input["package"]["source"]});
    ensure!(
        origin["device_id"].is_string() && origin["path"].is_string(),
        "Missing verified local source identity"
    );
    let result = request_with_origin(&app.db, &input, Some(&origin))?;
    Ok(json!({"text":serde_json::to_string(&result)?}))
}
// Fixed writer only; filenames and bytes use stdin and directory descriptors.
// An invocation gets its own directory and never replaces another run's files.
const WRITE: &str = r#"import os,sys,json,base64,uuid
v=json.load(sys.stdin)
assert str(uuid.UUID(v['run']))==v['run']
assert len(v['hash'])==64 and all(c in '0123456789abcdef' for c in v['hash'])
root=os.open('/workspace',os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
for part in ('.kindred-skills',v['run'],v['hash']):
 try: os.mkdir(part,0o700,dir_fd=root)
 except FileExistsError: pass
 child=os.open(part,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=root);os.close(root);root=child
for path,encoded in v['files'].items():
 parts=path.split('/');assert all(p and p not in ('.','..') and '\\' not in p for p in parts)
 parent=os.dup(root)
 for part in parts[:-1]:
  try: os.mkdir(part,0o700,dir_fd=parent)
  except FileExistsError: pass
  child=os.open(part,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=parent);os.close(parent);parent=child
 data=base64.b64decode(encoded,validate=True)
 try: fd=os.open(parts[-1],os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600,dir_fd=parent)
 except FileExistsError:
  fd=os.open(parts[-1],os.O_RDONLY|os.O_NOFOLLOW,dir_fd=parent)
  with os.fdopen(fd,'rb') as f: assert f.read(len(data)+1)==data,'Saved skill file changed; use a fresh task'
 else:
  with os.fdopen(fd,'wb') as f: f.write(data)
 os.close(parent)
os.close(root)
print(json.dumps({'path':'/workspace/.kindred-skills/'+v['run']+'/'+v['hash']}))
"#;
fn run_package(db: &Db, run: &Run, name: &str) -> Result<Value> {
    let c = db.0.lock().unwrap();
    let snap: Option<String> = c
        .query_row(
            "SELECT receipt FROM run_commands WHERE run_id=?",
            [&run.id],
            |r| r.get(0),
        )
        .optional()?;
    let snap: Value = snap
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    if snap["skill_name"] == name {
        if !snap["import_bundle"].is_null() {
            return Ok(snap["import_bundle"].clone());
        }
        if let Some(body) = snap["instructions"].as_str() {
            return Ok(json!({"instructions_only":true,"body":body}));
        }
    }
    let package = load(&c, name)?;
    if package.is_null() {
        // Taught and manually authored skills have no imported package or VM
        // files. They must still be loadable through the same workflow tool.
        let skill: Option<(String, String)> = c
            .query_row(
                "SELECT body,parameters FROM skills WHERE name=?",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (body, parameters) =
            skill.context("Skill not found; use skills_list to find its exact name")?;
        return Ok(
            json!({"instructions_only":true,"body":body,"parameters":serde_json::from_str::<Value>(&parameters)?}),
        );
    }
    ensure!(
        package["disable_model_invocation"] != true,
        "This imported workflow requires an explicit slash invocation by the user"
    );
    Ok(package)
}
fn invocation_instructions(db: &Db, run: &Run, name: &str, package: &Value) -> Result<String> {
    let receipt: Option<String> =
        db.0.lock()
            .unwrap()
            .query_row(
                "SELECT receipt FROM run_commands WHERE run_id=?",
                [&run.id],
                |r| r.get(0),
            )
            .optional()?;
    let receipt: Value = receipt
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if receipt["skill_name"] == name {
        if let Some(body) = receipt["expanded_instructions"].as_str() {
            return Ok(format!(
                "{}\n\nNamed input values (JSON): {}\nResolved placeholder values (JSON): {}\nValues above were inserted once at invocation; do not expand them again.",
                body, receipt["arguments"], receipt["placeholder_values"]
            ));
        }
    }
    Ok(package["body"].as_str().unwrap_or("").into())
}
pub async fn materialize(app: &App, run: &Run, name: &str) -> Result<Value> {
    let package = run_package(&app.db, run, name)?;
    let instructions = invocation_instructions(&app.db, run, name, &package)?;
    if package["instructions_only"] == true {
        return Ok(json!({
            "text":format!("Saved workflow {name}. Apply it only for the user's requested task. Resolve any missing inputs before acting and follow the normal approval policy.\nParameters: {}\nInstructions:\n{}",package["parameters"],instructions),
            "name":name,"instructions":instructions,"files":[],"path":null
        }));
    }
    let mut cmd = crate::vm::ssh(&app.config.vm);
    cmd.arg(format!("python3 -c '{}'", WRITE.replace('\'', "'\\''")));
    let input =
        serde_json::to_vec(&json!({"run":run.id,"hash":package["hash"],"files":package["files"]}))?;
    let result: Value =
        serde_json::from_slice(&crate::vm::capture(cmd, Some(input), 60, 4096).await?)?;
    let path = result["path"].as_str().context("Missing skill directory")?;
    Ok(
        json!({"text":format!("Imported workflow {name}. Files are materialized at {path}. Resolve relative supporting-file paths from this directory. If present, ${{CLAUDE_SKILL_DIR}} also refers to this directory. Run scripts through the normal guest tools (for example python3 or bash), only as needed for the user's task. This load does not run them. Do not use original desktop source paths as VM paths.\nSource desktop link: {}. For source-workspace state use that verified desktop through normal local-access permissions; never substitute the VM or another desktop.\nCompatibility notes: {}\nInstructions:\n{}",package["origin"],warnings(&package),instructions),"path":path,"hash":package["hash"],"files":package["files"].as_object().unwrap().keys().collect::<Vec<_>>()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn taught_skill_survives_restart_and_loads_without_a_vm() {
        use std::sync::Arc;
        let root = std::env::temp_dir().join(format!("kindred-taught-{}", crate::db::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("kindred.db").to_string_lossy().into_owned();
        let mut app = crate::tests::app();
        Arc::get_mut(&mut app).unwrap().db = Db::open(&path).unwrap();
        let bot = crate::tests::bot(&app.db, "codex");
        let lesson = "When to use: Prepare the weekly report\n\nProcedure:\n1. Open Reports and choose the current week.\n\nVerify success: Check the date range and totals.";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server =
            tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
        let client = reqwest::Client::new();
        let input = json!({"name":"Weekly report","body":lesson,"description":"Prepare the current week's report"});
        assert_eq!(
            client
                .post(format!("{url}/api/skills"))
                .json(&input)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        let saved: Value = client
            .post(format!("{url}/api/skills"))
            .bearer_auth(&app.token)
            .json(&input)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let command = saved["command"].as_str().unwrap();
        let commands: Value = client
            .get(format!("{url}/api/commands"))
            .bearer_auth(&app.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            commands
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["name"] == command && c["skill_name"] == "Weekly report")
        );
        // Saving the lesson must never start a bot task.
        assert!(app.db.runs(None).unwrap().is_empty());
        server.abort();
        let _ = server.await;
        drop(app);
        let mut app = crate::tests::app();
        Arc::get_mut(&mut app).unwrap().db = Db::open(&path).unwrap();
        let id = app.db.queue(&bot.id, &format!("/{command}"), 0).unwrap();
        let run = app.db.run(&id).unwrap();
        assert!(run.prompt.contains(lesson));
        let listed = crate::runtime::call_tool(&app, &bot, &run, "skills_list", json!({}))
            .await
            .unwrap();
        assert!(listed["text"].as_str().unwrap().contains("Weekly report"));
        let loaded = crate::runtime::call_tool(
            &app,
            &bot,
            &run,
            "skill_load",
            json!({"name":"Weekly report"}),
        )
        .await
        .unwrap();
        assert_ne!(loaded["failed"], true);
        assert!(loaded["text"].as_str().unwrap().contains(lesson));
        assert_eq!(loaded["files"], json!([]));
        assert!(loaded["path"].is_null());
        assert!(app.db.run_approvals(&id).unwrap().is_empty());
        // A queued lesson remains stable even if its library entry changes.
        app.db
            .save_skill(&json!({"name":"Weekly report","body":"Changed instructions"}))
            .unwrap();
        assert_eq!(
            materialize(&app, &run, "Weekly report").await.unwrap(),
            loaded
        );
        app.db.delete_skill("Weekly report").unwrap();
        assert_eq!(
            materialize(&app, &run, "Weekly report").await.unwrap(),
            loaded
        );
        let next_id = app.db.queue(&bot.id, "Use the report lesson", 0).unwrap();
        assert!(
            materialize(&app, &app.db.run(&next_id).unwrap(), "Weekly report")
                .await
                .is_err()
        );
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn ordinary_skill_load_uses_current_instructions_or_pinned_named_inputs() {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "codex");
        app.db.save_skill(&json!({"name":"Report","command":"report","body":"Read {{project}} and verify its totals.","parameters":[{"name":"project"}]})).unwrap();
        let plain = app.db.queue(&bot.id, "Use the report workflow", 0).unwrap();
        let read = materialize(&app, &app.db.run(&plain).unwrap(), "Report")
            .await
            .unwrap();
        assert!(read["text"].as_str().unwrap().contains("Read {{project}}"));
        assert!(
            read["text"]
                .as_str()
                .unwrap()
                .contains("\"name\":\"project\"")
        );
        let invoked = app
            .db
            .queue(&bot.id, "/report 'Project Juniper'", 0)
            .unwrap();
        let loaded = materialize(&app, &app.db.run(&invoked).unwrap(), "Report")
            .await
            .unwrap();
        assert!(
            loaded["text"]
                .as_str()
                .unwrap()
                .contains("\"project\":\"Project Juniper\"")
        );
        assert_eq!(app.db.runs(None).unwrap().len(), 2);
    }

    fn package(extra: &str) -> Value {
        json!({"entry":"SKILL.md","name":"vulntracker","source":"fixture/.claude/skills/vulntracker/SKILL.md","files":{"SKILL.md":STANDARD.encode(format!("---\nname: vulntracker\ndescription: >-\n  Generate a tracker\n  from a Nessus export\nargument-hint: '[export] [client]'\n{extra}---\nUse $ARGUMENTS with $0 and $ARGUMENTS[1]. Read references/format.md and run python3 ${{CLAUDE_SKILL_DIR}}/scripts/tracker.py.\n")),"scripts/tracker.py":STANDARD.encode(b"print('fixture tracker')\n"),"references/format.md":STANDARD.encode(b"Preserve host and severity."),"templates/tracker.xlsx":STANDARD.encode([0,255,1,2])}})
    }
    fn import(db: &Db, package: Value) -> Value {
        let preview = request(db, &json!({"action":"preview","package":package})).unwrap();
        request(
            db,
            &json!({"action":"import","package":package,"expected_hash":preview["import"]["hash"]}),
        )
        .unwrap()
    }
    #[test]
    fn imports_preserve_files_review_hash_and_conflicts() {
        let db = Db::open(":memory:").unwrap();
        let p = package("allowed-tools: Read, Bash\n");
        let preview = request(&db, &json!({"action":"preview","package":p})).unwrap();
        assert!(db.skills().unwrap().is_empty());
        assert_eq!(
            preview["description"],
            "Generate a tracker from a Nessus export"
        );
        assert_eq!(preview["files"].as_array().unwrap().len(), 4);
        assert_eq!(preview["import"]["warnings"].as_array().unwrap().len(), 1);
        assert!(
            request(
                &db,
                &json!({"action":"import","package":p,"expected_hash":"old"})
            )
            .is_err()
        );
        let result = import(&db, p.clone());
        assert_eq!(result["status"], "imported");
        assert_eq!(import(&db, p.clone())["status"], "unchanged");
        let saved = load(&db.0.lock().unwrap(), "vulntracker").unwrap();
        assert_eq!(saved["files"], p["files"]);
        let mut changed = p.clone();
        changed["files"]["references/format.md"] = json!(STANDARD.encode(b"New format"));
        let review = request(&db, &json!({"action":"preview","package":changed})).unwrap();
        assert!(request(&db,&json!({"action":"import","package":changed,"expected_hash":review["import"]["hash"]})).is_err());
        assert_eq!(request(&db,&json!({"action":"import","package":changed,"expected_hash":review["import"]["hash"],"replace_hash":result["import"]["hash"]})).unwrap()["status"],"imported");
        assert!(request(&db,&json!({"action":"import","package":p,"expected_hash":result["import"]["hash"],"name":"another"})).is_err());
        assert!(Db::open(":memory:").unwrap().skills().unwrap().is_empty());
    }
    #[test]
    fn package_validation_rejects_unsafe_or_incomplete_inputs() {
        let db = Db::open(":memory:").unwrap();
        for path in [
            "../secret",
            "scripts/../../secret",
            "/absolute",
            "C:/secret",
            "a\\secret",
            ".credentials.json",
            "auth.json",
            "id_rsa",
            "secret.pem",
        ] {
            let mut p = package("");
            p["files"][path] = json!(STANDARD.encode("fixture"));
            assert!(
                request(&db, &json!({"action":"preview","package":p})).is_err(),
                "{path}"
            );
        }
        for text in [
            "---\nname: [invalid\n---\nbody",
            "---\nno end",
            "---\nname: okay\n---\n",
            "\0",
        ] {
            let mut p = package("");
            p["files"]["SKILL.md"] = json!(STANDARD.encode(text));
            assert!(request(&db, &json!({"action":"preview","package":p})).is_err());
        }
        let mut p = package("");
        p["files"].as_object_mut().unwrap().remove("SKILL.md");
        assert!(request(&db, &json!({"action":"preview","package":p})).is_err());
        assert!(db.skills().unwrap().is_empty());
    }
    #[test]
    fn command_receipt_pins_assets_and_badges_after_edit_or_delete() {
        let db = Db::open(":memory:").unwrap();
        let bot = crate::tests::bot(&db, "codex");
        let chat = format!("dm-{}", bot.id);
        db.save_chat(&crate::chats::Chat {
            bot_only: false,
            description: String::new(),
            id: chat.clone(),
            name: bot.name.clone(),
            members: vec![bot.id.clone()],
            archived: false,
            pinned: false,
            last_message: None,
        })
        .unwrap();
        import(&db, package("disable-model-invocation: true\n"));
        assert!(model_catalog(&db).unwrap().is_empty());
        db.queue(&bot.id, "ordinary task", 0).unwrap();
        let ordinary = db.claim().unwrap().unwrap();
        assert!(run_package(&db, &ordinary, "vulntracker").is_err());
        db.finish(&ordinary.id, "completed", "done", "").unwrap();
        let literal = "/vulntracker \"scan export.nessus\" Client's report";
        let ids = db.chat_send(&chat, literal, &[]).unwrap();
        let run = db.run(&ids[0]).unwrap();
        let original = run_package(&db, &run, "vulntracker").unwrap();
        assert!(run.prompt.contains("skill_load"));
        assert!(run.prompt.contains("scan export.nessus"));
        let messages = db.chat_messages(&chat).unwrap();
        let m = messages.iter().find(|m| m["text"] == literal).unwrap();
        assert_eq!(m["command"]["command"], "vulntracker");
        assert_eq!(m["command"]["hash"], original["hash"]);
        db.save_skill(&json!({"name":"vulntracker","body":"Edited instructions"}))
            .unwrap();
        let edited = load(&db.0.lock().unwrap(), "vulntracker").unwrap();
        assert_ne!(edited["hash"], original["hash"]);
        assert_eq!(run_package(&db, &run, "vulntracker").unwrap(), original);
        db.delete_skill("vulntracker").unwrap();
        assert_eq!(run_package(&db, &run, "vulntracker").unwrap(), original);
        assert_eq!(
            db.chat_messages(&chat)
                .unwrap()
                .iter()
                .find(|m| m["text"] == literal)
                .unwrap()["command"]["command"],
            "vulntracker"
        );
    }
    #[test]
    fn hidden_skills_are_available_to_bots_without_a_composer_alias() {
        let db = Db::open(":memory:").unwrap();
        import(&db, package("user-invocable: false\n"));
        assert!(
            !db.commands()
                .unwrap()
                .iter()
                .any(|c| c.name == "vulntracker")
        );
        assert_eq!(model_catalog(&db).unwrap().len(), 1);
    }
    #[cfg(unix)]
    #[test]
    fn native_discovery_and_guest_materialization_use_real_files_without_execution() {
        use std::io::Write;
        let temp = std::env::temp_dir().join(crate::db::id());
        let claude = temp.join(".claude");
        let root = claude.join("skills/vulntracker");
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::create_dir_all(claude.join("commands")).unwrap();
        std::fs::write(
            root.join("SKILL.md"),
            "# Review\nRun scripts/never.py when asked.",
        )
        .unwrap();
        std::fs::write(
            root.join("scripts/never.py"),
            "raise Exception('must not execute')",
        )
        .unwrap();
        std::fs::write(claude.join("commands/check.md"), "Review $ARGUMENTS").unwrap();
        assert_eq!(
            crate::skill_files::scan(&claude).unwrap()["candidates"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let p = crate::skill_files::bundle(&root.join("SKILL.md")).unwrap();
        assert_eq!(p["files"].as_object().unwrap().len(), 2);
        let package = parse(&p).unwrap();
        let workspace = temp.join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let run = crate::db::id();
        let script = WRITE.replace("'/workspace'", &format!("'{}'", workspace.display()));
        let invoke = || {
            let mut child = std::process::Command::new("python3")
                .args(["-c", &script])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(
                    serde_json::to_string(
                        &json!({"run":run,"hash":package["hash"],"files":package["files"]}),
                    )
                    .unwrap()
                    .as_bytes(),
                )
                .unwrap();
            child.wait_with_output().unwrap()
        };
        let output = invoke();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(invoke().status.success());
        let at = workspace
            .join(".kindred-skills")
            .join(&run)
            .join(package["hash"].as_str().unwrap());
        assert_eq!(
            std::fs::read(at.join("scripts/never.py")).unwrap(),
            b"raise Exception('must not execute')"
        );
        std::fs::write(at.join("scripts/never.py"), "changed").unwrap();
        assert!(!invoke().status.success());
        std::os::unix::fs::symlink(temp.join("outside"), root.join("scripts/link.py")).unwrap();
        assert!(crate::skill_files::bundle(&root.join("SKILL.md")).is_err());
        std::fs::remove_dir_all(&temp).unwrap();
    }
    #[test]
    fn invocation_conventions_follow_the_source_harness() {
        assert!(
            argument_conventions(&json!({"source":"/work/.pi/prompts/review.md"}))
                .contains("one-based $1")
        );
        assert!(
            argument_conventions(&json!({"source":"/work/.codex/prompts/review.md"}))
                .contains("one-based $1")
        );
        assert!(
            argument_conventions(&json!({"source":"/work/.claude/commands/review.md"}))
                .contains("zero-based $0")
        );
        assert!(
            argument_conventions(&json!({"source":"/work/skills/review/SKILL.md"}))
                .contains("instead of assuming")
        );
    }

    #[test]
    fn shell_warnings_recognize_opening_syntax_and_repair_existing_imports() {
        for body in [
            "- Examples: `Summer2026!`, `Fall2026!`, `Winter2026!`, `Spring2026!`.",
            "A literal !`unclosed",
            "word!`not dynamic`",
            "`!` is punctuation",
        ] {
            assert!(!arguments::dynamic_shell(body), "{body}");
            let mut p = package("");
            p["files"]["SKILL.md"] = json!(STANDARD.encode(body));
            let parsed = parse(&p).unwrap();
            assert!(!summary(&parsed)["warnings"].to_string().contains("Dynamic"));
        }
        // Current CLI permissions may come from outside the file's frontmatter.
        for body in [
            "!`git status`",
            "Current status: !`git status`",
            "```!\ngit status\n```",
        ] {
            assert!(arguments::dynamic_shell(body), "{body}");
        }
        let db = Db::open(":memory:").unwrap();
        let mut p = package("");
        p["files"]["SKILL.md"] = json!(STANDARD.encode("Example `Summer2026!`"));
        import(&db, p);
        let c = db.0.lock().unwrap();
        let mut saved = load(&c, "vulntracker").unwrap();
        saved["warnings"] = json!([SHELL_WARNING]);
        c.execute(
            "UPDATE skills SET import_data=? WHERE name='vulntracker'",
            [saved.to_string()],
        )
        .unwrap();
        let corrected = load(&c, "vulntracker").unwrap();
        assert_eq!(corrected["warnings"], json!([]));
        assert_eq!(corrected["hash"], saved["hash"]);
        assert_eq!(corrected["files"], saved["files"]);
    }

    #[test]
    fn imported_positions_are_named_expanded_once_and_frozen_for_skill_load() {
        let db = Db::open(":memory:").unwrap();
        let raw =
            "---\nargument-hint: '[label] [domain]'\n---\nFirst=$1; second=$2; all=$ARGUMENTS.";
        let p = json!({"name":"binding-demo","entry":"binding-demo.md","source":"/work/.claude/commands/binding-demo.md","files":{"binding-demo.md":STANDARD.encode(raw)}});
        import(&db, p.clone());
        let command = db
            .commands()
            .unwrap()
            .into_iter()
            .find(|c| c.name == "binding-demo")
            .unwrap();
        assert_eq!(command.parameters[0].name, "label");
        assert_eq!(command.parameters[1].name, "domain");
        assert!(command.parameters[0].required && command.parameters[1].required);
        assert!(
            db.save_skill(&json!({"name":"binding-demo","body":"Invalid edit","argument_index":2}))
                .is_err()
        );
        assert_eq!(
            db.skills().unwrap()[0]["body"],
            "First=$1; second=$2; all=$ARGUMENTS."
        );
        let bot = crate::tests::bot(&db, "codex");
        let chat = format!("dm-{}", bot.id);
        db.save_chat(&crate::chats::Chat {
            bot_only: false,
            description: String::new(),
            id: chat.clone(),
            name: bot.name.clone(),
            members: vec![bot.id.clone()],
            archived: false,
            pinned: false,
            last_message: None,
        })
        .unwrap();
        assert!(db.chat_send(&chat, "/binding-demo only-one", &[]).is_err());
        assert!(db.chat_messages(&chat).unwrap().is_empty());
        let input = "/binding-demo 'Example! $2 $(never-run)' example.invalid";
        let ids = db.chat_send(&chat, input, &[]).unwrap();
        let run = db.run(&ids[0]).unwrap();
        assert!(run.prompt.contains("First=Example! $2 $(never-run); second=example.invalid; all='Example! $2 $(never-run)' example.invalid."));
        let saved = run_package(&db, &run, "binding-demo").unwrap();
        assert_eq!(saved["files"], p["files"]);
        let loaded = invocation_instructions(&db, &run, "binding-demo", &saved).unwrap();
        assert!(loaded.contains("First=Example! $2 $(never-run); second=example.invalid"));
        db.save_skill(&json!({"name":"binding-demo","body":"Changed $1","argument_index":0}))
            .unwrap();
        assert_eq!(
            invocation_instructions(&db, &run, "binding-demo", &saved).unwrap(),
            loaded
        );
        let c = db.0.lock().unwrap();
        let receipt: String = c
            .query_row(
                "SELECT receipt FROM run_commands WHERE run_id=?",
                [&run.id],
                |r| r.get(0),
            )
            .unwrap();
        let receipt: Value = serde_json::from_str(&receipt).unwrap();
        assert_eq!(
            receipt["placeholder_values"]["$1"],
            "Example! $2 $(never-run)"
        );
        assert_eq!(receipt["placeholder_values"]["$2"], "example.invalid");
        assert_eq!(receipt["arguments"]["label"], "Example! $2 $(never-run)");
    }

    #[test]
    fn modern_and_legacy_positions_remain_distinct_and_overrides_are_explicit() {
        for (source, body, wanted) in [
            (
                "/w/.claude/skills/demo/SKILL.md",
                "$0|$1|$ARGUMENTS[1]",
                "first|two words|two words",
            ),
            ("/w/.claude/commands/demo.md", "$1|$2", "first|two words"),
            ("/w/.claude/commands/demo.md", "$0|$1", "first|two words"),
            ("/w/.codex/prompts/demo.md", "$1|$2", "first|two words"),
            (
                "/w/.pi/prompts/demo.md",
                "$1|$2|$@",
                "first|two words|first 'two words'",
            ),
        ] {
            let p = json!({"source":source,"body":body});
            let (expanded, _) = arguments::expand(
                &p,
                "first 'two words'",
                &["first".into(), "two words".into()],
            )
            .unwrap();
            assert_eq!(expanded, wanted, "{source}");
        }
        let mut p = json!({"source":"/w/.claude/commands/demo.md","body":"$1"});
        assert!(arguments::inferred_legacy(&p));
        p["argument_index"] = json!(0);
        assert!(!arguments::inferred_legacy(&p));
        assert!(arguments::expand(&p, "first", &["first".into()]).is_err());
        assert_eq!(
            arguments::expand(&p, "first second", &["first".into(), "second".into()])
                .unwrap()
                .0,
            "second"
        );
        let escaped = json!({"body":r"Literal \$1 and $1name; value=$1"});
        assert_eq!(
            arguments::expand(&escaped, "hello", &["hello".into()])
                .unwrap()
                .0,
            r"Literal \$1 and $1name; value=hello"
        );
        let large = json!({"body":"$1 $1"});
        assert!(arguments::expand(&large, "", &["x".repeat(40000)]).is_err());
        let p = json!({"body":"$1 $2","argument_hint":"[label] [description...]"});
        let parameters = arguments::parameters(
            &p,
            vec![crate::commands::Parameter {
                name: "arguments".into(),
                description: String::new(),
                required: false,
                rest: true,
            }],
        );
        assert!(
            !parameters[1].rest,
            "A positional placeholder receives one quoted input, even if its hint suggests free text"
        );
        assert!(parameters[2].rest);
    }
}
