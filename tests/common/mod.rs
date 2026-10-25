#![allow(dead_code)]
use std::path::Path;

use assert_cmd::Command;

/// A `projectgui` command isolated to its own database and config.
pub fn cmd(home: &Path) -> Command {
    let mut c = Command::cargo_bin("projectgui").unwrap();
    c.env("PROJECTGUI_DB", home.join("test.db"))
        .env("PROJECTGUI_CONFIG", home.join("config.toml"))
        .env("NO_COLOR", "1")
        .env_remove("PROJECTGUI_NO_UPDATE_CHECK")
        .current_dir(home);
    c
}

/// Same isolated store, but run from another working directory.
pub fn cmd_in(home: &Path, dir: &Path) -> Command {
    let mut c = cmd(home);
    c.current_dir(dir);
    c
}

pub fn stdout(c: &mut Command) -> String {
    String::from_utf8(c.assert().success().get_output().stdout.clone()).unwrap()
}
