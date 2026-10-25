#![allow(dead_code)]
use std::path::Path;

use assert_cmd::Command;

/// A `pgui` command isolated to its own database, config and install receipt.
pub fn cmd(home: &Path) -> Command {
    let mut c = Command::cargo_bin("pgui").unwrap();
    c.env("PROJECTGUI_DB", home.join("test.db"))
        .env("PROJECTGUI_CONFIG", home.join("config.toml"))
        // Where axoupdater looks for the installer receipt; never the real one of an installed pgui.
        .env("AXOUPDATER_CONFIG_PATH", home)
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
