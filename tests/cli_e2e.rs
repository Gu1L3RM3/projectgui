mod common;

use common::{cmd, cmd_in, stdout};

fn setup() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    stdout(cmd(h).args([
        "add",
        "Website redesign",
        "-p",
        "high",
        "-s",
        "in-progress",
        "-t",
        "copy",
        "-t",
        "fonts",
        "-t",
        "launch",
        "-t",
        "qa",
    ]));
    stdout(cmd(h).args(["add", "API", "-p", "critical"]));
    stdout(cmd(h).args(["task", "add", "api", "auth", "docs"]));
    home
}

fn first_task_id(h: &std::path::Path, project: &str) -> String {
    let json = stdout(cmd(h).args(["--json", "task", "list", project]));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    v[0]["id"].as_str().unwrap().to_string()
}

#[test]
fn status_shows_every_project_with_bars_sorted_by_priority() {
    let home = setup();
    let id = first_task_id(home.path(), "website");
    stdout(cmd(home.path()).args(["task", "done", "website-redesign", &id]));
    let out = stdout(cmd(home.path()).arg("status"));
    assert!(!out.contains('\x1b'), "NO_COLOR must disable colors: {out}");
    let (api, web) = (
        out.find("API").unwrap(),
        out.find("Website redesign").unwrap(),
    );
    assert!(api < web, "critical first:\n{out}");
    assert!(out.contains(" 25%") && out.contains("1 of 4"), "{out}");
    assert!(out.contains("2 projects, 1 of 6 tasks done"), "{out}");
}

#[test]
fn status_by_name_and_here() {
    let home = setup();
    let h = home.path();
    let detail = stdout(cmd(h).args(["status", "website"]));
    assert!(
        detail.contains("Website redesign") && detail.contains("○ copy"),
        "{detail}"
    );

    let work = h.join("work/site");
    std::fs::create_dir_all(work.join("src")).unwrap();
    stdout(cmd_in(h, &work).args(["link", "website-redesign"]));
    assert!(
        stdout(cmd_in(h, &work.join("src")).args(["status", "--here"]))
            .contains("Website redesign")
    );
    cmd(h).args(["status", "--here"]).assert().code(2); // home itself is not linked
}

#[test]
fn ambiguous_and_missing_names_have_distinct_exit_codes() {
    let home = setup();
    let h = home.path();
    stdout(cmd(h).args(["add", "Website blog"]));
    cmd(h).args(["status", "website"]).assert().code(3);
    cmd(h).args(["status", "nonexistent"]).assert().code(2);
    cmd(h).args(["add", "   "]).assert().code(4);
}

#[test]
fn list_filters_and_json() {
    let home = setup();
    let h = home.path();
    let id = first_task_id(h, "api");
    stdout(cmd(h).args(["task", "done", "api", &id]));

    let out = stdout(cmd(h).args([
        "list",
        "--priority",
        "high,critical",
        "--min-progress",
        "50",
    ]));
    assert!(
        out.contains("api") && !out.contains("website-redesign"),
        "{out}"
    );
    let out = stdout(cmd(h).args(["list", "--stage", "in-progress"]));
    assert!(
        out.contains("website-redesign") && !out.contains("\napi"),
        "{out}"
    );
    let out = stdout(cmd(h).args(["list", "--sort", "title", "--reverse"]));
    assert!(
        out.find("website-redesign").unwrap() < out.find("\napi").unwrap(),
        "{out}"
    );

    let v: serde_json::Value = serde_json::from_str(&stdout(cmd(h).args([
        "--json",
        "list",
        "--max-progress",
        "10",
    ])))
    .unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["slug"], "website-redesign");
    assert_eq!(v[0]["progress"]["percent"], 0);
    cmd(h)
        .args(["list", "--min-progress", "101"])
        .assert()
        .failure();
}

#[test]
fn delete_needs_confirmation_without_a_terminal() {
    let home = setup();
    let h = home.path();
    cmd(h).args(["rm", "api"]).assert().code(5);
    assert!(stdout(cmd(h).arg("list")).contains("api"));
    cmd(h).args(["rm", "web", "--yes"]).assert().code(2); // fuzzy names never reach a mutation
    assert!(stdout(cmd(h).arg("list")).contains("website-redesign"));
    stdout(cmd(h).args(["rm", "api", "--yes"]));
    assert!(!stdout(cmd(h).arg("list")).contains("\napi"));
}

#[test]
fn rename_keeps_old_slug_working() {
    let home = setup();
    let h = home.path();
    stdout(cmd(h).args(["edit", "api", "--slug", "backend", "--title", "Backend"]));
    assert!(stdout(cmd(h).args(["status", "backend"])).contains("Backend"));
    assert!(stdout(cmd(h).args(["edit", "api", "-s", "review"])).contains("Updated"));
}

#[test]
fn export_import_and_backup() {
    let home = setup();
    let h = home.path();
    let export = h.join("out.json");
    stdout(cmd(h).args(["export", "--out", export.to_str().unwrap()]));
    cmd(h)
        .args(["export", "--out", export.to_str().unwrap()])
        .assert()
        .code(6); // never overwrite

    let other = tempfile::tempdir().unwrap();
    let out = stdout(cmd(other.path()).args(["import", export.to_str().unwrap()]));
    assert!(out.contains("Imported 2"), "{out}");
    assert!(stdout(cmd(other.path()).arg("list")).contains("website-redesign"));

    let backup = h.join("b.db");
    stdout(cmd(h).args(["backup", "--out", backup.to_str().unwrap()]));
    assert!(backup.exists());
}

#[test]
fn config_round_trip_and_ascii() {
    let home = setup();
    let h = home.path();
    assert_eq!(
        stdout(cmd(h).args(["config", "get", "ascii"])).trim(),
        "false"
    );
    stdout(cmd(h).args(["config", "set", "ascii", "true"]));
    assert!(
        stdout(cmd(h).arg("status")).contains('-'),
        "ascii bars use '-'"
    );
    assert!(!stdout(cmd(h).arg("status")).contains('█'));
    cmd(h)
        .args(["config", "set", "color", "rainbow"])
        .assert()
        .code(4);
}

#[test]
fn help_and_version_are_complete() {
    let home = setup();
    let help = stdout(cmd(home.path()).arg("--help"));
    for word in [
        "status",
        "list",
        "task",
        "ref",
        "backup",
        "config",
        "EXIT CODES",
        "EXAMPLES",
        "PROJECTGUI_DB",
    ] {
        assert!(help.contains(word), "help is missing {word}");
    }
    assert!(stdout(cmd(home.path()).arg("--version")).starts_with("pgui "));
    let v: serde_json::Value =
        serde_json::from_str(&stdout(cmd(home.path()).args(["--json", "version"]))).unwrap();
    assert_eq!(v["name"], "pgui");
    assert!(stdout(cmd(home.path()).args(["completions", "bash"])).contains("pgui"));
}

#[test]
fn search_narrows_status_and_list_and_reports_no_match() {
    let home = setup();
    let h = home.path();
    let out = stdout(cmd(h).args(["status", "--search", "FONTS"]));
    assert!(
        out.contains("Website redesign") && !out.contains("API"),
        "{out}"
    );
    let list = stdout(cmd(h).args(["list", "-s", "website"]));
    assert!(list.contains("website") && !list.contains("api"), "{list}");
    let none = stdout(cmd(h).args(["status", "--search", "zzz-nothing"]));
    assert!(none.contains("No projects match"), "{none}");
    let json = stdout(cmd(h).args(["--json", "list", "--search", "zzz-nothing"]));
    assert_eq!(json.trim(), "[]");
}

const STATUS_FILE: &str = r##"
title = "Filed project"
priority = "high"
stage = "in-progress"

[[task]]
title = "one"
status = "done"

[[task]]
title = "two"

[[reference]]
kind = "pr"
value = "#7"
"##;

#[test]
fn init_file_then_sync_round_trips_and_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    let (h, d) = (home.path(), proj.path());

    // a fresh directory gets a commented template named after the directory
    let wrote = stdout(cmd_in(h, d).arg("init-file"));
    assert!(wrote.contains(".project-status"), "{wrote}");
    cmd_in(h, d).arg("init-file").assert().code(6); // already exists: conflict
    stdout(cmd_in(h, d).args(["init-file", "--force"]));

    std::fs::write(d.join(".project-status"), STATUS_FILE).unwrap();
    let dry = stdout(cmd_in(h, d).args(["sync", "--dry-run"]));
    assert!(
        dry.contains("Would create") && dry.contains("Filed project"),
        "{dry}"
    );
    assert!(
        !stdout(cmd(h).arg("list")).contains("filed-project"),
        "dry run writes nothing"
    );

    let first = stdout(cmd_in(h, d).arg("sync"));
    assert!(
        first.contains("Created") && first.contains("+ 2 tasks: one, two"),
        "{first}"
    );
    let again = stdout(cmd_in(h, d).arg("sync"));
    assert!(again.contains("In sync"), "{again}");

    // the directory is now linked, so --here finds the project, and init-file describes it
    let here = stdout(cmd_in(h, d).args(["status", "--here"]));
    assert!(
        here.contains("Filed project") && here.contains("50%"),
        "{here}"
    );
    stdout(cmd_in(h, d).args(["init-file", "--force"]));
    let rewritten = std::fs::read_to_string(d.join(".project-status")).unwrap();
    assert!(rewritten.contains("title = \"Filed project\"") && rewritten.contains("[[task]]"));
    assert!(stdout(cmd_in(h, d).arg("sync")).contains("In sync"));
}

#[test]
fn sync_reports_missing_and_invalid_files_without_writing() {
    let home = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    let (h, d) = (home.path(), proj.path());
    cmd_in(h, d).arg("sync").assert().code(2);
    std::fs::write(d.join(".project-status"), "title = \"x\"\nbogus = 1\n").unwrap();
    cmd_in(h, d).arg("sync").assert().code(4);
    assert!(!stdout(cmd(h).arg("list")).contains('x'));
}

#[test]
fn scan_is_a_dry_run_until_apply_and_json_has_the_plan() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let (h, r) = (home.path(), root.path());
    for (dir, body) in [
        ("one", STATUS_FILE),
        ("two/nested", "title = \"Second\""),
        ("bad", "title = "),
    ] {
        std::fs::create_dir_all(r.join(dir)).unwrap();
        std::fs::write(r.join(dir).join(".project-status"), body).unwrap();
    }
    let dry = stdout(cmd(h).args(["scan", r.to_str().unwrap()]));
    assert!(
        dry.contains("new") && dry.contains("invalid") && dry.contains("--apply"),
        "{dry}"
    );
    assert!(
        !stdout(cmd(h).arg("list")).contains("second"),
        "scan alone never writes"
    );

    let json = stdout(cmd(h).args(["--json", "scan", r.to_str().unwrap(), "--depth", "1"]));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        v.as_array().unwrap().len(),
        2,
        "depth 1 does not reach two/nested: {json}"
    );

    let applied = stdout(cmd(h).args(["scan", r.to_str().unwrap(), "--apply"]));
    assert!(
        applied.contains("created") && applied.contains("1 invalid"),
        "{applied}"
    );
    let list = stdout(cmd(h).arg("list"));
    assert!(
        list.contains("filed-project") && list.contains("second"),
        "{list}"
    );
    let rerun = stdout(cmd(h).args(["scan", r.to_str().unwrap()]));
    assert!(
        rerun.contains("2 in sync") && !rerun.contains("--apply"),
        "{rerun}"
    );
}

#[test]
fn cli_changes_are_written_back_unless_the_file_has_unsynced_edits() {
    let home = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    let (h, d) = (home.path(), proj.path());
    std::fs::write(d.join(".project-status"), STATUS_FILE).unwrap();
    stdout(cmd_in(h, d).arg("sync"));

    stdout(cmd(h).args(["task", "add", "filed-project", "three"]));
    let file = std::fs::read_to_string(d.join(".project-status")).unwrap();
    assert!(file.contains("title = \"three\""), "{file}");
    assert!(stdout(cmd_in(h, d).arg("sync")).contains("In sync"));

    let mine = "title = \"Filed project\"\n# not synced yet\n";
    std::fs::write(d.join(".project-status"), mine).unwrap();
    let out = cmd(h)
        .args(["--json", "edit", "filed-project", "-s", "review"])
        .assert()
        .success()
        .get_output()
        .clone();
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("note:") && err.contains("pgui sync"), "{err}");
    serde_json::from_slice::<serde_json::Value>(&out.stdout).expect("stdout stays pure JSON");
    assert_eq!(
        std::fs::read_to_string(d.join(".project-status")).unwrap(),
        mine
    );
}
