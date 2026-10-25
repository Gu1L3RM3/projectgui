//! Tool catalogue and dispatch. Pure `serde_json` in and out so it is testable without a transport.
use std::path::Path;

use serde_json::{Map, Value, json};

use super::args::Args;
use crate::cli::render::project_json;
use crate::domain::{AppError, Project, Result, TaskStatus};
use crate::repo::Repository;
use crate::service::projectfile::{DEFAULT_DEPTH, MAX_DEPTH};
use crate::service::{Filter, NewProject, NewTask, ProjectPatch, Service, TaskPatch};

pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
    pub read_only: bool,
    pub destructive: bool,
}

const PRIORITIES: [&str; 4] = ["low", "medium", "high", "critical"];
const STAGES: [&str; 6] = [
    "idea",
    "planning",
    "in-progress",
    "review",
    "done",
    "paused",
];
const STATUSES: [&str; 4] = ["todo", "in-progress", "blocked", "done"];

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
}

fn id_prop(what: &str) -> Value {
    json!({ "type": "string", "description": format!("Full 26-character id of the {what}. Never a name or slug.") })
}

fn task_item() -> Value {
    obj(
        json!({
            "title": { "type": "string" },
            "description": { "type": "string" },
            "priority": { "type": "string", "enum": PRIORITIES },
            "status": { "type": "string", "enum": STATUSES }
        }),
        &["title"],
    )
}

fn abs_dir(a: &Args) -> Result<std::path::PathBuf> {
    let path = a.str("path")?;
    if !Path::new(&path).is_absolute() {
        return Err(AppError::invalid("path", "must be an absolute path"));
    }
    Ok(path.into())
}

pub fn definitions() -> Vec<ToolDef> {
    let read = |name, description, schema| ToolDef {
        name,
        description,
        schema,
        read_only: true,
        destructive: false,
    };
    let write = |name, description, schema| ToolDef {
        name,
        description,
        schema,
        read_only: false,
        destructive: false,
    };
    vec![
        read(
            "list_projects",
            "List projects with progress (derived from completed tasks). Optional filters; sorted by priority by default.",
            obj(
                json!({
                    "priority": { "type": "array", "items": { "type": "string", "enum": PRIORITIES } },
                    "stage": { "type": "array", "items": { "type": "string", "enum": STAGES } },
                    "min_progress": { "type": "integer", "minimum": 0, "maximum": 100 },
                    "max_progress": { "type": "integer", "minimum": 0, "maximum": 100 },
                    "done": { "type": "boolean", "description": "true = only finished projects, false = only unfinished" },
                    "include_archived": { "type": "boolean" },
                    "sort": { "type": "string", "enum": ["priority", "progress", "updated", "title"] },
                    "reverse": { "type": "boolean", "description": "Reverse the sort order" },
                    "search": { "type": "string", "description": "Case-insensitive text matched against project title, slug, description and task titles" }
                }),
                &[],
            ),
        ),
        read(
            "get_project",
            "Get one project with its tasks and references.",
            obj(json!({ "project_id": id_prop("project") }), &["project_id"]),
        ),
        read(
            "find_project_for_path",
            "Find the project linked to a directory (or one of its parents). Use the agent's working directory to check whether the current repo is already tracked.",
            obj(
                json!({ "path": { "type": "string", "description": "Absolute directory path" } }),
                &["path"],
            ),
        ),
        read(
            "resolve_project",
            "Look up candidate projects by name, slug or fragment. Returns ids; this is the only fuzzy lookup, every other tool needs the exact id.",
            obj(json!({ "query": { "type": "string" } }), &["query"]),
        ),
        read(
            "get_summary",
            "Totals, average progress and counts per stage and priority.",
            obj(json!({}), &[]),
        ),
        read(
            "scan_projects",
            "Find `.project-status` files under a directory and show what loading each one would change (new, changed, in sync, invalid). Writes nothing. Read projectgui://project-file-format for the format.",
            obj(
                json!({
                    "path": { "type": "string", "description": "Absolute directory to search" },
                    "depth": { "type": "integer", "minimum": 0, "maximum": MAX_DEPTH, "description": format!("Directory levels to descend (default {DEFAULT_DEPTH})") }
                }),
                &["path"],
            ),
        ),
        write(
            "sync_project_file",
            "Load the `.project-status` file found directly in a directory: creates the project (linked to that directory) or updates the one already linked there. Never deletes anything and is safe to repeat. Takes a directory, not an id, because the file defines the project.",
            obj(
                json!({ "path": { "type": "string", "description": "Absolute directory that contains .project-status" } }),
                &["path"],
            ),
        ),
        write(
            "create_project",
            "Create a project, optionally with initial tasks. Check find_project_for_path or resolve_project first to avoid duplicates.",
            obj(
                json!({
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "priority": { "type": "string", "enum": PRIORITIES },
                    "stage": { "type": "string", "enum": STAGES },
                    "path": { "type": "string", "description": "Absolute directory to link to the project" },
                    "tasks": { "type": "array", "items": task_item() }
                }),
                &["title"],
            ),
        ),
        write(
            "update_project",
            "Change project fields. Omitted fields stay as they are; path=null unlinks the directory. The id never changes when renaming.",
            obj(
                json!({
                    "project_id": id_prop("project"),
                    "title": { "type": "string" },
                    "slug": { "type": "string", "description": "Lowercase letters, digits and '-'" },
                    "description": { "type": "string" },
                    "priority": { "type": "string", "enum": PRIORITIES },
                    "stage": { "type": "string", "enum": STAGES },
                    "path": { "type": ["string", "null"] },
                    "archived": { "type": "boolean" }
                }),
                &["project_id"],
            ),
        ),
        write(
            "add_tasks",
            "Append tasks to a project. All tasks are validated before any is written.",
            obj(
                json!({ "project_id": id_prop("project"), "tasks": { "type": "array", "minItems": 1, "items": task_item() } }),
                &["project_id", "tasks"],
            ),
        ),
        write(
            "update_task",
            "Change a task's title, description, priority or status. description/priority=null clears them.",
            obj(
                json!({
                    "project_id": id_prop("project"),
                    "task_id": id_prop("task"),
                    "title": { "type": "string" },
                    "description": { "type": ["string", "null"] },
                    "priority": { "type": ["string", "null"], "enum": [PRIORITIES[0], PRIORITIES[1], PRIORITIES[2], PRIORITIES[3], null] },
                    "status": { "type": "string", "enum": STATUSES }
                }),
                &["project_id", "task_id"],
            ),
        ),
        write(
            "set_tasks_status",
            "Set the same status on several tasks at once (all-or-nothing). The quickest way to sync progress.",
            obj(
                json!({
                    "project_id": id_prop("project"),
                    "task_ids": { "type": "array", "minItems": 1, "items": { "type": "string" } },
                    "status": { "type": "string", "enum": STATUSES }
                }),
                &["project_id", "task_ids", "status"],
            ),
        ),
        ToolDef {
            name: "remove_task",
            description: "Permanently delete a task. Requires confirm=true; ask the user first.",
            schema: obj(
                json!({ "project_id": id_prop("project"), "task_id": id_prop("task"), "confirm": { "type": "boolean" } }),
                &["project_id", "task_id", "confirm"],
            ),
            read_only: false,
            destructive: true,
        },
        write(
            "add_reference",
            "Attach a commit, pull request or issue to a project or task. Stored only, nothing is fetched.",
            obj(
                json!({
                    "project_id": id_prop("project"),
                    "kind": { "type": "string", "enum": ["commit", "pr", "issue"] },
                    "value": { "type": "string", "description": "Commit hash, number or title" },
                    "url": { "type": "string" },
                    "task_id": id_prop("task")
                }),
                &["project_id", "kind", "value"],
            ),
        ),
        ToolDef {
            name: "delete_project",
            description: "Permanently delete a project and all its tasks. Requires confirm_title equal to the project's exact title; ask the user first.",
            schema: obj(
                json!({ "project_id": id_prop("project"), "confirm_title": { "type": "string" } }),
                &["project_id", "confirm_title"],
            ),
            read_only: false,
            destructive: true,
        },
    ]
}

fn names(defs: &[ToolDef], name: &str) -> Option<Vec<String>> {
    let props = defs.iter().find(|d| d.name == name)?.schema["properties"].as_object()?;
    Some(props.keys().cloned().collect())
}

fn new_tasks(items: Vec<Map<String, Value>>) -> Result<Vec<NewTask>> {
    items
        .into_iter()
        .map(|m| {
            let a = Args::new(Some(m), &["title", "description", "priority", "status"])?;
            Ok(NewTask {
                title: a.str("title")?,
                description: a.opt_str("description")?,
                priority: a.opt_parse("priority")?,
                status: a.opt_parse("status")?,
            })
        })
        .collect()
}

fn brief(p: &Project) -> Value {
    let pr = p.progress();
    json!({ "id": p.id, "slug": p.slug, "title": p.title, "priority": p.priority, "stage": p.stage,
            "progress": { "done": pr.done, "total": pr.total, "percent": pr.percent() } })
}

/// Runs a tool. Unknown tools are an error; every failure is an `AppError` with a stable code.
pub fn call<R: Repository>(
    svc: &mut Service<R>,
    name: &str,
    raw: Option<Map<String, Value>>,
) -> Result<Value> {
    let defs = definitions();
    let allowed = names(&defs, name).ok_or_else(|| AppError::NotFound {
        what: "tool",
        query: name.into(),
    })?;
    let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
    let a = Args::new(raw, &allowed)?;

    let mut result = match name {
        "list_projects" => {
            let filter = Filter {
                priorities: a.opt_parse_list("priority")?,
                stages: a.opt_parse_list("stage")?,
                min_progress: a.opt_percent("min_progress")?,
                max_progress: a.opt_percent("max_progress")?,
                done: a.opt_bool("done")?,
                include_archived: a.opt_bool("include_archived")?.unwrap_or(false),
                sort: a.opt_parse("sort")?.unwrap_or_default(),
                reverse: a.opt_bool("reverse")?.unwrap_or(false),
                search: a.opt_str("search")?.unwrap_or_default(),
            };
            json!({ "projects": svc.list(&filter)?.iter().map(project_json).collect::<Vec<_>>() })
        }
        "scan_projects" => {
            let depth = a.opt_uint("depth", MAX_DEPTH)?.unwrap_or(DEFAULT_DEPTH);
            json!({ "files": svc.scan(&abs_dir(&a)?, depth, false)? })
        }
        "sync_project_file" => {
            json!(svc.sync_dir(&abs_dir(&a)?, true)?)
        }
        "get_project" => project_json(&svc.find_exact(&a.id("project_id")?)?),
        "find_project_for_path" => {
            let path = a.str("path")?;
            if !Path::new(&path).is_absolute() {
                return Err(AppError::invalid("path", "must be an absolute path"));
            }
            match svc.find_for_path(Path::new(&path))? {
                Some(p) => json!({ "project": project_json(&p) }),
                None => json!({ "project": null }),
            }
        }
        "resolve_project" => {
            json!({ "candidates": svc.search(&a.str("query")?)?.iter().map(brief).collect::<Vec<_>>() })
        }
        "get_summary" => serde_json::to_value(svc.summary()?).expect("summary serializes"),
        "create_project" => {
            let p = svc.create_project(NewProject {
                title: a.str("title")?,
                description: a.opt_str("description")?,
                priority: a.opt_parse("priority")?,
                stage: a.opt_parse("stage")?,
                path: a.opt_str("path")?,
                tasks: new_tasks(a.objects("tasks")?)?,
            })?;
            project_json(&p)
        }
        "update_project" => {
            let id = a.id("project_id")?;
            let mut p = svc.update_project(
                &id,
                ProjectPatch {
                    title: a.opt_str("title")?,
                    slug: a.opt_str("slug")?,
                    description: a.opt_str("description")?,
                    priority: a.opt_parse("priority")?,
                    stage: a.opt_parse("stage")?,
                    path: if a.is_null("path") {
                        Some(None)
                    } else {
                        a.opt_str("path")?.map(Some)
                    },
                },
            )?;
            if let Some(archived) = a.opt_bool("archived")? {
                p = svc.set_archived(&id, archived)?;
            }
            project_json(&p)
        }
        "add_tasks" => {
            project_json(&svc.add_tasks(&a.id("project_id")?, new_tasks(a.objects("tasks")?)?)?)
        }
        "update_task" => {
            let t = svc.update_task(
                &a.id("project_id")?,
                &a.id("task_id")?,
                TaskPatch {
                    title: a.opt_str("title")?,
                    description: if a.is_null("description") {
                        Some(None)
                    } else {
                        a.opt_str("description")?.map(Some)
                    },
                    priority: if a.is_null("priority") {
                        Some(None)
                    } else {
                        a.opt_parse("priority")?.map(Some)
                    },
                    status: a.opt_parse::<TaskStatus>("status")?,
                },
            )?;
            serde_json::to_value(t).expect("task serializes")
        }
        "set_tasks_status" => {
            let ids = a.id_list("task_ids")?;
            project_json(&svc.set_tasks_status(&a.id("project_id")?, &ids, a.parse("status")?)?)
        }
        "remove_task" => {
            let confirm = a.opt_bool("confirm")?.unwrap_or(false);
            let t = svc.remove_task(&a.id("project_id")?, &a.id("task_id")?, confirm)?;
            json!({ "removed": { "id": t.id, "title": t.title } })
        }
        "add_reference" => {
            let task = a.opt_str("task_id")?.map(|_| a.id("task_id")).transpose()?;
            let r = svc.add_reference(
                &a.id("project_id")?,
                task.as_deref(),
                a.parse("kind")?,
                &a.str("value")?,
                a.opt_str("url")?.as_deref(),
            )?;
            serde_json::to_value(r).expect("reference serializes")
        }
        "delete_project" => {
            let p =
                svc.delete_project(&a.id("project_id")?, a.opt_str("confirm_title")?.as_deref())?;
            json!({ "deleted": { "id": p.id, "title": p.title, "tasks_removed": p.tasks.len() } })
        }
        _ => unreachable!("names() only returns catalogue entries"),
    };
    // e.g. a project file left untouched because it has unsynced edits; the agent should tell the user
    let notices = svc.take_notices();
    if let (false, Some(obj)) = (notices.is_empty(), result.as_object_mut()) {
        obj.insert("notices".into(), json!(notices));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::SqliteRepository;
    use crate::service::Service;

    fn svc() -> Service<SqliteRepository> {
        Service::new(SqliteRepository::open_in_memory().unwrap())
    }

    fn run(s: &mut Service<SqliteRepository>, tool: &str, args: Value) -> Result<Value> {
        call(s, tool, args.as_object().cloned())
    }

    fn create(s: &mut Service<SqliteRepository>, title: &str) -> Value {
        run(
            s,
            "create_project",
            json!({ "title": title, "tasks": [{ "title": "one" }, { "title": "two" }] }),
        )
        .unwrap()
    }

    #[test]
    fn every_tool_has_an_object_schema_and_unique_name() {
        let defs = definitions();
        let mut seen: Vec<_> = defs.iter().map(|d| d.name).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), defs.len());
        assert!(
            defs.iter()
                .all(|d| d.schema["type"] == "object" && d.schema["additionalProperties"] == false)
        );
        assert_eq!(defs.iter().filter(|d| d.destructive).count(), 2);
    }

    #[test]
    fn list_projects_searches_and_reverses() {
        let mut s = svc();
        create(&mut s, "Alpha billing");
        create(&mut s, "Beta");
        let titles = |v: Value| -> Vec<String> {
            v["projects"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["title"].as_str().unwrap().to_string())
                .collect()
        };
        let hit = run(&mut s, "list_projects", json!({ "search": "  BILLING " })).unwrap();
        assert_eq!(titles(hit), ["Alpha billing"]);
        let rev = run(
            &mut s,
            "list_projects",
            json!({ "sort": "title", "reverse": true }),
        )
        .unwrap();
        assert_eq!(titles(rev), ["Beta", "Alpha billing"]);
        let err = run(&mut s, "list_projects", json!({ "search": 3 })).unwrap_err();
        assert_eq!(err.to_json()["field"], "search");
    }

    #[test]
    fn scan_and_sync_project_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".project-status"),
            "title = \"Filed\"\n[[task]]\ntitle = \"a\"\n",
        )
        .unwrap();
        let mut s = svc();
        let path = dir.path().to_str().unwrap();
        let scan = run(&mut s, "scan_projects", json!({ "path": path })).unwrap();
        assert_eq!(scan["files"][0]["report"]["action"], "create");
        assert!(
            run(&mut s, "list_projects", json!({})).unwrap()["projects"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        let done = run(&mut s, "sync_project_file", json!({ "path": path })).unwrap();
        assert!(done["applied"] == true && done["title"] == "Filed");
        assert_eq!(
            run(&mut s, "sync_project_file", json!({ "path": path })).unwrap()["action"],
            "in_sync"
        );
        // a change made by the agent is written back; an unsynced file edit is kept and reported
        let pid = done["project_id"].as_str().unwrap().to_string();
        std::fs::write(
            dir.path().join(".project-status"),
            "title = \"Filed\"\n# mine\n",
        )
        .unwrap();
        let r = run(
            &mut s,
            "update_project",
            json!({ "project_id": pid, "stage": "review" }),
        )
        .unwrap();
        assert!(
            r["notices"][0].as_str().unwrap().contains("not synced"),
            "{r}"
        );
        for (tool, args, field) in [
            ("sync_project_file", json!({ "path": "rel" }), "path"),
            (
                "scan_projects",
                json!({ "path": path, "depth": 99 }),
                "depth",
            ),
            (
                "sync_project_file",
                json!({ "path": path, "extra": 1 }),
                "arguments",
            ),
        ] {
            let err = run(&mut s, tool, args).unwrap_err();
            assert_eq!(err.to_json()["field"], field, "{tool}");
        }
    }

    #[test]
    fn create_then_track_progress_by_id() {
        let mut s = svc();
        let p = create(&mut s, "Agent made");
        let (pid, t0) = (
            p["id"].as_str().unwrap(),
            p["tasks"][0]["id"].as_str().unwrap(),
        );
        let p = run(
            &mut s,
            "set_tasks_status",
            json!({ "project_id": pid, "task_ids": [t0], "status": "done" }),
        )
        .unwrap();
        assert_eq!(p["progress"]["percent"], 50);
        let got = run(&mut s, "get_project", json!({ "project_id": pid })).unwrap();
        assert_eq!(got["slug"], "agent-made");
        assert_eq!(
            run(&mut s, "get_summary", json!({})).unwrap()["projects"],
            1
        );
    }

    #[test]
    fn mutations_reject_names_slugs_and_unknown_arguments() {
        let mut s = svc();
        create(&mut s, "Alpha");
        let e = run(
            &mut s,
            "update_project",
            json!({ "project_id": "alpha", "stage": "review" }),
        )
        .unwrap_err();
        assert_eq!(e.code(), "invalid");
        let e = run(
            &mut s,
            "get_project",
            json!({ "project_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "extra": 1 }),
        )
        .unwrap_err();
        assert!(e.to_string().contains("unknown argument"));
        let e = run(
            &mut s,
            "get_project",
            json!({ "project_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV" }),
        )
        .unwrap_err();
        assert_eq!(e.code(), "not_found");
        assert_eq!(
            run(&mut s, "nope", json!({})).unwrap_err().code(),
            "not_found"
        );
    }

    #[test]
    fn resolve_is_the_fuzzy_entry_point_and_reports_all_candidates() {
        let mut s = svc();
        create(&mut s, "Website redesign");
        create(&mut s, "Website blog");
        let r = run(&mut s, "resolve_project", json!({ "query": "website" })).unwrap();
        assert_eq!(r["candidates"].as_array().unwrap().len(), 2);
        assert_eq!(r["candidates"][0]["id"].as_str().unwrap().len(), 26);
    }

    #[test]
    fn destructive_tools_enforce_confirmation_server_side() {
        let mut s = svc();
        let p = create(&mut s, "Doomed");
        let (pid, tid) = (
            p["id"].as_str().unwrap(),
            p["tasks"][0]["id"].as_str().unwrap(),
        );
        let e = run(
            &mut s,
            "delete_project",
            json!({ "project_id": pid, "confirm_title": "wrong" }),
        )
        .unwrap_err();
        assert_eq!(e.code(), "confirmation_required");
        let e = run(
            &mut s,
            "remove_task",
            json!({ "project_id": pid, "task_id": tid, "confirm": false }),
        )
        .unwrap_err();
        assert_eq!(e.code(), "confirmation_required");
        run(
            &mut s,
            "remove_task",
            json!({ "project_id": pid, "task_id": tid, "confirm": true }),
        )
        .unwrap();
        let d = run(
            &mut s,
            "delete_project",
            json!({ "project_id": pid, "confirm_title": "Doomed" }),
        )
        .unwrap();
        assert_eq!(d["deleted"]["tasks_removed"], 1);
    }

    #[test]
    fn update_project_can_clear_path_and_archive() {
        let mut s = svc();
        let p = run(
            &mut s,
            "create_project",
            json!({ "title": "P", "path": "/tmp/p" }),
        )
        .unwrap();
        let pid = p["id"].as_str().unwrap();
        let found = run(
            &mut s,
            "find_project_for_path",
            json!({ "path": "/tmp/p/src" }),
        )
        .unwrap();
        assert_eq!(found["project"]["id"], pid);
        let p = run(
            &mut s,
            "update_project",
            json!({ "project_id": pid, "path": null, "archived": true }),
        )
        .unwrap();
        assert!(p["path"].is_null() && p["archived_at"].is_string());
        assert!(
            run(
                &mut s,
                "find_project_for_path",
                json!({ "path": "relative" })
            )
            .is_err()
        );
    }

    #[test]
    fn invalid_values_name_the_field_and_valid_options() {
        let mut s = svc();
        let e = run(
            &mut s,
            "create_project",
            json!({ "title": "x", "priority": "urgent" }),
        )
        .unwrap_err();
        let j = e.to_json();
        assert_eq!(j["code"], "invalid");
        assert_eq!(j["field"], "priority");
        assert!(j["message"].as_str().unwrap().contains("critical"));
        let e = run(
            &mut s,
            "create_project",
            json!({ "title": "x", "tasks": [{ "title": "" }] }),
        )
        .unwrap_err();
        assert_eq!(e.code(), "invalid");
        assert!(run(&mut s, "list_projects", json!({ "min_progress": 500 })).is_err());
    }

    #[test]
    fn references_attach_to_tasks() {
        let mut s = svc();
        let p = create(&mut s, "R");
        let (pid, tid) = (
            p["id"].as_str().unwrap(),
            p["tasks"][0]["id"].as_str().unwrap(),
        );
        let r = run(
            &mut s,
            "add_reference",
            json!({ "project_id": pid, "kind": "pr", "value": "#9", "task_id": tid }),
        )
        .unwrap();
        assert_eq!(r["task_id"], tid);
        assert_eq!(
            run(&mut s, "get_project", json!({ "project_id": pid })).unwrap()["references"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
