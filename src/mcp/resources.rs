use serde_json::{Value, json};

use crate::cli::color::Style;
use crate::cli::render::{project_json, status_overview};
use crate::domain::{AppError, Result};
use crate::repo::Repository;
use crate::service::{Filter, Service};

pub const PROJECTS: &str = "projectgui://projects";
pub const SUMMARY: &str = "projectgui://summary";
pub const PROJECT_FILE_FORMAT: &str = "projectgui://project-file-format";
pub const PROJECT_PREFIX: &str = "projectgui://projects/";
pub const PROJECT_TEMPLATE: &str = "projectgui://projects/{id}";

/// (uri, name, description, mime)
pub const STATIC: &[(&str, &str, &str, &str)] = &[
    (
        PROJECTS,
        "projects",
        "All active projects with progress, tasks and references (JSON)",
        "application/json",
    ),
    (
        SUMMARY,
        "summary",
        "Plain-text status overview of every project, same as `projectgui status`",
        "text/plain",
    ),
    (
        PROJECT_FILE_FORMAT,
        "project-file-format",
        "How to write a .project-status file and how syncing it behaves (Markdown)",
        "text/markdown",
    ),
];

pub struct Content {
    pub mime: &'static str,
    pub text: String,
}

pub fn read<R: Repository>(svc: &mut Service<R>, uri: &str) -> Result<Content> {
    if uri == PROJECTS {
        let projects = svc.list(&Filter::default())?;
        let body: Vec<Value> = projects.iter().map(project_json).collect();
        return Ok(Content {
            mime: "application/json",
            text: json!({ "projects": body }).to_string(),
        });
    }
    if uri == PROJECT_FILE_FORMAT {
        return Ok(Content {
            mime: "text/markdown",
            text: include_str!("../../docs/project-file.md").to_string(),
        });
    }
    if uri == SUMMARY {
        let projects = svc.list(&Filter::default())?;
        let summary = crate::service::query::summarize(&projects);
        return Ok(Content {
            mime: "text/plain",
            text: status_overview(
                &projects,
                &summary,
                &Style::plain(),
                crate::cli::render::now(),
            ),
        });
    }
    if let Some(id) = uri.strip_prefix(PROJECT_PREFIX) {
        let p = svc.find_exact(id)?;
        return Ok(Content {
            mime: "application/json",
            text: project_json(&p).to_string(),
        });
    }
    Err(AppError::NotFound {
        what: "resource",
        query: uri.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::SqliteRepository;
    use crate::service::NewProject;

    #[test]
    fn reads_every_resource_kind() {
        let mut s = Service::new(SqliteRepository::open_in_memory().unwrap());
        let p = s
            .create_project(NewProject {
                title: "Res".into(),
                ..Default::default()
            })
            .unwrap();
        let all: Value = serde_json::from_str(&read(&mut s, PROJECTS).unwrap().text).unwrap();
        assert_eq!(all["projects"][0]["id"], p.id.as_str());
        let one = read(&mut s, &format!("{PROJECT_PREFIX}{}", p.id)).unwrap();
        assert_eq!(one.mime, "application/json");
        let text = read(&mut s, SUMMARY).unwrap();
        assert!(text.text.contains("Res") && !text.text.contains('\x1b'));
        let format = read(&mut s, PROJECT_FILE_FORMAT).unwrap();
        assert!(format.mime == "text/markdown" && format.text.contains("[[task]]"));
        assert_eq!(
            read(&mut s, "projectgui://nope").err().unwrap().code(),
            "not_found"
        );
        assert!(read(&mut s, &format!("{PROJECT_PREFIX}missing")).is_err());
    }
}
