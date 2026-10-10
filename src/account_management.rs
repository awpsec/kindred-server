//! Immutable account identity, fixed Owner authority, and trusted-host recovery.
use super::*;
pub(super) const OWNER: &str = "owner_account_id";

pub(super) fn owner_id(c: &Connection) -> Result<Option<String>> {
    Ok(
        c.query_row("SELECT value FROM controls WHERE key=?", [OWNER], |r| {
            r.get(0)
        })
        .optional()?,
    )
}
pub(super) fn owner_resolved(c: &Connection) -> Result<bool> {
    Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM controls o JOIN accounts a ON a.id=o.value WHERE o.key=? AND a.admin=1 AND a.disabled=0 AND NOT EXISTS(SELECT 1 FROM controls r WHERE r.key='removing:'||a.id))",[OWNER],|r|r.get(0))?)
}
pub(super) fn role(c: &Connection, account: &str, admin: bool) -> Result<&'static str> {
    Ok(
        if admin && owner_resolved(c)? && owner_id(c)?.as_deref() == Some(account) {
            "owner"
        } else if admin {
            "admin"
        } else {
            "user"
        },
    )
}
pub(super) fn protect_owner(c: &Connection, target: &str) -> Result<()> {
    ensure!(
        owner_id(c)?.as_deref() != Some(target),
        "The Owner account cannot be disabled or removed"
    );
    if !owner_resolved(c)? {
        let admin: bool = c
            .query_row("SELECT admin FROM accounts WHERE id=?", [target], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(false);
        ensure!(
            !admin,
            "Confirm this server's Owner on the host before disabling or removing an administrator"
        );
    }
    Ok(())
}
pub(super) fn identity_in(c: &Connection, token: &str) -> Result<Identity> {
    ensure!(
        (32..=256).contains(&token.len()),
        "Sign in to your Kindred account"
    );
    c.query_row("SELECT s.account_id,s.profile_id,a.admin FROM sessions s JOIN accounts a ON a.id=s.account_id JOIN profiles p ON p.id=s.profile_id AND p.account_id=a.id WHERE s.digest=? AND s.expires>? AND a.disabled=0",params![hash(token),db::now()],|r|Ok(Identity{account:r.get(0)?,profile:r.get(1)?,admin:r.get(2)?,legacy:false})).optional()?.ok_or_else(||anyhow::anyhow!("Your session expired. Sign in again."))
}
fn login(v: &Value, key: &str) -> Result<String> {
    let name = field(v, key, 80)?.to_lowercase();
    ensure!(
        name.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b)),
        "Use letters, numbers, dots, hyphens or an email address for your username"
    );
    Ok(name)
}
pub(super) async fn rename(
    State(p): State<Portal>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> ApiResult {
    p.origin(&headers)?;
    let expected = login(&v, "expected_username")?;
    let name = login(&v, "username")?;
    let account = field(&v, "account_id", 64)?;
    let mut c = p.registry.lock().unwrap();
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let actor = identity_in(&tx, bearer(&headers))?;
    if actor.account != account {
        return Err(web::Error::with_status(
            StatusCode::FORBIDDEN,
            "Change only your own username.",
        ));
    }
    let current: String =
        tx.query_row("SELECT login FROM accounts WHERE id=?", [account], |r| {
            r.get(0)
        })?;
    if current != expected {
        return Err(web::Error::with_status(
            StatusCode::CONFLICT,
            "Your account changed elsewhere. Reload and try again.",
        ));
    }
    let taken: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM accounts WHERE login=? AND id!=?)",
        params![name, account],
        |r| r.get(0),
    )?;
    if taken {
        return Err(web::Error::with_status(
            StatusCode::CONFLICT,
            "That username is already in use.",
        ));
    }
    tx.execute(
        "UPDATE accounts SET login=? WHERE id=?",
        params![name, account],
    )?;
    let result = json!({"account_id":account,"username":name,"admin":actor.admin,"role":role(&tx,account,actor.admin)?,"owner_resolved":owner_resolved(&tx)?});
    tx.commit()?;
    Ok(Json(result))
}
pub(super) fn change_role(
    tx: &Connection,
    actor: &Identity,
    v: &Value,
) -> std::result::Result<(), web::Error> {
    if !owner_resolved(tx)? || owner_id(tx)?.as_deref() != Some(&actor.account) || !actor.admin {
        return Err(web::Error::with_status(
            StatusCode::FORBIDDEN,
            "Only the Owner can change roles.",
        ));
    }
    let target = field(v, "user_id", 64)?;
    ensure!(
        owner_id(tx)?.as_deref() != Some(target),
        "The Owner's role cannot be changed."
    );
    let removing: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM controls WHERE key=?)",
        [format!("removing:{target}")],
        |r| r.get(0),
    )?;
    ensure!(
        !removing,
        "Account removal is in progress. Retry removal to finish."
    );
    let admin = match v["role"].as_str() {
        Some("admin") => true,
        Some("user") => false,
        _ => return Err(anyhow::anyhow!("Choose User or Admin.").into()),
    };
    let current: bool = tx
        .query_row("SELECT admin FROM accounts WHERE id=?", [target], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("User unavailable"))?;
    let expected = if current { "admin" } else { "user" };
    if v["expected_role"].as_str() != Some(expected) {
        return Err(web::Error::with_status(
            StatusCode::CONFLICT,
            "This user's role changed elsewhere. Reload and try again.",
        ));
    }
    tx.execute(
        "UPDATE accounts SET admin=? WHERE id=?",
        params![admin, target],
    )?;
    Ok(())
}

/// Uses only an existing config-selected registry. Does not open Apps, provision
/// profiles, inspect provider credentials, or use a browser/legacy token.
pub fn host_owner(config: &Config, confirm: Option<(&str, &str)>) -> Result<Value> {
    ensure!(
        config.profiles.enabled,
        "Account profiles are not enabled in the selected configuration"
    );
    let path = Path::new(&config.profiles.directory).join("accounts.db");
    let mut c = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|_| {
            anyhow::anyhow!(
                "Could not open the existing account registry selected by this configuration"
            )
        })?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    // Old supported registries already contain controls; never infer an Owner
    // from ordering, names, admin count, or a reconstructed import.
    if let Some((account, username)) = confirm {
        ensure!(uuid::Uuid::parse_str(account).is_ok(), "Invalid account ID");
        let row: Option<(String, bool, bool)> = tx
            .query_row(
                "SELECT login,admin,disabled FROM accounts WHERE id=?",
                [account],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((login, admin, disabled)) = row else {
            anyhow::bail!("Account unavailable");
        };
        ensure!(login == username, "Confirm the exact current username");
        ensure!(
            admin && !disabled,
            "Confirm an enabled administrator account"
        );
        ensure!(
            !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM controls WHERE key=?)",
                [format!("removing:{account}")],
                |r| r.get::<_, bool>(0)
            )?,
            "Account removal is in progress"
        );
        if let Some(existing) = owner_id(&tx)? {
            ensure!(
                existing == account,
                "This server's Owner is already fixed; Owner transfer is not supported"
            );
        } else {
            tx.execute("INSERT INTO controls VALUES(?,?)", params![OWNER, account])?;
        }
    }
    let owner = owner_id(&tx)?;
    let resolved = owner_resolved(&tx)?;
    let accounts:Vec<Value>=tx.prepare("SELECT id,login,admin,disabled FROM accounts ORDER BY login,id")?.query_map([],|r|Ok(json!({"account_id":r.get::<_,String>(0)?,"username":r.get::<_,String>(1)?,"admin":r.get::<_,bool>(2)?,"disabled":r.get::<_,bool>(3)?})))?.collect::<rusqlite::Result<_>>()?;
    let result = json!({"owner_account_id":owner,"owner_resolved":resolved,"accounts":accounts});
    tx.commit()?;
    Ok(result)
}
