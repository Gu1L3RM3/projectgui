//! The `.project-status` file: one TOML file at the root of a project directory that describes the project.
//!
//! Both directions are kept in step:
//! - file -> database: `sync_dir` loads the file in one transaction. It never deletes (a task missing from the file
//!   is kept and reported) and is idempotent.
//! - database -> file: after any change made through the service, an existing file is rewritten to match
//!   (`mirror`). A file is never created that way, and a file edited since ProjectGui last read or wrote it is left
//!   alone (with a notice), so edits that were not synced yet are never lost.
//!
//! "Last read or wrote" is the file content stored in `kv` under `project_file:<project id>`.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{NewTask, Service, build_reference, build_task, new_id};
use chrono::Utc;

use crate::domain::model::{clean_text, clean_title, clean_url};
use crate::domain::{AppError, Priority, Project, RefKind, Result, Stage, Task, TaskStatus};
use crate::repo::{Repository, Write};

pub const FILE_NAME: &str = ".project-status";
pub const DEFAULT_DEPTH: usize = 3;
pub const MAX_DEPTH: usize = 6;
/// A project file is a short summary; anything bigger is not one.
const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_FOUND: usize = 200;
/// Directories that never hold a project of their own and can be huge.
const SKIP_DIRS: &[&str] = &["node_modules", "target"];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectFile {
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<Priority>,
    pub stage: Option<Stage>,
    #[serde(default, rename = "task")]
    pub tasks: Vec<FileTask>,
    #[serde(default, rename = "reference")]
    pub references: Vec<FileRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileTask {
    pub title: String,
    pub status: Option<TaskStatus>,
    pub description: Option<String>,
    pub priority: Option<Priority>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRef {
    pub kind: RefKind,
    pub value: String,
    pub url: Option<String>,
}

/// Parses and validates a file; fields come back trimmed so later comparisons are plain equality.
pub fn parse(text: &str) -> Result<ProjectFile> {
    let mut f: ProjectFile =
        toml::from_str(text).map_err(|e| AppError::invalid("project file", e.to_string()))?;
    f.title = clean_title("title", &f.title)?;
    f.description = f
        .description
        .as_deref()
        .map(|d| clean_text("description", d))
        .transpose()?;
    for t in &mut f.tasks {
        t.title = clean_title("task title", &t.title)?;
        t.description = t
            .description
            .as_deref()
            .map(|d| clean_text("task description", d))
            .transpose()?;
    }
    for (i, t) in f.tasks.iter().enumerate() {
        if f.tasks[..i].iter().any(|o| o.title == t.title) {
            return Err(AppError::invalid(
                "task title",
                format!("'{}' appears twice; titles identify tasks", t.title),
            ));
        }
    }
    for r in &mut f.references {
        r.value = clean_title("reference", &r.value)?;
        r.url = clean_url(r.url.as_deref())?;
    }
    Ok(f)
}

/// Reads the file in `dir`. Symlinks and oversized files are refused: the name is fixed, so this can only
/// ever read a small regular file called `.project-status`.
pub fn read_text(dir: &Path) -> Result<String> {
    read_file(&dir.join(FILE_NAME))
}

fn read_file(path: &Path) -> Result<String> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| AppError::NotFound {
        what: "project file (run `pgui init-file` to create one)",
        query: path.display().to_string(),
    })?;
    if !meta.is_file() {
        return Err(AppError::invalid(
            "project file",
            format!("{} must be a regular file, not a link", path.display()),
        ));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(AppError::invalid(
            "project file",
            format!(
                "{} is larger than {} KiB",
                path.display(),
                MAX_FILE_BYTES / 1024
            ),
        ));
    }
    std::fs::read_to_string(path).map_err(|e| AppError::Storage(format!("{}: {e}", path.display())))
}

/// Puts `src`'s file at `dst` unless something is already there (`AlreadyExists`). A hard link makes the check and
/// the placement one atomic step. `src` itself is left for the caller to remove.
fn place(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::hard_link(src, dst) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
            // shortcut: without hard links (FAT, some network shares) this checks then renames, which can replace a
            // file created in between; replace with a platform no-replace rename if such filesystems matter.
            if std::fs::symlink_metadata(dst).is_ok() {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            std::fs::rename(src, dst)
        }
        other => other,
    }
}

fn kv_key(project_id: &str) -> String {
    format!("project_file:{project_id}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Create,
    Update,
    InSync,
}

/// What a sync would change. Pure data: the same value drives dry runs and real runs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SyncPlan {
    pub action: Action,
    /// Project fields that differ (`title`, `description`, `priority`, `stage`).
    pub changed_fields: Vec<&'static str>,
    pub tasks_to_add: Vec<String>,
    pub tasks_to_update: Vec<String>,
    pub references_to_add: usize,
    /// Tasks stored for the project but absent from the file. Left alone: sync never deletes.
    pub not_in_file: Vec<String>,
}

pub fn plan(existing: Option<&Project>, f: &ProjectFile) -> SyncPlan {
    let Some(p) = existing else {
        return SyncPlan {
            action: Action::Create,
            changed_fields: vec![],
            tasks_to_add: f.tasks.iter().map(|t| t.title.clone()).collect(),
            tasks_to_update: vec![],
            references_to_add: f.references.len(),
            not_in_file: vec![],
        };
    };
    let mut changed_fields = Vec::new();
    if f.title != p.title {
        changed_fields.push("title");
    }
    if f.description.as_ref().is_some_and(|d| *d != p.description) {
        changed_fields.push("description");
    }
    if f.priority.is_some_and(|x| x != p.priority) {
        changed_fields.push("priority");
    }
    if f.stage.is_some_and(|x| x != p.stage) {
        changed_fields.push("stage");
    }
    let mut tasks_to_add = Vec::new();
    let mut tasks_to_update = Vec::new();
    for t in &f.tasks {
        match p.tasks.iter().find(|s| s.title == t.title) {
            None => tasks_to_add.push(t.title.clone()),
            Some(s) if task_differs(s, t) => tasks_to_update.push(t.title.clone()),
            Some(_) => {}
        }
    }
    let references_to_add = f
        .references
        .iter()
        .filter(|r| {
            !p.references
                .iter()
                .any(|s| s.kind == r.kind && s.value == r.value)
        })
        .count();
    let not_in_file = p
        .tasks
        .iter()
        .filter(|s| !f.tasks.iter().any(|t| t.title == s.title))
        .map(|s| s.title.clone())
        .collect();
    let in_sync = changed_fields.is_empty()
        && tasks_to_add.is_empty()
        && tasks_to_update.is_empty()
        && references_to_add == 0;
    SyncPlan {
        action: if in_sync {
            Action::InSync
        } else {
            Action::Update
        },
        changed_fields,
        tasks_to_add,
        tasks_to_update,
        references_to_add,
        not_in_file,
    }
}

fn task_differs(stored: &Task, f: &FileTask) -> bool {
    f.status.is_some_and(|s| s != stored.status)
        || f.priority.is_some_and(|p| stored.priority != Some(p))
        || f.description.as_ref().is_some_and(|d| {
            Some(d.as_str()).filter(|d| !d.is_empty()) != stored.description.as_deref()
        })
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncReport {
    pub dir: String,
    /// False for a dry run.
    pub applied: bool,
    pub title: String,
    pub project_id: Option<String>,
    pub slug: Option<String>,
    #[serde(flatten)]
    pub plan: SyncPlan,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanItem {
    pub dir: String,
    pub report: Option<SyncReport>,
    /// Why the file could not be read or applied; `report` is then absent.
    pub error: Option<String>,
}

/// Directories under `root` (at most `depth` levels down) that hold a project file. Does not follow symlinks, skips
/// dot-directories, `node_modules` and `target`, and stops after `MAX_FOUND` so a huge tree cannot hang a UI or an
/// agent.
pub fn discover(root: &Path, depth: usize) -> Result<Vec<PathBuf>> {
    let root = root
        .canonicalize()
        .map_err(|e| AppError::invalid("path", format!("{}: {e}", root.display())))?;
    let depth = depth.min(MAX_DEPTH);
    let mut found = Vec::new();
    let mut stack = vec![(root, 0usize)];
    while let Some((dir, level)) = stack.pop() {
        if found.len() >= MAX_FOUND {
            break;
        }
        if std::fs::symlink_metadata(dir.join(FILE_NAME)).is_ok() {
            found.push(dir.clone());
        }
        if level == depth {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue; // unreadable subdirectory: skip it, the rest of the tree is still useful
        };
        let mut subdirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_ref())
            })
            .map(|e| e.path())
            .collect();
        subdirs.sort();
        // reversed so the stack pops them in alphabetical order
        stack.extend(subdirs.into_iter().rev().map(|d| (d, level + 1)));
    }
    found.sort();
    Ok(found)
}

/// Text of a starter file: the project's current state, or a commented template for a new one.
pub fn render_file(project: Option<&Project>, title_hint: &str) -> String {
    let q = |s: &str| toml::Value::String(s.to_string()).to_string();
    let Some(p) = project else {
        return format!(
            "# ProjectGui keeps this file and its database in step. Edit it, then run `pgui sync`;\n\
             # changes made in ProjectGui are written back here (comments are not kept). Tasks are matched by title.\n\
             title = {}\n\
             # description = \"What this project is\"\n\
             # priority = \"medium\"   # low | medium | high | critical\n\
             # stage = \"idea\"        # idea | planning | in-progress | review | done | paused\n\n\
             # [[task]]\n\
             # title = \"First step\"\n\
             # status = \"todo\"       # todo | in-progress | blocked | done\n\n\
             # [[reference]]\n\
             # kind = \"pr\"           # commit | pr | issue\n\
             # value = \"#1\"\n",
            q(title_hint)
        );
    };
    let mut out = format!("title = {}\n", q(&p.title));
    if !p.description.is_empty() {
        out += &format!("description = {}\n", q(&p.description));
    }
    out += &format!(
        "priority = {}\nstage = {}\n",
        q(p.priority.as_str()),
        q(p.stage.as_str())
    );
    for t in &p.tasks {
        out += &format!(
            "\n[[task]]\ntitle = {}\nstatus = {}\n",
            q(&t.title),
            q(t.status.as_str())
        );
        if let Some(d) = &t.description {
            out += &format!("description = {}\n", q(d));
        }
        if let Some(x) = t.priority {
            out += &format!("priority = {}\n", q(x.as_str()));
        }
    }
    for r in &p.references {
        out += &format!(
            "\n[[reference]]\nkind = {}\nvalue = {}\n",
            q(r.kind.as_str()),
            q(&r.value)
        );
        if let Some(u) = &r.url {
            out += &format!("url = {}\n", q(u));
        }
    }
    out
}

impl<R: Repository> Service<R> {
    /// Loads (or, with `apply == false`, only plans) the file in `dir`. The project is the one whose `path` equals
    /// `dir`; without one, a project is created and linked. Every write lands in one transaction.
    pub fn sync_dir(&mut self, dir: &Path, apply: bool) -> Result<SyncReport> {
        let dir = dir
            .canonicalize()
            .map_err(|e| AppError::invalid("path", format!("{}: {e}", dir.display())))?;
        let text = read_text(&dir)?;
        let f = parse(&text)?;
        let dir_str = dir.to_string_lossy().into_owned();
        let existing = self.project_at(&dir)?;
        let mut report = SyncReport {
            dir: dir_str.clone(),
            applied: false,
            title: f.title.clone(),
            project_id: existing.as_ref().map(|p| p.id.clone()),
            slug: existing.as_ref().map(|p| p.slug.clone()),
            plan: plan(existing.as_ref(), &f),
        };
        if !apply {
            return Ok(report);
        }
        let now = Utc::now();
        let id = match existing {
            None => {
                let id = new_id();
                let mut project = Project {
                    id: id.clone(),
                    slug: String::new(),
                    title: f.title.clone(),
                    description: f.description.clone().unwrap_or_default(),
                    priority: f.priority.unwrap_or(Priority::Medium),
                    stage: f.stage.unwrap_or(Stage::Idea),
                    path: Some(dir_str),
                    created_at: now,
                    updated_at: now,
                    archived_at: None,
                    aliases: vec![],
                    tasks: f
                        .tasks
                        .iter()
                        .map(|t| build_task(&id, new_task(t)))
                        .collect::<Result<_>>()?,
                    references: f
                        .references
                        .iter()
                        .map(|r| build_reference(&id, None, r.kind, &r.value, r.url.as_deref()))
                        .collect::<Result<_>>()?,
                };
                let key = kv_key(&id);
                self.insert_with_free_slug(&mut project, &[Write::KvSet(&key, &text)])?;
                report.slug = Some(project.slug);
                id
            }
            Some(p) => {
                let key = kv_key(&p.id);
                let known = self.repo.kv_get(&key)?.as_deref() == Some(text.as_str());
                if report.plan.action == Action::InSync && known {
                    report.applied = true;
                    return Ok(report);
                }
                // Planned again against the row as stored inside the write transaction, so a change another
                // process committed after the dry-run plan above is built on, not overwritten.
                let mut applied = None;
                self.repo.modify_project(
                    &p.id,
                    &mut |p| {
                        let pl = plan(Some(p), &f);
                        if !pl.changed_fields.is_empty() {
                            p.title = f.title.clone();
                            if let Some(d) = &f.description {
                                p.description = d.clone();
                            }
                            p.priority = f.priority.unwrap_or(p.priority);
                            p.stage = f.stage.unwrap_or(p.stage);
                            p.updated_at = now;
                        }
                        for t in &f.tasks {
                            if pl.tasks_to_add.contains(&t.title) {
                                let new = build_task(&p.id, new_task(t))?;
                                p.tasks.push(new);
                            } else if pl.tasks_to_update.contains(&t.title)
                                && let Some(stored) =
                                    p.tasks.iter_mut().find(|s| s.title == t.title)
                            {
                                if let Some(d) = &t.description {
                                    stored.description = Some(d.clone()).filter(|d| !d.is_empty());
                                }
                                if let Some(x) = t.priority {
                                    stored.priority = Some(x);
                                }
                                stored.updated_at = now;
                                if let Some(st) = t.status {
                                    stored.set_status(st, now);
                                }
                            }
                        }
                        for r in &f.references {
                            if !p
                                .references
                                .iter()
                                .any(|s| s.kind == r.kind && s.value == r.value)
                            {
                                let new = build_reference(
                                    &p.id,
                                    None,
                                    r.kind,
                                    &r.value,
                                    r.url.as_deref(),
                                )?;
                                p.references.push(new);
                            }
                        }
                        applied = Some(pl);
                        Ok(())
                    },
                    &[Write::KvSet(&key, &text)],
                )?;
                report.plan = applied.expect("modify_project ran the change");
                p.id
            }
        };
        report.project_id = Some(id.clone());
        report.applied = true;
        // Tasks only the database knows about go into the file, so the two stay in step.
        if !report.plan.not_in_file.is_empty() {
            let p = self.fetch(&id)?;
            self.mirror(&p);
        }
        Ok(report)
    }

    /// Scans `root` and plans or applies every file found. One bad file does not stop the others.
    pub fn scan(&mut self, root: &Path, depth: usize, apply: bool) -> Result<Vec<ScanItem>> {
        Ok(discover(root, depth)?
            .into_iter()
            .map(|dir| {
                let shown = dir.to_string_lossy().into_owned();
                match self.sync_dir(&dir, apply) {
                    Ok(report) => ScanItem {
                        dir: shown,
                        report: Some(report),
                        error: None,
                    },
                    Err(e) => ScanItem {
                        dir: shown,
                        report: None,
                        error: Some(e.to_string()),
                    },
                }
            })
            .collect())
    }

    /// Writes a starter file into `dir`: `project`'s current state (and remembers it, so later changes are
    /// mirrored), or a commented template. Refuses to replace an existing file unless `force`.
    pub fn init_file(
        &mut self,
        dir: &Path,
        project: Option<&Project>,
        title_hint: &str,
        force: bool,
    ) -> Result<PathBuf> {
        let path = dir.join(FILE_NAME);
        let text = render_file(project, title_hint);
        let mut open = std::fs::OpenOptions::new();
        open.write(true);
        if force {
            open.create(true).truncate(true);
        } else {
            open.create_new(true);
        }
        let mut file = open.open(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                AppError::Conflict(format!(
                    "{} already exists (use --force to replace it)",
                    path.display()
                ))
            } else {
                AppError::Storage(format!("{}: {e}", path.display()))
            }
        })?;
        std::io::Write::write_all(&mut file, text.as_bytes())
            .map_err(|e| AppError::Storage(format!("{}: {e}", path.display())))?;
        if let Some(p) = project {
            self.repo.kv_set(&kv_key(&p.id), &text)?;
        }
        Ok(path)
    }

    /// Database -> file. Rewrites `p`'s existing project file to match `p`. Never creates a file and never
    /// overwrites one edited since ProjectGui last saw it; problems become notices, because the database change
    /// they follow has already been committed.
    /// shortcut: the file is regenerated, so comments and key order in it are not kept; switch to `toml_edit` if
    /// people annotate their files.
    pub(super) fn mirror(&mut self, p: &Project) {
        let Some(dir) = p.path.as_deref() else {
            return;
        };
        let dir = Path::new(dir);
        let path = dir.join(FILE_NAME);
        if std::fs::symlink_metadata(&path).is_err() {
            return; // no file: the project does not use one
        }
        let tmp = dir.join(format!("{FILE_NAME}.{}.tmp", new_id()));
        let result = self.mirror_via(p, dir, &tmp);
        let _ = std::fs::remove_file(&tmp);
        if let Err(e) = result {
            self.notices
                .push(format!("{} was not updated: {e}", path.display()));
        }
    }

    fn mirror_via(&mut self, p: &Project, dir: &Path, tmp: &Path) -> Result<()> {
        let path = dir.join(FILE_NAME);
        let unsynced = format!(
            "{} has edits that were not synced, so it was not updated. Run `pgui sync` in {} to load \
             them (the file's values win), then repeat this change if it was undone.",
            path.display(),
            dir.display()
        );
        let key = kv_key(&p.id);
        let seen = self.repo.kv_get(&key)?;
        let current = read_text(dir)?;
        if seen.as_deref() != Some(current.as_str()) {
            self.notices.push(unsynced);
            return Ok(());
        }
        let text = render_file(Some(p), &p.title);
        if text == current {
            return Ok(());
        }
        parse(&text)?; // never write a file that sync would then reject (old duplicate titles, for example)
        let io = |e: std::io::Error| AppError::Storage(e.to_string());
        std::fs::write(tmp, &text).map_err(io)?;
        // Move the file aside before checking it: an edit saved from now on creates a new file instead of being
        // replaced, and the check reads exactly what is being replaced. Readers may find no file for that instant.
        let aside = dir.join(format!("{FILE_NAME}.{}.prev", new_id()));
        std::fs::rename(&path, &aside).map_err(io)?;
        if read_file(&aside).ok().as_deref() != Some(current.as_str()) {
            // Edited after the first check: put it back, unless yet another file has appeared meanwhile.
            return match place(&aside, &path) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&aside);
                    self.notices.push(unsynced);
                    Ok(())
                }
                Err(_) => Err(AppError::Conflict(format!(
                    "it was edited twice while being updated; the earlier edit is kept in {}",
                    aside.display()
                ))),
            };
        }
        let placed = place(tmp, &path);
        let _ = std::fs::remove_file(&aside); // holds `current`, which the database already knows
        match placed {
            Ok(()) => self.repo.kv_set(&key, &text),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                self.notices.push(unsynced); // a new file was saved meanwhile; it stays
                Ok(())
            }
            Err(e) => Err(io(e)),
        }
    }

    /// The single project whose linked path is `dir` (canonical). Paths stored before they were canonicalized on
    /// write are resolved here, so an old symlinked or `..` path still finds its project instead of making a second.
    fn project_at(&mut self, dir: &Path) -> Result<Option<Project>> {
        let mut hits: Vec<Project> = self
            .repo
            .list_projects(true)?
            .into_iter()
            .filter(|p| {
                p.path
                    .as_deref()
                    .map(Path::new)
                    .is_some_and(|x| x == dir || x.canonicalize().is_ok_and(|c| c == dir))
            })
            .collect();
        match hits.len() {
            0 | 1 => Ok(hits.pop()),
            _ => Err(AppError::Ambiguous {
                query: dir.display().to_string(),
                candidates: hits.into_iter().map(|p| p.slug).collect(),
            }),
        }
    }
}

fn new_task(t: &FileTask) -> NewTask {
    NewTask {
        title: t.title.clone(),
        description: t.description.clone(),
        priority: t.priority,
        status: t.status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::SqliteRepository;
    use crate::service::{Filter, NewProject, TaskPatch};

    fn svc() -> Service<SqliteRepository> {
        Service::new(SqliteRepository::open_in_memory().unwrap())
    }

    fn put(dir: &Path, text: &str) {
        std::fs::write(dir.join(FILE_NAME), text).unwrap();
    }

    fn on_disk(dir: &Path) -> String {
        std::fs::read_to_string(dir.join(FILE_NAME)).unwrap()
    }

    const FULL: &str = r##"
title = "  Website  "
description = "New site"
priority = "high"
stage = "in-progress"

[[task]]
title = "copy"
status = "done"

[[task]]
title = "launch"

[[reference]]
kind = "pr"
value = "#12"
url = "https://example.com/pr/12"
"##;

    /// A synced project and its directory.
    fn synced(s: &mut Service<SqliteRepository>, text: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), text);
        let id = s.sync_dir(dir.path(), true).unwrap().project_id.unwrap();
        (dir, id)
    }

    #[test]
    fn parse_trims_and_names_the_problem() {
        let f = parse(FULL).unwrap();
        assert_eq!(f.title, "Website");
        assert_eq!(f.tasks.len(), 2);
        for (bad, needle) in [
            ("title = \"\"", "title"),
            ("title = \"a\"\nbogus = 1", "bogus"),
            ("title = \"a\"\npriority = \"urgent\"", "urgent"),
            (
                "title = \"a\"\n[[task]]\ntitle = \"x\"\n[[task]]\ntitle = \"x\"",
                "twice",
            ),
            (
                "title = \"a\"\n[[reference]]\nkind = \"pr\"\nvalue = \"1\"\nurl = \"ftp://x\"",
                "https",
            ),
            ("not toml at all [", "project file"),
        ] {
            let err = parse(bad).unwrap_err().to_string();
            assert!(err.contains(needle), "{bad:?} -> {err}");
        }
    }

    #[test]
    fn first_sync_creates_and_second_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), FULL);
        let mut s = svc();
        let dry = s.sync_dir(dir.path(), false).unwrap();
        assert!(!dry.applied && dry.plan.action == Action::Create);
        assert!(
            s.list(&Filter::default()).unwrap().is_empty(),
            "dry run writes nothing"
        );

        let first = s.sync_dir(dir.path(), true).unwrap();
        assert!(first.applied && first.plan.action == Action::Create);
        let p = s.find(first.project_id.as_deref().unwrap()).unwrap();
        assert_eq!(
            (
                p.title.as_str(),
                p.priority,
                p.stage,
                p.tasks.len(),
                p.references.len()
            ),
            ("Website", Priority::High, Stage::InProgress, 2, 1)
        );
        assert_eq!(
            p.path.as_deref(),
            Some(dir.path().canonicalize().unwrap().to_str().unwrap())
        );
        assert_eq!(p.progress().done, 1);
        assert_eq!(
            on_disk(dir.path()),
            FULL,
            "a sync that adds nothing leaves the file as written"
        );

        let again = s.sync_dir(dir.path(), true).unwrap();
        assert_eq!(again.plan.action, Action::InSync);
        assert_eq!(
            s.find(&p.id).unwrap().updated_at,
            p.updated_at,
            "idempotent"
        );
    }

    #[test]
    fn file_changes_update_and_nothing_is_ever_deleted() {
        let mut s = svc();
        let (dir, id) = synced(&mut s, FULL);
        let edited = r##"
title = "Website v2"
stage = "review"

[[task]]
title = "launch"
status = "blocked"

[[task]]
title = "new one"

[[reference]]
kind = "pr"
value = "#12"

[[reference]]
kind = "issue"
value = "#3"
"##;
        put(dir.path(), edited);
        let r = s.sync_dir(dir.path(), true).unwrap();
        assert_eq!(r.plan.changed_fields, ["title", "stage"]);
        assert_eq!(r.plan.tasks_to_add, ["new one"]);
        assert_eq!(r.plan.tasks_to_update, ["launch"]);
        assert_eq!(r.plan.references_to_add, 1);
        assert_eq!(r.plan.not_in_file, ["copy"]);

        let p = s.find(&id).unwrap();
        assert_eq!(
            (p.title.as_str(), p.stage, p.priority),
            ("Website v2", Stage::Review, Priority::High)
        );
        assert_eq!(
            p.description, "New site",
            "keys absent from the file are left alone"
        );
        let status = |t: &str| p.tasks.iter().find(|x| x.title == t).unwrap().status;
        assert_eq!(status("launch"), TaskStatus::Blocked);
        assert_eq!(
            status("copy"),
            TaskStatus::Done,
            "kept although the file dropped it"
        );
        assert_eq!(p.tasks.len(), 3);
        assert_eq!(p.references.len(), 2);
        assert_eq!(p.slug, "website", "the slug never follows the title");

        // the task only the database had is written back, so the file now mirrors the database
        let text = on_disk(dir.path());
        assert!(text.contains("title = \"copy\""), "{text}");
        assert_eq!(
            s.sync_dir(dir.path(), false).unwrap().plan.action,
            Action::InSync
        );
    }

    #[test]
    fn an_invalid_task_in_an_update_writes_nothing() {
        let mut s = svc();
        let (dir, id) = synced(&mut s, "title = \"A\"\n[[task]]\ntitle = \"t\"\n");
        let before = s.find(&id).unwrap();
        // the project change is valid, the new task's description is not: the whole sync is refused
        let long = "x".repeat(10_001);
        put(
            dir.path(),
            &format!(
                "title = \"B\"\n[[task]]\ntitle = \"t\"\n[[task]]\ntitle = \"u\"\ndescription = \"{long}\"\n"
            ),
        );
        assert!(s.sync_dir(dir.path(), true).is_err());
        let after = s.find(&id).unwrap();
        assert_eq!((after.title.as_str(), after.tasks.len()), ("A", 1));
        assert_eq!(after.updated_at, before.updated_at);
    }

    #[test]
    fn service_changes_are_mirrored_into_an_existing_file() {
        let mut s = svc();
        let (dir, id) = synced(&mut s, FULL);
        let launch = s.find(&id).unwrap().tasks[1].id.clone();
        s.set_tasks_status(&id, std::slice::from_ref(&launch), TaskStatus::InProgress)
            .unwrap();
        s.add_tasks(
            &id,
            vec![NewTask {
                title: "qa".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        assert!(s.take_notices().is_empty());
        let text = on_disk(dir.path());
        assert!(
            text.contains("status = \"in-progress\"") && text.contains("title = \"qa\""),
            "{text}"
        );
        assert_eq!(
            s.sync_dir(dir.path(), false).unwrap().plan.action,
            Action::InSync,
            "file == database"
        );

        s.remove_task(&id, &launch, true).unwrap();
        assert!(
            !on_disk(dir.path()).contains("launch"),
            "an explicit removal reaches the file"
        );
        assert!(
            !std::fs::read_dir(dir.path()).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{FILE_NAME}."))),
            "no temporary or set-aside file left behind"
        );
    }

    #[test]
    fn duplicate_task_titles_never_reach_the_file() {
        let mut s = svc();
        let (dir, id) = synced(&mut s, FULL);
        let copy = |title: &str| NewTask {
            title: title.into(),
            ..Default::default()
        };
        assert!(s.add_tasks(&id, vec![copy("copy")]).is_err());
        assert!(s.add_tasks(&id, vec![copy("x"), copy("x")]).is_err());
        let launch = s.find(&id).unwrap().tasks[1].id.clone();
        let rename = TaskPatch {
            title: Some("copy".into()),
            ..Default::default()
        };
        assert!(s.update_task(&id, &launch, rename).is_err());
        assert_eq!(s.find(&id).unwrap().tasks.len(), 2, "nothing was written");

        // A duplicate stored before the rule existed: the file is left alone instead of becoming unreadable.
        let mut old = build_task(&id, copy("copy")).unwrap();
        old.id = new_id();
        s.repo.insert_task(&old).unwrap();
        let before = on_disk(dir.path());
        s.set_archived(&id, true).unwrap();
        assert_eq!(on_disk(dir.path()), before);
        let notes = s.take_notices();
        assert!(
            notes.len() == 1 && notes[0].contains("appears twice"),
            "{notes:?}"
        );
        assert!(
            s.sync_dir(dir.path(), false).is_ok(),
            "the file still parses"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_and_old_uncanonical_paths_find_the_same_project() {
        let mut s = svc();
        let (dir, id) = synced(&mut s, FULL);
        let real = dir.path().canonicalize().unwrap();
        let other = tempfile::tempdir().unwrap();
        let link = other.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let made = s
            .create_project(NewProject {
                title: "Via link".into(),
                path: Some(link.to_string_lossy().into_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(made.path.as_deref(), real.to_str(), "stored canonical");
        s.delete_project(&made.id, Some("Via link")).unwrap();

        // A path stored as typed, before paths were canonicalized on write.
        let mut p = s.repo.get_project(&id).unwrap().unwrap();
        p.path = Some(link.to_string_lossy().into_owned());
        s.repo.update_project(&p).unwrap();
        let report = s.sync_dir(&real, true).unwrap();
        assert_eq!(report.project_id.as_deref(), Some(id.as_str()));
        assert_eq!(
            s.list(&Filter::default()).unwrap().len(),
            1,
            "no second project"
        );
    }

    #[test]
    fn place_never_replaces_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst, fresh) = (
            dir.path().join("src"),
            dir.path().join("dst"),
            dir.path().join("fresh"),
        );
        std::fs::write(&src, "new").unwrap();
        std::fs::write(&dst, "user edit").unwrap();
        let err = place(&src, &dst).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "user edit");
        place(&src, &fresh).unwrap();
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "new");
    }

    #[test]
    fn unsynced_file_edits_are_never_overwritten() {
        let mut s = svc();
        let (dir, id) = synced(&mut s, FULL);
        let mine = "title = \"Website\"\n# my unsynced note\n[[task]]\ntitle = \"from me\"\n";
        put(dir.path(), mine);
        s.update_project(
            &id,
            crate::service::ProjectPatch {
                stage: Some(Stage::Review),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(on_disk(dir.path()), mine, "the user's edit survives");
        let notes = s.take_notices();
        assert!(
            notes.len() == 1 && notes[0].contains("pgui sync"),
            "{notes:?}"
        );

        // once synced, mirroring resumes
        s.sync_dir(dir.path(), true).unwrap();
        let t = s
            .find(&id)
            .unwrap()
            .tasks
            .iter()
            .find(|t| t.title == "from me")
            .unwrap()
            .id
            .clone();
        s.update_task(
            &id,
            &t,
            TaskPatch {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(s.take_notices().is_empty());
        assert!(on_disk(dir.path()).contains("status = \"done\""));
    }

    #[test]
    fn projects_without_a_file_never_get_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut s = svc();
        let p = s
            .create_project(NewProject {
                title: "Plain".into(),
                path: Some(path),
                ..Default::default()
            })
            .unwrap();
        s.add_tasks(
            &p.id,
            vec![NewTask {
                title: "t".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        assert!(!dir.path().join(FILE_NAME).exists());
        assert!(s.take_notices().is_empty());
    }

    #[test]
    fn init_file_writes_once_and_a_project_file_is_mirrored_right_away() {
        let mut s = svc();
        let dir = tempfile::tempdir().unwrap();
        s.init_file(dir.path(), None, "Hint", false).unwrap();
        assert_eq!(parse(&on_disk(dir.path())).unwrap().title, "Hint");
        assert!(matches!(
            s.init_file(dir.path(), None, "Hint", false),
            Err(AppError::Conflict(_))
        ));

        let path = dir
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let p = s
            .create_project(NewProject {
                title: "Linked".into(),
                path: Some(path),
                ..Default::default()
            })
            .unwrap();
        s.init_file(dir.path(), Some(&p), "", true).unwrap();
        s.add_tasks(
            &p.id,
            vec![NewTask {
                title: "t".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        assert!(
            on_disk(dir.path()).contains("title = \"t\""),
            "known file, so it is mirrored"
        );
    }

    #[test]
    fn two_projects_on_one_path_are_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut s = svc();
        for t in ["A", "B"] {
            s.create_project(NewProject {
                title: t.into(),
                path: Some(path.clone()),
                ..Default::default()
            })
            .unwrap();
        }
        put(dir.path(), "title = \"A\"");
        let err = s.sync_dir(dir.path(), true).unwrap_err();
        assert!(matches!(err, AppError::Ambiguous { .. }), "{err}");
    }

    #[test]
    fn discover_respects_depth_and_skips_noise() {
        let root = tempfile::tempdir().unwrap();
        let w = |rel: &str| {
            let d = root.path().join(rel);
            std::fs::create_dir_all(&d).unwrap();
            put(&d, "title = \"x\"");
        };
        for rel in ["", "a", "a/b/c/d", "node_modules/x", ".hidden/x", "bad"] {
            w(rel);
        }
        let names: Vec<_> = discover(root.path(), 3)
            .unwrap()
            .iter()
            .map(|d| d.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 3, "root, a, bad: {names:?}");
        assert!(
            discover(root.path(), 4)
                .unwrap()
                .iter()
                .any(|d| d.ends_with("d"))
        );
        assert_eq!(discover(root.path(), 0).unwrap().len(), 1, "root only");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_project_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::write(&real, "title = \"x\"").unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join(FILE_NAME)).unwrap();
        assert!(matches!(
            read_text(dir.path()),
            Err(AppError::Invalid { .. })
        ));
        assert!(matches!(
            read_text(&dir.path().join("missing")),
            Err(AppError::NotFound { .. })
        ));
    }

    #[test]
    fn scan_applies_good_files_and_reports_bad_ones() {
        let root = tempfile::tempdir().unwrap();
        for (d, text) in [("ok", "title = \"Ok\""), ("bad", "title = \"\"")] {
            let p = root.path().join(d);
            std::fs::create_dir_all(&p).unwrap();
            put(&p, text);
        }
        let mut s = svc();
        let items = s.scan(root.path(), 2, true).unwrap();
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .any(|i| i.error.as_deref().is_some_and(|e| e.contains("title")))
        );
        assert_eq!(s.list(&Filter::default()).unwrap().len(), 1);
    }

    #[test]
    fn rendered_files_parse_back_to_the_same_project() {
        let mut s = svc();
        let (dir, id) = synced(&mut s, FULL);
        let p = s.find(&id).unwrap();
        put(dir.path(), &render_file(Some(&p), "ignored"));
        assert_eq!(
            s.sync_dir(dir.path(), false).unwrap().plan.action,
            Action::InSync
        );
        let template = render_file(None, "My \"quoted\" app");
        assert_eq!(parse(&template).unwrap().title, "My \"quoted\" app");
    }
}
