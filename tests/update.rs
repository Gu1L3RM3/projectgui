#![cfg(feature = "update")]
mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use common::{cmd, stdout};

struct Stub {
    base: String,
    hits: Arc<AtomicUsize>,
}

impl Stub {
    fn start(latest_tag: &str, notice: Option<String>) -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!(
            "http://127.0.0.1:{}",
            server.server_addr().to_ip().unwrap().port()
        );
        let hits = Arc::new(AtomicUsize::new(0));
        let (h, tag) = (hits.clone(), latest_tag.to_string());
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                h.fetch_add(1, Ordering::SeqCst);
                let body = match req.url() {
                    "/latest" => Some(format!(
                        r#"{{"tag_name":"{tag}","html_url":"https://example.com/releases/{tag}"}}"#
                    )),
                    "/notice.json" => notice.clone(),
                    _ => None,
                };
                let _ = match body {
                    Some(b) => req.respond(tiny_http::Response::from_string(b)),
                    None => {
                        req.respond(tiny_http::Response::from_string("nope").with_status_code(404))
                    }
                };
            }
        });
        Self { base, hits }
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    fn wait_for_hits(&self, n: usize) {
        let start = Instant::now();
        while self.hits() < n {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "stub only saw {} requests",
                self.hits()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn with_stub(home: &std::path::Path, stub: &Stub) -> assert_cmd::Command {
    let mut c = cmd(home);
    c.env("PROJECTGUI_UPDATE_BASE", &stub.base);
    c
}

#[test]
fn check_reports_available_with_exit_code_10_and_json() {
    let home = tempfile::tempdir().unwrap();
    let stub = Stub::start("v99.0.0", None);
    let out = with_stub(home.path(), &stub)
        .args(["update", "--check"])
        .assert()
        .code(10)
        .get_output()
        .stdout
        .clone();
    assert!(String::from_utf8_lossy(&out).contains("99.0.0"));

    let json = with_stub(home.path(), &stub)
        .args(["--json", "update", "--check"])
        .assert()
        .code(10)
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(
        (v["update_available"].as_bool(), v["latest"].as_str()),
        (Some(true), Some("99.0.0"))
    );
    assert_eq!(v["current"], current());
}

#[test]
fn up_to_date_exits_zero() {
    let home = tempfile::tempdir().unwrap();
    let stub = Stub::start(&format!("v{}", current()), None);
    let out = stdout(with_stub(home.path(), &stub).args(["update", "--check"]));
    assert!(out.contains("up to date"), "{out}");
}

#[test]
fn unreachable_source_is_a_clean_error_not_a_hang() {
    let home = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let out = cmd(home.path())
        .env("PROJECTGUI_UPDATE_BASE", "http://127.0.0.1:9")
        .args(["update", "--check"])
        .assert()
        .code(1)
        .get_output()
        .stderr
        .clone();
    assert!(String::from_utf8_lossy(&out).contains("could not check for updates"));
    assert!(started.elapsed() < Duration::from_secs(8));
}

#[test]
fn update_never_modifies_a_non_installer_binary() {
    let home = tempfile::tempdir().unwrap();
    let stub = Stub::start("v99.0.0", None);
    let out = stdout(with_stub(home.path(), &stub).args(["update", "--yes"]));
    assert!(
        out.contains("Installed via") && out.contains("to upgrade"),
        "{out}"
    );
}

#[test]
fn a_receipt_for_another_install_does_not_make_this_binary_self_updating() {
    let home = tempfile::tempdir().unwrap();
    // The receipt the official installer writes, for a copy installed somewhere else.
    std::fs::write(
        home.path().join("projectgui-receipt.json"),
        r#"{"binaries":["projectgui"],"binary_aliases":{},"cdylibs":[],"cstaticlibs":[],
            "install_layout":"cargo-home","install_prefix":"/nonexistent/prefix","modify_path":false,
            "provider":{"source":"cargo-dist","version":"0.32.0"},
            "source":{"app_name":"projectgui","name":"projectgui","owner":"Gu1L3RM3","release_type":"github"},
            "version":"0.1.0"}"#,
    )
    .unwrap();
    let stub = Stub::start("v99.0.0", None);
    let out = stdout(with_stub(home.path(), &stub).args(["update", "--yes"]));
    assert!(
        out.contains("Installed via") && out.contains("to upgrade"),
        "{out}"
    );
}

#[test]
fn passive_notice_uses_the_cache_and_checks_once_per_day() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let stub = Stub::start(
        "v99.0.0",
        Some(r#"{"id":"n1","severity":"warn","message":"Please update soon"}"#.into()),
    );
    let run = |args: &[&str]| {
        let mut c = with_stub(h, &stub);
        c.env("PROJECTGUI_NOTICE_FORCE_TTY", "1").args(args);
        String::from_utf8(c.assert().success().get_output().stderr.clone()).unwrap()
    };

    // First run: nothing cached yet, so nothing printed; a detached child refreshes the cache.
    assert_eq!(run(&["list"]), "");
    stub.wait_for_hits(2); // /latest and /notice.json
    std::thread::sleep(Duration::from_millis(300)); // let the child write the cache

    let second = run(&["list"]);
    assert!(
        second.contains("99.0.0 is available") && second.contains("projectgui update"),
        "{second}"
    );
    assert!(second.contains("[warning] Please update soon"), "{second}");

    let third = run(&["list"]);
    assert!(
        third.contains("99.0.0 is available"),
        "version hint repeats: {third}"
    );
    assert!(
        !third.contains("Please update soon"),
        "a notice is shown once: {third}"
    );

    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(stub.hits(), 2, "throttled to one check per day");
}

#[test]
fn opt_outs_and_machine_output_stay_silent_and_offline() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let stub = Stub::start("v99.0.0", Some(r#"{"id":"n1","message":"hi"}"#.into()));
    let base = |c: &mut assert_cmd::Command| {
        c.env("PROJECTGUI_UPDATE_BASE", &stub.base)
            .env("PROJECTGUI_NOTICE_FORCE_TTY", "1");
    };

    let mut c = cmd(h);
    base(&mut c);
    c.env("PROJECTGUI_NO_UPDATE_CHECK", "1")
        .arg("list")
        .assert()
        .success()
        .stderr("");

    stdout(cmd(h).args(["config", "set", "update.check", "false"]));
    let mut c = cmd(h);
    base(&mut c);
    c.arg("list").assert().success().stderr("");

    stdout(cmd(h).args(["config", "set", "update.check", "true"]));
    let mut c = cmd(h);
    base(&mut c);
    c.args(["--json", "list"]).assert().success().stderr("");

    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        stub.hits(),
        0,
        "no request may be made in any opted-out or machine mode"
    );
}

#[test]
fn hostile_notice_text_cannot_inject_terminal_escapes() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let notice = serde_json::json!({ "id": "x", "severity": "critical", "message": "\u{1b}[2J\u{1b}]0;owned\u{7}Update now", "url": "javascript:alert(1)" });
    let stub = Stub::start("v99.0.0", Some(notice.to_string()));
    let mut c = with_stub(h, &stub);
    c.env("PROJECTGUI_NOTICE_FORCE_TTY", "1")
        .arg("list")
        .assert()
        .success();
    stub.wait_for_hits(2);
    std::thread::sleep(Duration::from_millis(300));
    let mut c = with_stub(h, &stub);
    let err = String::from_utf8(
        c.env("PROJECTGUI_NOTICE_FORCE_TTY", "1")
            .arg("list")
            .assert()
            .success()
            .get_output()
            .stderr
            .clone(),
    )
    .unwrap();
    assert!(err.contains("Update now"), "{err:?}");
    assert!(
        !err.contains('\u{1b}') && !err.contains('\u{7}') && !err.contains("javascript"),
        "{err:?}"
    );
}

#[test]
fn version_check_shows_latest() {
    let home = tempfile::tempdir().unwrap();
    let stub = Stub::start("v99.0.0", None);
    let out = stdout(with_stub(home.path(), &stub).args(["version", "--check"]));
    assert!(out.contains("latest: 99.0.0"), "{out}");
}
