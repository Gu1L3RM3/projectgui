use std::collections::HashSet;
use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::Service;
use super::link_path;
use crate::domain::model::{clean_text, clean_title, clean_url, slugify};
use crate::domain::{AppError, Project, Result};
use crate::repo::{Repository, Write};

const FORMAT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Export {
    format_version: u32,
    exported_at: String,
    projects: Vec<Project>,
}

#[derive(Debug, PartialEq)]
pub struct ImportReport {
    pub imported: usize,
    pub skipped_existing: usize,
}

impl<R: Repository> Service<R> {
    pub fn backup(&mut self, dest: &Path) -> Result<()> {
        self.repo.backup_to(dest)
    }

    pub fn export_json(&mut self) -> Result<String> {
        let doc = Export {
            format_version: FORMAT_VERSION,
            exported_at: Utc::now().to_rfc3339(),
            projects: self.repo.list_projects(true)?,
        };
        serde_json::to_string_pretty(&doc).map_err(|e| AppError::Storage(e.to_string()))
    }

    /// Merges by id: projects already present are left untouched; slug clashes get a numeric suffix.
    pub fn import_json(&mut self, json: &str) -> Result<ImportReport> {
        let doc: Export =
            serde_json::from_str(json).map_err(|e| AppError::invalid("import", e.to_string()))?;
        if doc.format_version != FORMAT_VERSION {
            return Err(AppError::invalid(
                "import",
                format!("unsupported format_version {}", doc.format_version),
            ));
        }
        let existing = self.repo.list_projects(true)?;
        let mut taken: HashSet<String> = existing
            .iter()
            .flat_map(|p| std::iter::once(p.slug.clone()).chain(p.aliases.iter().cloned()))
            .collect();
        let mut report = ImportReport {
            imported: 0,
            skipped_existing: 0,
        };
        let mut fresh = Vec::new();
        for mut p in doc.projects {
            if existing.iter().any(|e| e.id == p.id) {
                report.skipped_existing += 1;
                continue;
            }
            clean_imported(&mut p)?;
            let base = slugify(if p.slug.is_empty() { &p.title } else { &p.slug });
            p.slug = (1..=100)
                .map(|n| match n {
                    1 => base.clone(),
                    _ => format!("{base}-{n}"),
                })
                .find(|s| !taken.contains(s))
                .ok_or_else(|| AppError::Conflict(format!("no free slug for '{base}'")))?;
            taken.insert(p.slug.clone());
            fresh.push(p);
        }
        // One transaction: a failure (or a slug taken meanwhile by another process) imports nothing.
        let writes: Vec<Write> = fresh.iter().map(Write::InsertProject).collect();
        self.repo.write_batch(&writes)?;
        report.imported = fresh.len();
        Ok(report)
    }
}

/// Applies the same validation as the service's create paths to a project read from an import file.
fn clean_imported(p: &mut Project) -> Result<()> {
    p.title = clean_title("title", &p.title)?;
    p.description = clean_text("description", &p.description)?;
    p.path = p.path.as_deref().map(link_path).transpose()?;
    p.aliases.clear(); // aliases are local history, not portable identity
    let task_ids: Vec<String> = p.tasks.iter().map(|t| t.id.clone()).collect();
    for t in &mut p.tasks {
        t.project_id = p.id.clone();
        t.title = clean_title("task title", &t.title)?;
        t.description = t
            .description
            .as_deref()
            .map(|d| clean_text("task description", d))
            .transpose()?
            .filter(|d| !d.is_empty());
    }
    for r in &mut p.references {
        r.project_id = p.id.clone();
        r.value = clean_title("reference", &r.value)?;
        r.url = clean_url(r.url.as_deref())?;
        if let Some(tid) = &r.task_id
            && !task_ids.contains(tid)
        {
            return Err(AppError::invalid(
                "reference",
                format!("task {tid} is not part of project '{}'", p.title),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::SqliteRepository;
    use crate::service::{NewProject, NewTask};

    fn svc() -> Service<SqliteRepository> {
        Service::new(SqliteRepository::open_in_memory().unwrap())
    }

    fn seed(s: &mut Service<SqliteRepository>) {
        s.create_project(NewProject {
            title: "Alpha".into(),
            tasks: vec![NewTask {
                title: "t".into(),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
    }

    #[test]
    fn export_import_round_trip_preserves_identity() {
        let mut a = svc();
        seed(&mut a);
        let json = a.export_json().unwrap();
        let mut b = svc();
        assert_eq!(
            b.import_json(&json).unwrap(),
            ImportReport {
                imported: 1,
                skipped_existing: 0
            }
        );
        let (pa, pb) = (a.find("alpha").unwrap(), b.find("alpha").unwrap());
        assert_eq!(
            (pa.id, pa.tasks[0].id.clone()),
            (pb.id, pb.tasks[0].id.clone())
        );
    }

    #[test]
    fn import_is_idempotent_and_resolves_slug_clashes() {
        let mut a = svc();
        seed(&mut a);
        let json = a.export_json().unwrap();
        assert_eq!(a.import_json(&json).unwrap().skipped_existing, 1);

        let mut b = svc();
        seed(&mut b); // same slug, different id
        assert_eq!(b.import_json(&json).unwrap().imported, 1);
        assert_eq!(b.find("alpha-2").unwrap().title, "Alpha");
    }

    #[test]
    fn import_validates_every_field_and_is_all_or_nothing() {
        let mut a = svc();
        seed(&mut a);
        a.create_project(NewProject {
            title: "Beta".into(),
            ..Default::default()
        })
        .unwrap();
        let good: serde_json::Value = serde_json::from_str(&a.export_json().unwrap()).unwrap();
        // Export order is not fixed for projects created in the same millisecond; pick by title.
        let at = |title: &str| {
            good["projects"]
                .as_array()
                .unwrap()
                .iter()
                .position(|p| p["title"] == title)
                .unwrap()
        };
        let (alpha, beta) = (at("Alpha"), at("Beta"));
        let bad = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut v = good.clone();
            edit(&mut v["projects"][beta]);
            v.to_string()
        };
        let mut b = svc();
        for json in [
            bad(&|p| p["path"] = "relative/dir".into()),
            bad(&|p| p["description"] = "\u{1b}[2J".into()),
            bad(&|p| p["tasks"] = good["projects"][alpha]["tasks"].clone()), // id clash with Alpha's task
        ] {
            assert!(b.import_json(&json).is_err(), "{json}");
            assert!(b.list(&Default::default()).unwrap().is_empty());
        }
        let mixed_case = bad(&|p| p["slug"] = "My Slug".into());
        assert_eq!(b.import_json(&mixed_case).unwrap().imported, 2);
        assert_eq!(b.find_exact("my-slug").unwrap().title, "Beta");
    }

    #[test]
    fn import_rejects_garbage_and_unknown_versions() {
        let mut s = svc();
        assert!(s.import_json("not json").is_err());
        assert!(
            s.import_json(r#"{"format_version":99,"exported_at":"x","projects":[]}"#)
                .is_err()
        );
    }
}
