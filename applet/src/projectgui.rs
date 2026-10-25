// SPDX-License-Identifier: MIT
//
// Talks to the `projectgui` CLI. The applet holds no rules of its own: reads use `--json list`, changes go through
// the same commands a user would type, so validation, the `.project-status` mirror and concurrency stay in one place.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub stage: String,
    pub priority: String,
    pub progress: Progress,
    #[serde(default)]
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Progress {
    pub done: u32,
    pub total: u32,
    pub percent: u8,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub status: String,
}

impl Project {
    pub fn in_progress(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.status == "in-progress")
            .count()
    }
}

/// Projects still being worked on, in the CLI's order (priority first).
pub fn open(projects: Vec<Project>) -> Vec<Project> {
    projects.into_iter().filter(|p| p.stage != "done").collect()
}

/// Share of finished tasks across `projects`, or `None` when they have no tasks.
pub fn overall(projects: &[Project]) -> Option<u8> {
    let (done, total) = projects.iter().fold((0, 0), |(d, t), p| {
        (d + p.progress.done, t + p.progress.total)
    });
    (total > 0).then(|| (done * 100 / total) as u8)
}

/// cosmic-panel starts applets with a minimal PATH that lacks `~/.local/bin`, where the installer puts `projectgui`.
fn binary() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = home
        .iter()
        .flat_map(|h| {
            [
                h.join(".local/bin/projectgui"),
                h.join(".cargo/bin/projectgui"),
            ]
        })
        .collect();
    candidates.push("/usr/local/bin/projectgui".into());
    candidates.push("/usr/bin/projectgui".into());
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(|| "projectgui".into())
}

async fn run(args: &[&str]) -> Result<Vec<u8>, String> {
    let bin = binary();
    let out = tokio::process::Command::new(&bin)
        .args(args)
        .env("NO_COLOR", "1")
        .env("PROJECTGUI_NO_UPDATE_CHECK", "1")
        .output()
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "projectgui not found; install it to ~/.local/bin".to_string()
            }
            _ => format!("failed to run {}: {e}", bin.display()),
        })?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(stderr
        .lines()
        .next()
        .map(str::to_string)
        .unwrap_or_else(|| format!("projectgui exited with {}", out.status)))
}

pub async fn fetch() -> Result<Vec<Project>, String> {
    let json = run(&["--json", "list"]).await?;
    serde_json::from_slice(&json).map_err(|e| format!("could not read projectgui output: {e}"))
}

/// Marks a task done, or back to todo when `done` is false. Takes the exact slug and full task id, as every
/// ProjectGui mutation requires.
pub async fn set_done(slug: String, task_id: String, done: bool) -> Result<(), String> {
    let verb = if done { "done" } else { "reopen" };
    run(&["task", verb, &slug, &task_id]).await.map(|_| ())
}

/// Opens the window; a running one is focused instead (the GUI's single-instance socket).
pub fn open_gui() {
    if let Err(e) = std::process::Command::new(binary()).arg("gui").spawn() {
        eprintln!("projectgui-applet: could not start projectgui gui: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"[
      {"id":"A","slug":"site","title":"Site","stage":"in-progress","priority":"high","archived_at":null,
       "progress":{"done":1,"total":3,"percent":33},
       "tasks":[{"id":"T1","title":"Copy","status":"done","position":0},
                {"id":"T2","title":"Fonts","status":"in-progress","position":1},
                {"id":"T3","title":"Launch","status":"todo","position":2}]},
      {"id":"B","slug":"blog","title":"Blog","stage":"done","priority":"low",
       "progress":{"done":2,"total":2,"percent":100},"tasks":[]},
      {"id":"C","slug":"lab","title":"Lab","stage":"idea","priority":"medium",
       "progress":{"done":0,"total":0,"percent":0}}
    ]"#;

    #[test]
    fn reads_cli_json_and_summarizes_open_projects() {
        let all: Vec<Project> = serde_json::from_str(LIST).unwrap();
        let open = open(all);
        assert_eq!(
            open.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
            ["site", "lab"]
        );
        assert_eq!(open[0].in_progress(), 1);
        assert_eq!(overall(&open), Some(33));
        assert_eq!(overall(&open[1..]), None);
    }
}
