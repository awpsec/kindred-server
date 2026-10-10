use rusqlite::{Connection, params};
use serde_json::Value;
use std::{
    path::PathBuf,
    process::{Command, Output},
};
struct Fixture {
    root: PathBuf,
    config: PathBuf,
    owner: String,
    other: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kindred-owner-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("selected")).unwrap();
        let owner = uuid::Uuid::new_v4().to_string();
        let other = uuid::Uuid::new_v4().to_string();
        let config = root.join("server.toml");
        std::fs::write(&config,format!("[profiles]\nenabled=true\ndirectory={:?}\nvm_manager=\"/must-not-run-provider-or-vm\"\n",root.join("selected").to_str().unwrap())).unwrap();
        let c = Connection::open(root.join("selected/accounts.db")).unwrap();
        c.execute_batch("CREATE TABLE accounts(id TEXT PRIMARY KEY,login TEXT UNIQUE,salt TEXT,password BLOB,admin INTEGER,disabled INTEGER,created INTEGER);CREATE TABLE controls(key TEXT PRIMARY KEY,value TEXT);CREATE TABLE profiles(id TEXT,account_id TEXT,name TEXT);CREATE TABLE sessions(digest BLOB,account_id TEXT);CREATE TABLE device_links(digest BLOB,account_id TEXT);").unwrap();
        for (id, login) in [
            (&owner, "confirmed-owner"),
            (&other, "other-admin\n\u{1b}[31m"),
        ] {
            c.execute(
                "INSERT INTO accounts VALUES(?,?,'SECRET-SALT',X'010203',1,0,1)",
                params![id, login],
            )
            .unwrap();
        }
        c.execute(
            "INSERT INTO profiles VALUES('workspace',?,'Keep profile')",
            [&owner],
        )
        .unwrap();
        c.execute("INSERT INTO sessions VALUES(X'0001',?)", [&owner])
            .unwrap();
        c.execute("INSERT INTO device_links VALUES(X'0002',?)", [&owner])
            .unwrap();
        std::fs::write(root.join("vm.disk"), b"keep exact disk").unwrap();
        Self {
            root,
            config,
            owner,
            other,
        }
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_kindred"))
            .current_dir(&self.root)
            .env_remove("KINDRED_TOKEN")
            .args(["--config", self.config.to_str().unwrap(), "owner"])
            .args(args)
            .output()
            .unwrap()
    }
    fn db(&self) -> Connection {
        Connection::open(self.root.join("selected/accounts.db")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
#[test]
fn account_owner_cli_explicit_selection_immutable_and_secret_free() {
    let f = Fixture::new();
    let before = f.run(&["status"]);
    assert!(before.status.success());
    let value: Value = serde_json::from_slice(&before.stdout).unwrap();
    assert_eq!(value["owner_resolved"], false);
    assert!(value["owner_account_id"].is_null());
    let output = String::from_utf8(before.stdout).unwrap();
    assert!(!output.contains("SECRET-SALT"));
    assert!(!output.contains('\u{1b}'));
    assert!(!output.contains("digest"));
    assert!(
        !f.run(&[
            "confirm",
            "--account-id",
            &f.owner,
            "--confirm-username",
            "wrong"
        ])
        .status
        .success()
    );
    assert!(
        f.run(&[
            "confirm",
            "--account-id",
            &f.owner,
            "--confirm-username",
            "confirmed-owner"
        ])
        .status
        .success()
    );
    assert!(
        f.run(&[
            "confirm",
            "--account-id",
            &f.owner,
            "--confirm-username",
            "confirmed-owner"
        ])
        .status
        .success()
    );
    assert!(
        !f.run(&[
            "confirm",
            "--account-id",
            &f.other,
            "--confirm-username",
            "other-admin\n\u{1b}[31m"
        ])
        .status
        .success()
    );
    let c = f.db();
    let fixed: String = c
        .query_row(
            "SELECT value FROM controls WHERE key='owner_account_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fixed, f.owner);
    for table in ["profiles", "sessions", "device_links"] {
        assert_eq!(
            c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    assert_eq!(
        std::fs::read(f.root.join("vm.disk")).unwrap(),
        b"keep exact disk"
    );
    assert!(!f.root.join("data").exists());
}
#[test]
fn account_owner_cli_no_registry_creation_or_disabled_selection() {
    let f = Fixture::new();
    f.db()
        .execute("UPDATE accounts SET disabled=1 WHERE id=?", [&f.owner])
        .unwrap();
    assert!(
        !f.run(&[
            "confirm",
            "--account-id",
            &f.owner,
            "--confirm-username",
            "confirmed-owner"
        ])
        .status
        .success()
    );
    std::fs::write(
        &f.config,
        format!(
            "[profiles]\nenabled=true\ndirectory={:?}\n",
            f.root.join("does-not-exist").to_str().unwrap()
        ),
    )
    .unwrap();
    assert!(!f.run(&["status"]).status.success());
    assert!(!f.root.join("does-not-exist").exists());
}
