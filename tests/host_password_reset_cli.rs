use rusqlite::{Connection, params};
use serde_json::Value;
use std::{
    path::PathBuf,
    process::{Command, Output},
};
struct Fixture {
    root: PathBuf,
    config: PathBuf,
    id: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kindred-reset-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("live")).unwrap();
        let config = root.join("selected.toml");
        std::fs::write(&config,format!("[profiles]\nenabled=true\nimport_legacy=true\ndirectory={:?}\nvm_manager=\"/must-not-run-vm-manager\"\n",root.join("live").to_str().unwrap())).unwrap();
        let c = Connection::open(root.join("live/accounts.db")).unwrap();
        c.execute_batch("CREATE TABLE accounts(id TEXT PRIMARY KEY,login TEXT,salt TEXT,password BLOB,admin INTEGER,disabled INTEGER,created INTEGER);CREATE TABLE password_resets(id TEXT PRIMARY KEY,digest BLOB,account TEXT,created INTEGER,expires INTEGER,ip TEXT,state TEXT,decided INTEGER,decided_by TEXT,location TEXT);CREATE TABLE profiles(id TEXT PRIMARY KEY,account_id TEXT,name TEXT);CREATE TABLE sessions(digest BLOB,account_id TEXT);CREATE TABLE device_links(digest BLOB,account_id TEXT);INSERT INTO accounts VALUES('owner','sole-admin','salt',X'01',1,0,1);INSERT INTO profiles VALUES('workspace','owner','Keep');INSERT INTO sessions VALUES(X'01','owner');INSERT INTO device_links VALUES(X'02','owner');").unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        c.execute("INSERT INTO password_resets VALUES(?,X'0123456789','owner',?,?,'127.0.0.1','pending',NULL,NULL,?)",params![id,now,now+86400,"City\n\u{1b}[31mFake audit\u{202e}"]).unwrap();
        std::fs::write(root.join("vm.disk"), b"preserved vm data").unwrap();
        Self { root, config, id }
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_kindred"))
            .current_dir(&self.root)
            .env_remove("KINDRED_TOKEN")
            .args(["--config", self.config.to_str().unwrap(), "password-reset"])
            .args(args)
            .output()
            .unwrap()
    }
    fn db(&self) -> Connection {
        Connection::open(self.root.join("live/accounts.db")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
#[test]
fn host_reset_cli_selected_registry_and_sole_admin() {
    let f = Fixture::new();
    let o = f.run(&["list"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["requests"][0]["id"], f.id);
    assert_eq!(v["requests"][0]["username"], "sole-admin");
    assert_eq!(v["requests"][0]["state"], "pending");
    assert!(!v.to_string().contains("digest"));
    assert!(
        !v["requests"][0]["location"]
            .as_str()
            .unwrap()
            .chars()
            .any(char::is_control)
    );
    let o = f.run(&["approve", &f.id]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let c = f.db();
    let row: (String, String, i64) = c
        .query_row(
            "SELECT state,decided_by,expires-decided FROM password_resets",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, ("approved".into(), "local-host".into(), 900));
    assert!(!f.run(&["approve", &f.id]).status.success());
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM profiles", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        std::fs::read(f.root.join("vm.disk")).unwrap(),
        b"preserved vm data"
    );
    assert!(!f.root.join("data").exists());
}
#[test]
fn host_reset_cli_rejects_expired_disabled_unknown_and_concurrent_repeat() {
    for mode in ["expired", "disabled", "unknown"] {
        let f = Fixture::new();
        let c = f.db();
        match mode {
            "expired" => {
                c.execute("UPDATE password_resets SET expires=0", [])
                    .unwrap();
            }
            "disabled" => {
                c.execute("UPDATE accounts SET disabled=1", []).unwrap();
            }
            _ => {
                c.execute("UPDATE password_resets SET account=NULL", [])
                    .unwrap();
            }
        }
        assert!(!f.run(&["approve", &f.id]).status.success(), "{mode}");
        assert_eq!(
            c.query_row("SELECT state FROM password_resets", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "pending"
        );
    }
    let f = Fixture::new();
    let launch = |action| {
        Command::new(env!("CARGO_BIN_EXE_kindred"))
            .args([
                "--config",
                f.config.to_str().unwrap(),
                "password-reset",
                action,
                &f.id,
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let a = launch("approve");
    let b = launch("deny");
    let a = a.wait_with_output().unwrap();
    let b = b.wait_with_output().unwrap();
    assert_ne!(a.status.success(), b.status.success());
    assert!(!f.run(&["deny", &f.id]).status.success());
    let n:i64=f.db().query_row("SELECT COUNT(*) FROM password_reset_audit WHERE outcome='approved' OR outcome='denied'",[],|r|r.get(0)).unwrap();
    assert_eq!(n, 1);
}
#[test]
fn host_reset_cli_missing_registry_does_not_create_or_leak_input() {
    let f = Fixture::new();
    std::fs::write(
        &f.config,
        "[profiles]\nenabled=true\ndirectory=\"missing-registry\"\n",
    )
    .unwrap();
    let o = f.run(&["list"]);
    assert!(!o.status.success());
    assert!(!f.root.join("missing-registry").exists());
    let o = f.run(&["approve", "secret-reset-token-DO-NOT-ECHO"]);
    assert!(!o.status.success());
    assert!(!String::from_utf8_lossy(&o.stderr).contains("secret-reset-token-DO-NOT-ECHO"));
}

#[test]
fn host_reset_cli_wrong_config_registry_leaves_decoy_and_legacy_workspace_unchanged() {
    let f = Fixture::new();
    let decoy = f.root.join("accounts.db");
    std::fs::write(&decoy, b"legacy snapshot sentinel").unwrap();
    let c = f.db();
    c.execute(
        "UPDATE password_resets SET location=?",
        ["https://example.test/reset?token=DO_NOT_LEAK_LOCATION"],
    )
    .unwrap();
    let output = f.run(&["list"]);
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("DO_NOT_LEAK_LOCATION"));
    assert!(f.run(&["deny", &f.id]).status.success());
    assert_eq!(std::fs::read(decoy).unwrap(), b"legacy snapshot sentinel");
    std::fs::write(&f.config, "[profiles]\nenabled=false\n").unwrap();
    assert!(!f.run(&["list"]).status.success());
}
#[test]
fn host_reset_cli_audit_failure_rolls_back_the_decision() {
    let f = Fixture::new();
    assert!(f.run(&["list"]).status.success());
    let c = f.db();
    c.execute_batch("CREATE TRIGGER reject_audit BEFORE INSERT ON password_reset_audit BEGIN SELECT RAISE(FAIL,'audit unavailable'); END;").unwrap();
    let output = f.run(&["approve", &f.id]);
    assert!(!output.status.success());
    assert_eq!(
        c.query_row("SELECT state FROM password_resets", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "pending"
    );
    c.execute_batch("DROP TRIGGER reject_audit;").unwrap();
    assert!(f.run(&["approve", &f.id]).status.success());
}
