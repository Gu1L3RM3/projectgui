//! Prompts are templates a client can offer to a user or model. They do not run anything by
//! themselves: no background agent exists, only the tools the model chooses to call.
use serde_json::{Map, Value};

use crate::domain::{AppError, Result};

pub struct PromptDef {
    pub name: &'static str,
    pub description: &'static str,
    /// (name, description, required)
    pub args: &'static [(&'static str, &'static str, bool)],
}

pub const PROMPTS: &[PromptDef] = &[
    PromptDef {
        name: "track_this_project",
        description: "Find or create the ProjectGui entry for the repository you are working in, and propose its tasks.",
        args: &[(
            "path",
            "Absolute path of the repository (defaults to the current working directory)",
            false,
        )],
    },
    PromptDef {
        name: "sync_project_files",
        description: "Find .project-status files under a directory, show what would change and load them into ProjectGui.",
        args: &[(
            "path",
            "Absolute directory to search (defaults to the current working directory)",
            false,
        )],
    },
    PromptDef {
        name: "sync_progress",
        description: "Compare the work done in this session with the project's tasks and update statuses.",
        args: &[
            ("project_id", "Full project id, if known", false),
            ("summary", "Short description of what was done", false),
        ],
    },
    PromptDef {
        name: "plan_tasks",
        description: "Break a goal into concrete tasks and add them to a project.",
        args: &[
            ("project_id", "Full project id", true),
            ("goal", "What should be achieved", true),
        ],
    },
    PromptDef {
        name: "weekly_review",
        description: "Summarise progress across projects, flag stalled or blocked work and suggest priorities.",
        args: &[],
    },
];

fn arg(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Returns (description, user message text).
pub fn get(name: &str, args: &Map<String, Value>) -> Result<(&'static str, String)> {
    let def = PROMPTS
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| AppError::NotFound {
            what: "prompt",
            query: name.into(),
        })?;
    for (key, _, required) in def.args {
        if *required && arg(args, key).is_none() {
            return Err(AppError::invalid(
                "arguments",
                format!("'{key}' is required"),
            ));
        }
    }
    if let Some(extra) = args
        .keys()
        .find(|k| !def.args.iter().any(|(a, _, _)| a == k))
    {
        return Err(AppError::invalid(
            "arguments",
            format!("unknown argument '{extra}'"),
        ));
    }
    let text = match name {
        "track_this_project" => {
            let path = arg(args, "path").map(|p| format!("`{p}`")).unwrap_or_else(|| "your current working directory".into());
            format!(
                "Keep ProjectGui in sync with the repository at {path}.\n\
                 1. Call `find_project_for_path` with that absolute path.\n\
                 If the repository has a `.project-status` file, call `sync_project_file` with the repository path and stop: \
                 ProjectGui then keeps the file in step, so later changes you make with the tools are written back into it. \
                 Read `projectgui://project-file-format` if you need to write or fix one.\n\
                 2. If nothing is linked, call `resolve_project` with the repository name. If a candidate is clearly the same project, \
                 use `update_project` to link the path; otherwise ask me before creating one.\n\
                 3. If it is new, call `create_project` with a short description, a sensible priority and stage, the path, and 3-8 concrete tasks.\n\
                 4. Report the project id and the task list. Do not delete anything."
            )
        }
        "sync_progress" => {
            let project = arg(args, "project_id").map(|id| format!("project `{id}`")).unwrap_or_else(|| "the project for this repository (use `find_project_for_path`)".into());
            let done = arg(args, "summary").map(|s| format!("\nWork done: {s}")).unwrap_or_default();
            format!(
                "Update {project} to match reality.{done}\n\
                 1. Call `get_project` and read the tasks.\n\
                 2. Use `set_tasks_status` for tasks that are finished, in progress or blocked. Only use the status \"done\" for work that is verifiably complete.\n\
                 3. Add missing work with `add_tasks`; adjust the stage with `update_project` if it changed.\n\
                 4. Summarise what you changed, and show me any notices the results contain (for example a project file left \
                 untouched because it has unsynced edits)."
            )
        }
        "sync_project_files" => {
            let path = arg(args, "path").map(|p| format!("`{p}`")).unwrap_or_else(|| "your current working directory".into());
            format!(
                "Load the project files under {path} into ProjectGui.\n\
                 1. Call `scan_projects` with that absolute path. It writes nothing.\n\
                 2. Show me the list: which files are new, changed, in sync or invalid (with the reason). Stop if there are none.\n\
                 3. After I confirm, call `sync_project_file` once for each new or changed directory. Skip invalid files and tell me why; \
                 read `projectgui://project-file-format` if one needs fixing, and edit the file only if I ask.\n\
                 4. Report what was created or updated. Sync never deletes: tasks missing from a file are kept and written back into it, so mention them. \
                 If a result has notices, show them to me."
            )
        }
        "plan_tasks" => format!(
            "Plan the following goal for project `{}`: {}\n\
             Call `get_project` first so you do not duplicate tasks, then add 3-10 small, verifiable tasks with `add_tasks`. \
             List them back to me with their priorities.",
            arg(args, "project_id").unwrap_or_default(),
            arg(args, "goal").unwrap_or_default()
        ),
        "weekly_review" => "Review all my projects. Call `get_summary` and `list_projects`, then report: what moved forward, \
             what looks stalled (low progress, blocked tasks, old updates), and which two or three projects deserve attention next. \
             Suggest priority changes but do not apply them without asking."
            .to_string(),
        _ => unreachable!("validated above"),
    };
    Ok((def.description, text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn all_prompts_render_and_mention_real_tools() {
        let tools: Vec<_> = super::super::tools::definitions()
            .iter()
            .map(|d| d.name)
            .collect();
        for p in PROMPTS {
            let a = args(json!({ "project_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "goal": "ship it" }));
            let a = if p.args.is_empty() {
                Map::new()
            } else {
                a.into_iter()
                    .filter(|(k, _)| p.args.iter().any(|x| x.0 == k))
                    .collect()
            };
            let (_, text) = get(p.name, &a).unwrap();
            for word in text.split('`').skip(1).step_by(2) {
                if word.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                    assert!(
                        tools.contains(&word),
                        "{} mentions unknown tool {word}",
                        p.name
                    );
                }
            }
        }
    }

    #[test]
    fn validates_arguments() {
        assert!(get("plan_tasks", &args(json!({ "goal": "x" }))).is_err());
        assert!(get("weekly_review", &args(json!({ "x": 1 }))).is_err());
        assert!(get("nope", &Map::new()).is_err());
    }
}
