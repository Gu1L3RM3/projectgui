//! Independent OS processes hammering one database file.
mod common;

use std::process::Stdio;

use common::{cmd, stdout};

const WORKERS: usize = 8;
const TASKS_EACH: usize = 10;

#[test]
fn concurrent_processes_lose_nothing_and_keep_the_database_consistent() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    stdout(cmd(h).args(["add", "Shared"]));

    // Each worker is a separate process: adds tasks to one project and creates same-titled projects.
    let bin = assert_cmd::cargo::cargo_bin("projectgui");
    let children: Vec<_> = (0..WORKERS)
        .map(|w| {
            let script = (0..TASKS_EACH)
                .map(|i| format!("\"{bin}\" task add shared w{w}-t{i} >/dev/null || exit 1; \"{bin}\" add Same >/dev/null || exit 1;", bin = bin.display()))
                .collect::<String>();
            std::process::Command::new("sh")
                .arg("-c")
                .arg(script)
                .env("PROJECTGUI_DB", h.join("test.db"))
                .env("PROJECTGUI_CONFIG", h.join("config.toml"))
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for c in children {
        let out = c.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "worker failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let projects: serde_json::Value =
        serde_json::from_str(&stdout(cmd(h).args(["--json", "list"]))).unwrap();
    let projects = projects.as_array().unwrap();
    let shared = projects.iter().find(|p| p["slug"] == "shared").unwrap();
    assert_eq!(
        shared["tasks"].as_array().unwrap().len(),
        WORKERS * TASKS_EACH,
        "lost task writes"
    );
    let mut positions: Vec<_> = shared["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["position"].as_i64().unwrap())
        .collect();
    positions.sort();
    positions.dedup();
    assert_eq!(
        positions.len(),
        WORKERS * TASKS_EACH,
        "duplicate task positions"
    );

    let mut slugs: Vec<_> = projects
        .iter()
        .map(|p| p["slug"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(slugs.len(), 1 + WORKERS * TASKS_EACH);
    slugs.sort();
    slugs.dedup();
    assert_eq!(
        slugs.len(),
        1 + WORKERS * TASKS_EACH,
        "slugs must stay unique"
    );

    let conn = rusqlite::Connection::open(h.join("test.db")).unwrap();
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let fk_violations = conn
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query_map([], |_| Ok(()))
        .unwrap()
        .count();
    assert_eq!(fk_violations, 0);
}
