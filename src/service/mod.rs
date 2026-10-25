mod backup;
pub mod projectfile;
pub mod query;

use chrono::Utc;
use ulid::Ulid;

use crate::domain::model::{
    clean_path, clean_text, clean_title, clean_url, ensure_title_free, resolve_project,
    resolve_reference, resolve_task, slugify,
};
use crate::domain::{
    AppError, Priority, Project, RefKind, Reference, Result, Stage, Task, TaskStatus,
};
use crate::repo::{Repository, Write};

pub use query::{Filter, SortKey, Summary};

#[derive(Debug, Clone, Default)]
pub struct NewTask {
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<Priority>,
    pub status: Option<TaskStatus>,
}

#[derive(Debug, Clone, Default)]
pub struct NewProject {
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<Priority>,
    pub stage: Option<Stage>,
    pub path: Option<String>,
    pub tasks: Vec<NewTask>,
}

/// `None` leaves a field alone. For nullable fields `Some(None)` clears them.
#[derive(Debug, Clone, Default)]
pub struct ProjectPatch {
    pub title: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub priority: Option<Priority>,
    pub stage: Option<Stage>,
    pub path: Option<Option<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub priority: Option<Option<Priority>>,
    pub status: Option<TaskStatus>,
}

pub struct Service<R: Repository> {
    repo: R,
    /// Things worth telling the user that are not errors (a project file left untouched, for example).
    /// Adapters drain them with `take_notices` after a call.
    notices: Vec<String>,
}

fn new_id() -> String {
    Ulid::generate().to_string()
}

fn build_task(project_id: &str, n: NewTask) -> Result<Task> {
    let now = Utc::now();
    let status = n.status.unwrap_or(TaskStatus::Todo);
    Ok(Task {
        id: new_id(),
        project_id: project_id.into(),
        title: clean_title("task title", &n.title)?,
        description: n
            .description
            .as_deref()
            .map(|d| clean_text("task description", d))
            .transpose()?
            .filter(|d| !d.is_empty()),
        status,
        priority: n.priority,
        position: 0,
        created_at: now,
        updated_at: now,
        completed_at: (status == TaskStatus::Done).then_some(now),
    })
}

/// `clean_path`, then the canonical form (symlinks and `..` resolved) when the path exists, so it compares equal to
/// the canonical directories that `sync` and `--here` look up.
fn link_path(raw: &str) -> Result<String> {
    let path = clean_path(raw)?;
    Ok(std::fs::canonicalize(&path).map_or(path, |c| c.to_string_lossy().into_owned()))
}

/// The task with exact id `id` in a freshly read project (it may have been removed meanwhile).
fn task_in<'a>(p: &'a mut Project, id: &str) -> Result<&'a mut Task> {
    p.tasks
        .iter_mut()
        .find(|t| t.id == id)
        .ok_or_else(|| AppError::NotFound {
            what: "task",
            query: id.into(),
        })
}

fn build_reference(
    project_id: &str,
    task_id: Option<String>,
    kind: RefKind,
    value: &str,
    url: Option<&str>,
) -> Result<Reference> {
    Ok(Reference {
        id: new_id(),
        project_id: project_id.into(),
        task_id,
        kind,
        value: clean_title("reference", value)?,
        url: clean_url(url)?,
        created_at: Utc::now(),
    })
}

impl<R: Repository> Service<R> {
    pub fn new(repo: R) -> Self {
        Self {
            repo,
            notices: Vec::new(),
        }
    }

    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    #[cfg(any(feature = "update", feature = "gui"))]
    pub fn repo(&mut self) -> &mut R {
        &mut self.repo
    }

    // ---- reads (fuzzy lookup allowed) ----

    pub fn list(&mut self, filter: &Filter) -> Result<Vec<Project>> {
        Ok(query::apply(self.repo.list_projects(true)?, filter))
    }

    pub fn find(&mut self, query: &str) -> Result<Project> {
        let all = self.repo.list_projects(true)?;
        resolve_project(&all, query, true).cloned()
    }

    pub fn find_for_path(&mut self, dir: &std::path::Path) -> Result<Option<Project>> {
        let all = self.repo.list_projects(false)?;
        Ok(query::project_for_path(&all, dir).cloned())
    }

    /// Read-only exact lookup by id, slug or alias (no fuzzy matching).
    pub fn find_exact(&mut self, query: &str) -> Result<Project> {
        self.exact(query)
    }

    #[cfg(feature = "mcp")]
    pub fn search(&mut self, query: &str) -> Result<Vec<Project>> {
        let all = self.repo.list_projects(false)?;
        Ok(crate::domain::model::search_projects(&all, query)
            .into_iter()
            .cloned()
            .collect())
    }

    #[cfg(feature = "mcp")]
    pub fn summary(&mut self) -> Result<Summary> {
        Ok(query::summarize(&self.repo.list_projects(false)?))
    }

    // ---- mutations (exact id / slug / alias only) ----

    fn exact(&mut self, query: &str) -> Result<Project> {
        let all = self.repo.list_projects(true)?;
        resolve_project(&all, query, false).cloned()
    }

    pub fn create_project(&mut self, n: NewProject) -> Result<Project> {
        let now = Utc::now();
        let id = new_id();
        let title = clean_title("title", &n.title)?;
        let tasks = n
            .tasks
            .into_iter()
            .map(|t| build_task(&id, t))
            .collect::<Result<Vec<_>>>()?;
        for (i, t) in tasks.iter().enumerate() {
            ensure_title_free(&tasks[..i], &t.title, &t.id)?;
        }
        let mut project = Project {
            id: id.clone(),
            slug: String::new(),
            description: n
                .description
                .as_deref()
                .map(|d| clean_text("description", d))
                .transpose()?
                .unwrap_or_default(),
            priority: n.priority.unwrap_or(Priority::Medium),
            stage: n.stage.unwrap_or(Stage::Idea),
            path: n.path.as_deref().map(link_path).transpose()?,
            created_at: now,
            updated_at: now,
            archived_at: None,
            aliases: vec![],
            tasks,
            references: vec![],
            title,
        };
        self.insert_with_free_slug(&mut project, &[])?;
        Ok(project)
    }

    /// Inserts `project` (plus `extra` writes, in the same transaction), trying `slug`, `slug-2`, ... until one is free.
    fn insert_with_free_slug(&mut self, project: &mut Project, extra: &[Write]) -> Result<()> {
        let base = slugify(&project.title);
        for attempt in 1..=100 {
            project.slug = if attempt == 1 {
                base.clone()
            } else {
                format!("{base}-{attempt}")
            };
            let mut writes = vec![Write::InsertProject(project)];
            writes.extend(extra.iter().map(Write::reborrow));
            match self.repo.write_batch(&writes) {
                Err(AppError::Conflict(_)) => continue, // slug taken (possibly by a concurrent process)
                other => return other,
            }
        }
        Err(AppError::Conflict(format!(
            "could not find a free slug for '{base}'"
        )))
    }

    pub fn update_project(&mut self, query: &str, patch: ProjectPatch) -> Result<Project> {
        let id = self.exact(query)?.id;
        // Validated before the transaction; the closure below only assigns onto the fresh row.
        let title = patch.title.map(|t| clean_title("title", &t)).transpose()?;
        let slug = patch
            .slug
            .map(|s| {
                let slug = slugify(&s);
                if slug == s.trim().to_lowercase() {
                    Ok(slug)
                } else {
                    Err(AppError::invalid(
                        "slug",
                        format!("use lowercase letters, digits and '-' (try '{slug}')"),
                    ))
                }
            })
            .transpose()?;
        let description = patch
            .description
            .map(|d| clean_text("description", &d))
            .transpose()?;
        let path = patch
            .path
            .map(|p| p.as_deref().map(link_path).transpose())
            .transpose()?;
        self.repo.modify_project(
            &id,
            &mut |p| {
                if let Some(t) = &title {
                    p.title = t.clone(); // identity and slug stay; rename slug explicitly
                }
                if let Some(s) = &slug {
                    p.slug = s.clone();
                }
                if let Some(d) = &description {
                    p.description = d.clone();
                }
                p.priority = patch.priority.unwrap_or(p.priority);
                p.stage = patch.stage.unwrap_or(p.stage);
                if let Some(path) = &path {
                    p.path = path.clone();
                }
                p.updated_at = Utc::now();
                Ok(())
            },
            &[],
        )?;
        self.fetch_mirrored(&id)
    }

    pub fn set_archived(&mut self, query: &str, archived: bool) -> Result<Project> {
        let id = self.exact(query)?.id;
        self.repo.modify_project(
            &id,
            &mut |p| {
                let now = Utc::now();
                p.archived_at = archived.then_some(now);
                p.updated_at = now;
                Ok(())
            },
            &[],
        )?;
        self.fetch_mirrored(&id)
    }

    /// `confirm_title` must equal the project's title exactly; guards against acting on a wrong target.
    pub fn delete_project(&mut self, query: &str, confirm_title: Option<&str>) -> Result<Project> {
        let p = self.exact(query)?;
        if confirm_title != Some(p.title.as_str()) {
            return Err(AppError::ConfirmationRequired(format!(
                "deleting project '{}' ({}) removes its {} task(s); repeat with confirm_title set to its exact title",
                p.title,
                p.id,
                p.tasks.len()
            )));
        }
        if !self.repo.delete_project(&p.id, &p.title)? {
            return Err(AppError::ConfirmationRequired(format!(
                "project {} was renamed or removed meanwhile; nothing deleted",
                p.id
            )));
        }
        Ok(p)
    }

    pub fn add_tasks(&mut self, query: &str, tasks: Vec<NewTask>) -> Result<Project> {
        let p = self.exact(query)?;
        let built = tasks
            .into_iter()
            .map(|t| build_task(&p.id, t))
            .collect::<Result<Vec<_>>>()?; // validate all first
        self.repo.modify_project(
            &p.id,
            &mut |p| {
                for t in &built {
                    ensure_title_free(&p.tasks, &t.title, &t.id)?;
                    p.tasks.push(t.clone());
                }
                Ok(())
            },
            &[],
        )?;
        self.fetch_mirrored(&p.id)
    }

    pub fn update_task(&mut self, project: &str, task: &str, patch: TaskPatch) -> Result<Task> {
        let p = self.exact(project)?;
        let task_id = resolve_task(&p, task)?.id.clone();
        let title = patch
            .title
            .map(|t| clean_title("task title", &t))
            .transpose()?;
        let description = match patch.description {
            Some(d) => Some(
                d.as_deref()
                    .map(|d| clean_text("task description", d))
                    .transpose()?
                    .filter(|d| !d.is_empty()),
            ),
            None => None,
        };
        let mut updated = None;
        self.repo.modify_project(
            &p.id,
            &mut |p| {
                if let Some(title) = &title {
                    ensure_title_free(&p.tasks, title, &task_id)?;
                }
                let t = task_in(p, &task_id)?;
                if let Some(title) = &title {
                    t.title = title.clone();
                }
                if let Some(d) = &description {
                    t.description = d.clone();
                }
                if let Some(prio) = patch.priority {
                    t.priority = prio;
                }
                let now = Utc::now();
                t.updated_at = now;
                if let Some(status) = patch.status {
                    t.set_status(status, now);
                }
                updated = Some(t.clone());
                Ok(())
            },
            &[],
        )?;
        self.fetch_mirrored(&p.id)?;
        Ok(updated.expect("modify_project ran the change"))
    }

    /// Batch status change: all ids are resolved first, then every task is written in one transaction.
    pub fn set_tasks_status(
        &mut self,
        project: &str,
        tasks: &[String],
        status: TaskStatus,
    ) -> Result<Project> {
        let p = self.exact(project)?;
        let ids = tasks
            .iter()
            .map(|q| resolve_task(&p, q).map(|t| t.id.clone()))
            .collect::<Result<Vec<_>>>()?;
        self.repo.modify_project(
            &p.id,
            &mut |p| {
                let now = Utc::now();
                for id in &ids {
                    task_in(p, id)?.set_status(status, now);
                }
                Ok(())
            },
            &[],
        )?;
        self.fetch_mirrored(&p.id)
    }

    pub fn remove_task(&mut self, project: &str, task: &str, confirm: bool) -> Result<Task> {
        let p = self.exact(project)?;
        let t = resolve_task(&p, task)?.clone();
        if !confirm {
            return Err(AppError::ConfirmationRequired(format!(
                "removing task '{}' ({}) is permanent; repeat with confirm=true",
                t.title, t.id
            )));
        }
        self.repo.delete_task(&t.id)?;
        self.fetch_mirrored(&p.id)?;
        Ok(t)
    }

    pub fn add_reference(
        &mut self,
        project: &str,
        task: Option<&str>,
        kind: RefKind,
        value: &str,
        url: Option<&str>,
    ) -> Result<Reference> {
        let p = self.exact(project)?;
        let task_id = task
            .map(|q| resolve_task(&p, q).map(|t| t.id.clone()))
            .transpose()?;
        let r = build_reference(&p.id, task_id, kind, value, url)?;
        self.repo.insert_reference(&r)?;
        self.fetch_mirrored(&p.id)?;
        Ok(r)
    }

    pub fn remove_reference(&mut self, project: &str, reference: &str) -> Result<()> {
        let p = self.exact(project)?;
        let id = resolve_reference(&p, reference)?.id.clone();
        self.repo.delete_reference(&id)?;
        self.fetch_mirrored(&p.id)?;
        Ok(())
    }

    /// Reads the project back after a change and mirrors it into its `.project-status` file, when it has one.
    fn fetch_mirrored(&mut self, id: &str) -> Result<Project> {
        let p = self.fetch(id)?;
        self.mirror(&p);
        Ok(p)
    }

    fn fetch(&mut self, id: &str) -> Result<Project> {
        self.repo
            .get_project(id)?
            .ok_or_else(|| AppError::NotFound {
                what: "project",
                query: id.into(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::SqliteRepository;

    fn svc() -> Service<SqliteRepository> {
        Service::new(SqliteRepository::open_in_memory().unwrap())
    }

    fn np(title: &str) -> NewProject {
        NewProject {
            title: title.into(),
            ..Default::default()
        }
    }

    fn nt(title: &str) -> NewTask {
        NewTask {
            title: title.into(),
            ..Default::default()
        }
    }

    #[test]
    fn create_applies_defaults_and_unique_slugs() {
        let mut s = svc();
        let a = s.create_project(np("My App")).unwrap();
        let b = s.create_project(np("My App")).unwrap();
        assert_eq!((a.slug.as_str(), b.slug.as_str()), ("my-app", "my-app-2"));
        assert_eq!((a.priority, a.stage), (Priority::Medium, Stage::Idea));
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn create_validates_before_writing() {
        let mut s = svc();
        assert!(matches!(
            s.create_project(np("  ")),
            Err(AppError::Invalid { field: "title", .. })
        ));
        let mut bad = np("ok");
        bad.tasks.push(nt(""));
        assert!(s.create_project(bad).is_err());
        let mut rel = np("ok");
        rel.path = Some("relative".into());
        assert!(s.create_project(rel).is_err());
        assert!(s.list(&Filter::default()).unwrap().is_empty());
    }

    #[test]
    fn rename_keeps_id_slug_and_tasks_and_slug_change_leaves_alias() {
        let mut s = svc();
        let mut n = np("Old");
        n.tasks.push(nt("t1"));
        let p = s.create_project(n).unwrap();
        let r = s
            .update_project(
                &p.id,
                ProjectPatch {
                    title: Some("New".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            (
                r.id.as_str(),
                r.slug.as_str(),
                r.title.as_str(),
                r.tasks.len()
            ),
            (p.id.as_str(), "old", "New", 1)
        );
        let r = s
            .update_project(
                "old",
                ProjectPatch {
                    slug: Some("new".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(r.aliases, vec!["old"]);
        assert_eq!(
            s.update_project("old", ProjectPatch::default()).unwrap().id,
            p.id
        ); // alias still resolves
        assert!(
            s.update_project(
                "new",
                ProjectPatch {
                    slug: Some("Not A Slug".into()),
                    ..Default::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn mutations_reject_fuzzy_names_but_reads_accept_them() {
        let mut s = svc();
        s.create_project(np("Website redesign")).unwrap();
        assert!(matches!(
            s.update_project("webs", ProjectPatch::default()),
            Err(AppError::NotFound { .. })
        ));
        assert_eq!(s.find("redesign").unwrap().slug, "website-redesign");
    }

    #[test]
    fn clearing_path_and_archiving() {
        let mut s = svc();
        let mut n = np("A");
        let a = std::env::temp_dir().join("a");
        let a = a.to_str().unwrap();
        n.path = Some(format!("{a}/"));
        let p = s.create_project(n).unwrap();
        assert_eq!(p.path.as_deref(), Some(a));
        let p = s
            .update_project(
                "a",
                ProjectPatch {
                    path: Some(None),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(p.path, None);
        assert!(s.set_archived("a", true).unwrap().archived_at.is_some());
        assert!(s.list(&Filter::default()).unwrap().is_empty());
        assert!(s.set_archived("a", false).unwrap().archived_at.is_none());
    }

    #[test]
    fn delete_requires_exact_title() {
        let mut s = svc();
        s.create_project(np("Keep Me")).unwrap();
        assert!(matches!(
            s.delete_project("keep-me", None),
            Err(AppError::ConfirmationRequired(_))
        ));
        assert!(matches!(
            s.delete_project("keep-me", Some("keep me")),
            Err(AppError::ConfirmationRequired(_))
        ));
        assert_eq!(s.list(&Filter::default()).unwrap().len(), 1);
        s.delete_project("keep-me", Some("Keep Me")).unwrap();
        assert!(s.list(&Filter::default()).unwrap().is_empty());
    }

    #[test]
    fn task_lifecycle_drives_progress() {
        let mut s = svc();
        s.create_project(np("P")).unwrap();
        let p = s.add_tasks("p", vec![nt("a"), nt("b")]).unwrap();
        assert_eq!(p.progress().percent(), 0);
        let ids: Vec<String> = p.tasks.iter().map(|t| t.id.clone()).collect();
        let p = s
            .set_tasks_status("p", &ids[..1], TaskStatus::Done)
            .unwrap();
        assert_eq!(p.progress().percent(), 50);
        assert!(p.tasks[0].completed_at.is_some());
        let t = s
            .update_task(
                "p",
                &ids[0],
                TaskPatch {
                    status: Some(TaskStatus::Blocked),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!((t.status, t.completed_at), (TaskStatus::Blocked, None));
        assert_eq!(s.find(&p.id).unwrap().progress().percent(), 0);
    }

    #[test]
    fn batch_status_is_all_or_nothing() {
        let mut s = svc();
        s.create_project(np("P")).unwrap();
        let p = s.add_tasks("p", vec![nt("a")]).unwrap();
        let err = s.set_tasks_status(
            "p",
            &[p.tasks[0].id.clone(), "doesnotexist".into()],
            TaskStatus::Done,
        );
        assert!(err.is_err());
        assert_eq!(s.find("p").unwrap().progress().done, 0);
    }

    #[test]
    fn remove_task_needs_confirm() {
        let mut s = svc();
        s.create_project(np("P")).unwrap();
        let p = s.add_tasks("p", vec![nt("a")]).unwrap();
        let id = p.tasks[0].id.clone();
        assert!(matches!(
            s.remove_task("p", &id, false),
            Err(AppError::ConfirmationRequired(_))
        ));
        s.remove_task("p", &id, true).unwrap();
        assert!(s.find("p").unwrap().tasks.is_empty());
    }

    #[test]
    fn task_must_belong_to_the_project() {
        let mut s = svc();
        s.create_project(np("A")).unwrap();
        s.create_project(np("B")).unwrap();
        let a = s.add_tasks("a", vec![nt("x")]).unwrap();
        let err = s.update_task("b", &a.tasks[0].id, TaskPatch::default());
        assert!(matches!(err, Err(AppError::NotFound { what: "task", .. })));
    }

    #[test]
    fn references_validate_url_and_task() {
        let mut s = svc();
        s.create_project(np("P")).unwrap();
        let p = s.add_tasks("p", vec![nt("a")]).unwrap();
        let r = s
            .add_reference(
                "p",
                Some(&p.tasks[0].id),
                RefKind::Pr,
                "#7",
                Some("https://example.com/7"),
            )
            .unwrap();
        assert_eq!(r.task_id.as_deref(), Some(p.tasks[0].id.as_str()));
        assert!(
            s.add_reference("p", None, RefKind::Issue, "#8", Some("javascript:alert(1)"))
                .is_err()
        );
        assert!(
            s.add_reference("p", Some("nope1234"), RefKind::Commit, "abc", None)
                .is_err()
        );
        for q in ["", "7", &r.id[r.id.len() - 3..]] {
            assert!(s.remove_reference("p", q).is_err(), "{q:?} must not match");
        }
        s.remove_reference("p", crate::domain::model::short_id(&r.id))
            .unwrap();
        assert!(s.find("p").unwrap().references.is_empty());
    }
}
