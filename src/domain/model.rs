use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::enums::{Priority, RefKind, Stage, TaskStatus};
use super::error::{AppError, Result};

pub type Timestamp = DateTime<Utc>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub priority: Priority,
    pub stage: Stage,
    pub path: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub archived_at: Option<Timestamp>,
    pub aliases: Vec<String>,
    pub tasks: Vec<Task>,
    pub references: Vec<Reference>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: Option<Priority>,
    pub position: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub completed_at: Option<Timestamp>,
}

/// Link to a commit, PR or issue. Stored only; nothing is synchronised with GitHub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    pub id: String,
    pub project_id: String,
    pub task_id: Option<String>,
    pub kind: RefKind,
    pub value: String,
    pub url: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
}

impl Progress {
    /// Whole percent, rounded; a project without tasks is 0%.
    pub fn percent(self) -> u8 {
        ((self.done * 100 + self.total / 2).checked_div(self.total)).unwrap_or(0) as u8
    }
}

impl Project {
    pub fn progress(&self) -> Progress {
        Progress {
            done: self
                .tasks
                .iter()
                .filter(|t| t.status == TaskStatus::Done)
                .count(),
            total: self.tasks.len(),
        }
    }
}

impl Task {
    /// Applies a status change, keeping `completed_at` consistent with it.
    pub fn set_status(&mut self, status: TaskStatus, now: Timestamp) {
        self.completed_at = (status == TaskStatus::Done).then(|| self.completed_at.unwrap_or(now));
        self.status = status;
        self.updated_at = now;
    }
}

/// Short, copy-friendly handle: the random tail of a ULID (the head is a timestamp and collides).
pub fn short_id(id: &str) -> &str {
    &id[id.len().saturating_sub(6)..]
}

pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    let slug: String = slug.chars().take(48).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "project".into()
    } else {
        slug.into()
    }
}

pub fn clean_title(field: &'static str, raw: &str) -> Result<String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(AppError::invalid(field, "must not be empty"));
    }
    if s.chars().count() > 200 {
        return Err(AppError::invalid(field, "must be at most 200 characters"));
    }
    if s.chars().any(char::is_control) {
        return Err(AppError::invalid(
            field,
            "must not contain control characters",
        ));
    }
    Ok(s.to_string())
}

/// Multi-line text: newlines and tabs are kept (CRLF becomes LF), other control characters are rejected
/// because descriptions are printed raw to terminals.
pub fn clean_text(field: &'static str, raw: &str) -> Result<String> {
    let s = raw.trim().replace("\r\n", "\n");
    if s.chars().count() > 10_000 {
        return Err(AppError::invalid(field, "must be at most 10000 characters"));
    }
    if s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(AppError::invalid(
            field,
            "must not contain control characters other than newlines and tabs",
        ));
    }
    Ok(s)
}

pub fn clean_path(raw: &str) -> Result<String> {
    let p = std::path::Path::new(raw);
    if !p.is_absolute() {
        return Err(AppError::invalid("path", "must be an absolute path"));
    }
    if p.parent().is_none() {
        return Err(AppError::invalid("path", "must not be the filesystem root"));
    }
    Ok(raw.trim_end_matches(['/', '\\']).to_string())
}

/// Task titles are unique within a project, because `.project-status` matches tasks by title. Only the task being
/// added or renamed is checked, so a project that already holds duplicates can still be edited.
pub fn ensure_title_free(tasks: &[Task], title: &str, task_id: &str) -> Result<()> {
    if tasks.iter().any(|t| t.id != task_id && t.title == title) {
        return Err(AppError::invalid(
            "task title",
            format!("the project already has a task called '{title}'; titles identify tasks"),
        ));
    }
    Ok(())
}

/// An optional reference URL: blank means none, anything else must be http(s).
pub fn clean_url(raw: Option<&str>) -> Result<Option<String>> {
    let url = raw.map(str::trim).filter(|u| !u.is_empty());
    if let Some(u) = url
        && !(u.starts_with("https://") || u.starts_with("http://"))
    {
        return Err(AppError::invalid(
            "url",
            "must start with http:// or https://",
        ));
    }
    Ok(url.map(String::from))
}

fn unique<'a, T>(
    query: &str,
    what: &'static str,
    mut hits: Vec<(&'a T, String)>,
) -> Result<Option<&'a T>> {
    hits.dedup_by(|a, b| a.1 == b.1);
    match hits.len() {
        0 => Ok(None),
        1 => Ok(Some(hits.remove(0).0)),
        _ => {
            let _ = what;
            Err(AppError::Ambiguous {
                query: query.into(),
                candidates: hits.into_iter().map(|h| h.1).collect(),
            })
        }
    }
}

/// Finds a project. `fuzzy = false` (mutations) accepts only exact id, slug or alias.
/// `fuzzy = true` (read-only) additionally accepts an id tail/prefix, slug prefix or title substring,
/// and fails with `Ambiguous` unless exactly one project matches.
pub fn resolve_project<'a>(
    projects: &'a [Project],
    query: &str,
    fuzzy: bool,
) -> Result<&'a Project> {
    let q = query.trim();
    let ql = q.to_lowercase();
    let exact =
        |p: &Project| p.id.eq_ignore_ascii_case(q) || p.slug == ql || p.aliases.contains(&ql);
    if let Some(p) = projects.iter().find(|p| exact(p)) {
        return Ok(p);
    }
    if fuzzy && q.len() >= 3 {
        let hits = projects
            .iter()
            .filter(|p| {
                (q.len() >= 4
                    && (p.id.to_lowercase().ends_with(&ql) || p.id.to_lowercase().starts_with(&ql)))
                    || p.slug.starts_with(&ql)
                    || p.title.to_lowercase().contains(&ql)
            })
            .map(|p| (p, p.id.clone()))
            .collect();
        if let Some(p) = unique(q, "project", hits)? {
            return Ok(p);
        }
    }
    Err(AppError::NotFound {
        what: "project",
        query: q.into(),
    })
}

#[cfg(feature = "mcp")]
/// Every project a human-typed `query` could mean: exact id/slug/alias matches if any,
/// otherwise all fuzzy matches (id tail/prefix, slug prefix, title substring).
pub fn search_projects<'a>(projects: &'a [Project], query: &str) -> Vec<&'a Project> {
    let q = query.trim();
    let ql = q.to_lowercase();
    if let Some(p) = projects
        .iter()
        .find(|p| p.id.eq_ignore_ascii_case(q) || p.slug == ql || p.aliases.contains(&ql))
    {
        return vec![p];
    }
    if q.len() < 3 {
        return vec![];
    }
    projects
        .iter()
        .filter(|p| {
            (q.len() >= 4
                && (p.id.to_lowercase().ends_with(&ql) || p.id.to_lowercase().starts_with(&ql)))
                || p.slug.starts_with(&ql)
                || p.title.to_lowercase().contains(&ql)
        })
        .collect()
}

/// Finds a task inside one project by exact id, or by a unique id tail/prefix of 4+ characters.
pub fn resolve_task<'a>(project: &'a Project, query: &str) -> Result<&'a Task> {
    resolve_by_id(&project.tasks, |t| &t.id, "task", query)
}

/// Finds a reference inside one project, with the same rules as `resolve_task`.
pub fn resolve_reference<'a>(project: &'a Project, query: &str) -> Result<&'a Reference> {
    resolve_by_id(&project.references, |r| &r.id, "reference", query)
}

fn resolve_by_id<'a, T>(
    items: &'a [T],
    id: impl Fn(&T) -> &str,
    what: &'static str,
    query: &str,
) -> Result<&'a T> {
    let q = query.trim().to_lowercase();
    if let Some(t) = items.iter().find(|t| id(t).to_lowercase() == q) {
        return Ok(t);
    }
    if q.len() >= 4 {
        let hits = items
            .iter()
            .filter(|t| {
                let i = id(t).to_lowercase();
                i.ends_with(&q) || i.starts_with(&q)
            })
            .map(|t| (t, id(t).to_string()))
            .collect();
        if let Some(t) = unique(&q, what, hits)? {
            return Ok(t);
        }
    }
    Err(AppError::NotFound {
        what,
        query: query.into(),
    })
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn project(id: &str, slug: &str, title: &str) -> Project {
        let now = Utc::now();
        Project {
            id: id.into(),
            slug: slug.into(),
            title: title.into(),
            description: String::new(),
            priority: Priority::Medium,
            stage: Stage::Idea,
            path: None,
            created_at: now,
            updated_at: now,
            archived_at: None,
            aliases: vec![],
            tasks: vec![],
            references: vec![],
        }
    }

    pub fn task(id: &str, status: TaskStatus) -> Task {
        let now = Utc::now();
        Task {
            id: id.into(),
            project_id: "p".into(),
            title: id.into(),
            description: None,
            status,
            priority: None,
            position: 0,
            created_at: now,
            updated_at: now,
            completed_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn progress_is_derived_from_done_tasks() {
        let mut p = project("A", "a", "A");
        assert_eq!(p.progress().percent(), 0);
        p.tasks = vec![
            task("1", TaskStatus::Done),
            task("2", TaskStatus::Blocked),
            task("3", TaskStatus::Done),
        ];
        assert_eq!(p.progress(), Progress { done: 2, total: 3 });
        assert_eq!(p.progress().percent(), 67);
    }

    #[test]
    fn set_status_tracks_completed_at() {
        let mut t = task("1", TaskStatus::Todo);
        let now = Utc::now();
        t.set_status(TaskStatus::Done, now);
        assert_eq!(t.completed_at, Some(now));
        t.set_status(TaskStatus::InProgress, now);
        assert_eq!(t.completed_at, None);
    }

    #[test]
    fn slugify_cases() {
        assert_eq!(slugify("  My Cool_Project!! "), "my-cool-project");
        assert_eq!(slugify("日本語"), "project");
        assert_eq!(slugify(&"a".repeat(100)).len(), 48);
    }

    #[test]
    fn validation() {
        assert!(clean_title("title", "  ").is_err());
        assert!(clean_title("title", "a\u{1b}[31m").is_err());
        assert_eq!(clean_title("title", " ok ").unwrap(), "ok");
        assert!(clean_path("relative/dir").is_err());
        assert!(clean_path("/").is_err());
        assert_eq!(clean_path("/a/").unwrap(), "/a");
        assert_eq!(clean_text("d", "a\r\n\tb").unwrap(), "a\n\tb");
        assert!(clean_text("d", "a\u{1b}]0;x\u{7}").is_err());
        assert!(clean_url(Some("javascript:alert(1)")).is_err());
        assert_eq!(clean_url(Some("  ")).unwrap(), None);
    }

    #[test]
    fn mutation_lookup_is_exact_only() {
        let ps = vec![
            project("01AAAA", "alpha", "Alpha"),
            project("01BBBB", "alphabet", "Alphabet"),
        ];
        assert_eq!(resolve_project(&ps, "alpha", false).unwrap().id, "01AAAA");
        assert!(matches!(
            resolve_project(&ps, "alp", false),
            Err(AppError::NotFound { .. })
        ));
    }

    #[test]
    fn read_lookup_fuzzy_unique_or_ambiguous() {
        let mut ps = vec![
            project("01AAAA", "alpha", "Alpha"),
            project("01BBBB", "alphabet", "Alphabet"),
        ];
        ps[1].aliases.push("old-name".into());
        assert!(matches!(
            resolve_project(&ps, "alp", true),
            Err(AppError::Ambiguous { .. })
        ));
        assert_eq!(resolve_project(&ps, "alphab", true).unwrap().id, "01BBBB");
        assert_eq!(
            resolve_project(&ps, "OLD-NAME", false).unwrap().id,
            "01BBBB"
        );
    }

    #[cfg(feature = "mcp")]
    #[test]
    fn search_returns_exact_hit_alone_or_all_fuzzy_hits() {
        let ps = vec![
            project("01AAAA", "alpha", "Alpha"),
            project("01BBBB", "alphabet", "Alphabet"),
        ];
        assert_eq!(search_projects(&ps, "alpha").len(), 1);
        assert_eq!(search_projects(&ps, "alp").len(), 2);
        assert!(search_projects(&ps, "zz").is_empty());
    }

    #[test]
    fn task_lookup_by_tail() {
        let mut p = project("P", "p", "P");
        p.tasks = vec![
            task("01AAAAXYZ123", TaskStatus::Todo),
            task("01AAAAXYZ456", TaskStatus::Todo),
        ];
        assert_eq!(resolve_task(&p, "z456").unwrap().id, "01AAAAXYZ456");
        assert!(matches!(
            resolve_task(&p, "01AAAA"),
            Err(AppError::Ambiguous { .. })
        ));
        assert!(matches!(
            resolve_task(&p, "xx"),
            Err(AppError::NotFound { .. })
        ));
    }
}
