use std::{collections::BTreeMap, path::Path};

use serde::Serialize;

use crate::domain::{Priority, Project, Stage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Priority,
    Progress,
    Updated,
    Title,
}

#[cfg(feature = "gui")]
impl SortKey {
    pub const ALL: [SortKey; 4] = [Self::Priority, Self::Progress, Self::Updated, Self::Title];
}

impl std::fmt::Display for SortKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Priority => "priority",
            Self::Progress => "progress",
            Self::Updated => "updated",
            Self::Title => "title",
        })
    }
}

impl std::str::FromStr for SortKey {
    type Err = crate::domain::AppError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "priority" => Ok(Self::Priority),
            "progress" => Ok(Self::Progress),
            "updated" => Ok(Self::Updated),
            "title" => Ok(Self::Title),
            o => Err(crate::domain::AppError::invalid(
                "sort",
                format!("'{o}' (expected priority, progress, updated, title)"),
            )),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub priorities: Vec<Priority>,
    pub stages: Vec<Stage>,
    pub min_progress: Option<u8>,
    pub max_progress: Option<u8>,
    /// `Some(true)` = only finished projects (all tasks done, at least one task), `Some(false)` = only unfinished.
    pub done: Option<bool>,
    pub include_archived: bool,
    /// Case-insensitive substring over title, slug, description and task titles. Blank = no search.
    pub search: String,
    pub sort: SortKey,
    pub reverse: bool,
}

impl Filter {
    /// True when search or a filter hides projects (the done/open split and sorting do not count).
    pub fn narrows(&self) -> bool {
        !self.search.trim().is_empty()
            || !self.priorities.is_empty()
            || !self.stages.is_empty()
            || self.min_progress.is_some()
            || self.max_progress.is_some()
    }
}

pub fn is_done(p: &Project) -> bool {
    let pr = p.progress();
    pr.total > 0 && pr.done == pr.total
}

fn matches_search(p: &Project, needle: &str) -> bool {
    let hit = |s: &str| s.to_lowercase().contains(needle);
    needle.is_empty()
        || hit(&p.title)
        || hit(&p.slug)
        || hit(&p.description)
        || p.tasks.iter().any(|t| hit(&t.title))
}

pub fn apply(mut projects: Vec<Project>, f: &Filter) -> Vec<Project> {
    let needle = f.search.trim().to_lowercase();
    projects.retain(|p| {
        let pct = p.progress().percent();
        matches_search(p, &needle)
            && (f.priorities.is_empty() || f.priorities.contains(&p.priority))
            && (f.stages.is_empty() || f.stages.contains(&p.stage))
            && f.min_progress.is_none_or(|m| pct >= m)
            && f.max_progress.is_none_or(|m| pct <= m)
            && f.done.is_none_or(|d| is_done(p) == d)
            && (f.include_archived || p.archived_at.is_none())
    });
    match f.sort {
        // most important first; ties by title for a stable order
        SortKey::Priority => projects.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.title.cmp(&b.title))
        }),
        SortKey::Progress => projects.sort_by(|a, b| {
            b.progress()
                .percent()
                .cmp(&a.progress().percent())
                .then_with(|| a.title.cmp(&b.title))
        }),
        SortKey::Updated => projects.sort_by_key(|p| std::cmp::Reverse(p.updated_at)),
        SortKey::Title => projects.sort_by_cached_key(|p| p.title.to_lowercase()),
    }
    if f.reverse {
        projects.reverse();
    }
    projects
}

/// The non-archived project whose linked path is the deepest ancestor of (or equal to) `dir`.
pub fn project_for_path<'a>(projects: &'a [Project], dir: &Path) -> Option<&'a Project> {
    projects
        .iter()
        .filter(|p| p.archived_at.is_none())
        .filter_map(|p| p.path.as_deref().map(|path| (p, Path::new(path))))
        .filter(|(_, path)| dir.starts_with(path))
        .max_by_key(|(_, path)| path.components().count())
        .map(|(p, _)| p)
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Summary {
    pub projects: usize,
    pub tasks_done: usize,
    pub tasks_total: usize,
    pub average_progress: u8,
    pub by_stage: BTreeMap<String, usize>,
    pub by_priority: BTreeMap<String, usize>,
}

pub fn summarize(projects: &[Project]) -> Summary {
    let mut s = Summary {
        projects: projects.len(),
        tasks_done: 0,
        tasks_total: 0,
        average_progress: 0,
        by_stage: BTreeMap::new(),
        by_priority: BTreeMap::new(),
    };
    let mut pct_sum = 0usize;
    for p in projects {
        let pr = p.progress();
        s.tasks_done += pr.done;
        s.tasks_total += pr.total;
        pct_sum += pr.percent() as usize;
        *s.by_stage.entry(p.stage.to_string()).or_default() += 1;
        *s.by_priority.entry(p.priority.to_string()).or_default() += 1;
    }
    if !projects.is_empty() {
        s.average_progress = (pct_sum as f64 / projects.len() as f64).round() as u8;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TaskStatus;
    use crate::domain::model::fixtures::{project, task};

    fn with_progress(id: &str, prio: Priority, done: usize, total: usize) -> Project {
        let mut p = project(id, id, id);
        p.priority = prio;
        for i in 0..total {
            p.tasks.push(task(
                &format!("{id}{i}"),
                if i < done {
                    TaskStatus::Done
                } else {
                    TaskStatus::Todo
                },
            ));
        }
        p
    }

    fn ids(ps: &[Project]) -> Vec<&str> {
        ps.iter().map(|p| p.id.as_str()).collect()
    }

    fn sample() -> Vec<Project> {
        vec![
            with_progress("a", Priority::Low, 1, 4),
            with_progress("b", Priority::Critical, 4, 4),
            with_progress("c", Priority::High, 2, 4),
            with_progress("d", Priority::High, 0, 0),
        ]
    }

    #[test]
    fn default_sort_is_priority_then_title() {
        assert_eq!(
            ids(&apply(sample(), &Filter::default())),
            ["b", "c", "d", "a"]
        );
    }

    #[test]
    fn filters_combine() {
        let f = Filter {
            priorities: vec![Priority::High, Priority::Critical],
            min_progress: Some(50),
            ..Default::default()
        };
        assert_eq!(ids(&apply(sample(), &f)), ["b", "c"]);
        let f = Filter {
            max_progress: Some(25),
            sort: SortKey::Title,
            ..Default::default()
        };
        assert_eq!(ids(&apply(sample(), &f)), ["a", "d"]);
    }

    #[test]
    fn search_matches_title_description_slug_and_task_titles_ignoring_case() {
        let mut ps = sample();
        ps[0].title = "Billing Portal".into();
        ps[1].description = "uses the BILLING api".into();
        ps[2].tasks[0].title = "fix billing bug".into();
        ps[3].slug = "billing-cli".into();
        let f = |q: &str| Filter {
            search: q.into(),
            sort: SortKey::Title,
            ..Default::default()
        };
        assert_eq!(apply(sample(), &f("billing")).len(), 0);
        assert_eq!(
            ids(&apply(ps.clone(), &f("  BILLING "))),
            ["b", "a", "c", "d"]
        );
        assert_eq!(ids(&apply(ps.clone(), &f("portal"))), ["a"]);
        assert_eq!(apply(ps, &f("nothing like this")).len(), 0);
    }

    #[test]
    fn done_filter_ignores_empty_projects() {
        let f = Filter {
            done: Some(true),
            ..Default::default()
        };
        assert_eq!(ids(&apply(sample(), &f)), ["b"]);
        let f = Filter {
            done: Some(false),
            sort: SortKey::Title,
            ..Default::default()
        };
        assert_eq!(ids(&apply(sample(), &f)), ["a", "c", "d"]);
    }

    #[test]
    fn archived_hidden_and_reverse() {
        let mut ps = sample();
        ps[0].archived_at = Some(chrono::Utc::now());
        assert_eq!(apply(ps.clone(), &Filter::default()).len(), 3);
        let f = Filter {
            include_archived: true,
            reverse: true,
            ..Default::default()
        };
        assert_eq!(ids(&apply(ps, &f)), ["a", "d", "c", "b"]);
    }

    #[test]
    fn path_lookup_picks_deepest_ancestor() {
        let mut outer = project("o", "o", "o");
        outer.path = Some("/work".into());
        let mut inner = project("i", "i", "i");
        inner.path = Some("/work/app".into());
        let mut sibling = project("s", "s", "s");
        sibling.path = Some("/work/app2".into());
        let ps = vec![outer, inner, sibling];
        assert_eq!(
            project_for_path(&ps, Path::new("/work/app/src"))
                .unwrap()
                .id,
            "i"
        );
        assert_eq!(
            project_for_path(&ps, Path::new("/work/other")).unwrap().id,
            "o"
        );
        assert!(project_for_path(&ps, Path::new("/elsewhere")).is_none());
    }

    #[test]
    fn summary_counts() {
        let s = summarize(&sample());
        assert_eq!((s.projects, s.tasks_done, s.tasks_total), (4, 7, 12));
        assert_eq!(s.average_progress, 44); // (25+100+50+0)/4 = 43.75
        assert_eq!(s.by_priority["high"], 2);
    }
}
