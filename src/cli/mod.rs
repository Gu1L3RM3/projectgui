mod args;
pub(crate) mod color;
#[cfg(feature = "mcp")]
mod install;
pub(crate) mod render;
#[cfg(feature = "update")]
mod update_cli;

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser};
use serde_json::json;

pub use args::Cli;
use args::{Command, ConfigCommand, FilterArgs, RefCommand, TaskCommand};
use color::Style;

use crate::config::{self, Config};
use crate::domain::{AppError, Result, TaskStatus};
use crate::repo::SqliteRepository;
use crate::service::{
    Filter, NewProject, NewTask, ProjectPatch, Service, TaskPatch, projectfile, query,
};

type Svc = Service<SqliteRepository>;

pub fn run() -> i32 {
    let cli = Cli::parse();
    match execute(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

/// Writes to stdout and treats a closed pipe (`projectgui list | head`) as success.
fn out(text: &str) {
    let _ = std::io::stdout().write_all(text.as_bytes());
}

fn out_json(v: &serde_json::Value) {
    out(&format!(
        "{}\n",
        serde_json::to_string_pretty(v).expect("json serializes")
    ));
}

fn io_err(what: impl std::fmt::Display, e: std::io::Error) -> AppError {
    AppError::Storage(format!("{what}: {e}"))
}

fn cwd() -> Result<String> {
    let dir = std::env::current_dir().map_err(|e| io_err("current directory", e))?;
    Ok(dir
        .canonicalize()
        .unwrap_or(dir)
        .to_string_lossy()
        .into_owned())
}

fn to_filter(f: FilterArgs, cfg: &Config) -> Result<Filter> {
    Ok(Filter {
        priorities: f.priority,
        stages: f.stage,
        min_progress: f.min_progress,
        max_progress: f.max_progress,
        done: if f.done {
            Some(true)
        } else if f.open {
            Some(false)
        } else {
            None
        },
        include_archived: f.archived,
        sort: f.sort.as_deref().unwrap_or(&cfg.default_sort).parse()?,
        reverse: f.reverse,
        search: f.search.unwrap_or_default(),
    })
}

fn confirm_typed(prompt: &str, expected: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Err(AppError::ConfirmationRequired(
            "not a terminal; pass --yes to confirm".into(),
        ));
    }
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| io_err("stdin", e))?;
    Ok(line.trim() == expected)
}

fn execute(cli: Cli) -> Result<i32> {
    let config_path = config::config_path();
    let mut cfg = Config::load(&config_path)?;
    let st = Style::detect(cli.no_color, &cfg.color, cli.ascii || cfg.ascii);
    let json = cli.json;

    // Commands that never touch the database.
    match &cli.command {
        Command::Completions { shell } => {
            clap_complete::generate(
                *shell,
                &mut Cli::command(),
                "projectgui",
                &mut std::io::stdout(),
            );
            return Ok(0);
        }
        Command::Config(c) => return config_command(c, &mut cfg, &config_path, json).map(|()| 0),
        #[cfg(feature = "mcp")]
        Command::InstallMcp { target, write } => {
            let exe = std::env::current_exe().map_err(|e| io_err("current executable", e))?;
            if *write {
                for line in install::write(*target, &exe)? {
                    out(&format!("{line}\n"));
                }
            } else {
                out(&install::snippet(*target, &exe));
                out("\n(dry run: add --write to apply)\n");
            }
            return Ok(0);
        }
        Command::Version { .. } => {
            #[cfg(feature = "update")]
            let latest = match &cli.command {
                Command::Version { check: true } => Some(update_cli::latest_now()),
                _ => None,
            };
            let info = json!({
                "name": "projectgui", "version": env!("CARGO_PKG_VERSION"),
                "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
                "database": config::db_path(&cfg), "config": config_path,
            });
            #[cfg(feature = "update")]
            let mut info = info;
            #[cfg(feature = "update")]
            {
                info["install_method"] = update_cli::method_name().into();
                if let Some(l) = &latest {
                    info["latest"] = serde_json::to_value(l.as_ref().ok()).unwrap_or_default();
                }
            }
            if json {
                out_json(&info)
            } else {
                out(&format!(
                    "projectgui {} ({}-{})\n",
                    env!("CARGO_PKG_VERSION"),
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ));
                #[cfg(feature = "update")]
                if let Some(l) = latest {
                    out(&update_cli::describe_latest(l));
                }
            }
            return Ok(0);
        }
        _ => {}
    }

    #[cfg(feature = "update")]
    match &cli.command {
        Command::Update { check, yes } => {
            let mut svc: Svc = Service::new(SqliteRepository::open(&config::db_path(&cfg))?);
            return update_cli::update(&mut svc, *check, *yes, json);
        }
        Command::RefreshUpdateCache => {
            // Detached background job: stay silent whatever happens.
            if let Ok(mut repo) = SqliteRepository::open(&config::db_path(&cfg)) {
                let _ = crate::update::refresh(&mut repo, render::now());
            }
            return Ok(0);
        }
        _ => {}
    }

    let db = config::db_path(&cfg);
    let mut svc: Svc = Service::new(SqliteRepository::open(&db)?);
    let now = render::now();

    #[cfg(feature = "mcp")]
    if matches!(cli.command, Command::Mcp) {
        return crate::mcp::serve(svc).map(|()| 0);
    }

    #[cfg(feature = "gui")]
    if matches!(cli.command, Command::Gui) {
        return crate::gui::run(svc, cfg, config_path).map(|()| 0);
    }

    match cli.command {
        Command::Status {
            name,
            here,
            filters,
        } => {
            if here {
                let dir = cwd()?;
                let p = svc
                    .find_for_path(Path::new(&dir))?
                    .ok_or(AppError::NotFound {
                        what: "project linked to this directory (see `projectgui link`)",
                        query: dir,
                    })?;
                print_detail(&p, &st, json, now);
            } else if let Some(name) = name {
                print_detail(&svc.find(&name)?, &st, json, now);
            } else {
                let filter = to_filter(filters, &cfg)?;
                let projects = svc.list(&filter)?;
                if json {
                    out_json(&projects.iter().map(render::project_json).collect());
                } else if projects.is_empty() && (filter.narrows() || filter.done.is_some()) {
                    out("No projects match. Loosen the filters or the search.\n");
                } else {
                    out(&render::status_overview(
                        &projects,
                        &query::summarize(&projects),
                        &st,
                        now,
                    ));
                }
            }
        }
        Command::List { filters } => {
            let projects = svc.list(&to_filter(filters, &cfg)?)?;
            if json {
                out_json(&projects.iter().map(render::project_json).collect())
            } else {
                out(&render::project_table(&projects, &st, now))
            }
        }
        Command::Show { project } => print_detail(&svc.find(&project)?, &st, json, now),
        Command::Add {
            title,
            description,
            priority,
            stage,
            path,
            here,
            tasks,
        } => {
            let path = if here { Some(cwd()?) } else { path };
            let p = svc.create_project(NewProject {
                title,
                description,
                priority,
                stage,
                path,
                tasks: tasks
                    .into_iter()
                    .map(|t| NewTask {
                        title: t,
                        ..Default::default()
                    })
                    .collect(),
            })?;
            if json {
                out_json(&render::project_json(&p))
            } else {
                out(&format!("Created {} ({})\n", p.title, p.slug))
            }
        }
        Command::Edit {
            project,
            title,
            slug,
            description,
            priority,
            stage,
            path,
            here,
            clear_path,
        } => {
            let path = if here {
                Some(Some(cwd()?))
            } else if clear_path {
                Some(None)
            } else {
                path.map(Some)
            };
            let p = svc.update_project(
                &project,
                ProjectPatch {
                    title,
                    slug,
                    description,
                    priority,
                    stage,
                    path,
                },
            )?;
            if json {
                out_json(&render::project_json(&p))
            } else {
                out(&format!("Updated {} ({})\n", p.title, p.slug))
            }
        }
        Command::Rm { project, yes } => {
            let p = svc.find_exact(&project)?;
            if !yes
                && !confirm_typed(
                    &format!(
                        "Delete '{}' and its {} task(s)? Type the title to confirm: ",
                        p.title,
                        p.tasks.len()
                    ),
                    &p.title,
                )?
            {
                return Err(AppError::ConfirmationRequired(
                    "title did not match; nothing deleted".into(),
                ));
            }
            svc.delete_project(&p.id, Some(&p.title))?;
            out(&format!("Deleted {}\n", p.title));
        }
        Command::Archive { project } => archive(&mut svc, &project, true)?,
        Command::Unarchive { project } => archive(&mut svc, &project, false)?,
        Command::Link { project } => {
            let dir = cwd()?;
            let p = svc.update_project(
                &project,
                ProjectPatch {
                    path: Some(Some(dir.clone())),
                    ..Default::default()
                },
            )?;
            out(&format!("Linked {dir} to {}\n", p.title));
        }
        Command::Task(cmd) => task_command(cmd, &mut svc, &st, json)?,
        Command::Reference(cmd) => ref_command(cmd, &mut svc, &st, json)?,
        Command::Backup { out: dest } => {
            let dest = match dest {
                Some(d) => PathBuf::from(d),
                None => {
                    let dir = db.parent().unwrap_or(Path::new(".")).join("backups");
                    std::fs::create_dir_all(&dir).map_err(|e| io_err(dir.display(), e))?;
                    dir.join(format!("projectgui-{}.db", now.format("%Y%m%d-%H%M%S")))
                }
            };
            svc.backup(&dest)?;
            out(&format!("Backup written to {}\n", dest.display()));
        }
        Command::Sync { path, dry_run } => {
            let dir = dir_arg(path)?;
            let report = svc.sync_dir(&dir, !dry_run)?;
            if json {
                out_json(&json!(report));
            } else {
                out(&render::sync_report(&report, &st));
            }
        }
        Command::Scan { dir, depth, apply } => {
            let dir = dir_arg(dir)?;
            let depth = depth.unwrap_or(projectfile::DEFAULT_DEPTH);
            let items = svc.scan(&dir, depth, apply)?;
            if json {
                out_json(&json!(items));
            } else {
                out(&render::scan_table(&items, apply, &st));
            }
        }
        Command::InitFile { project, force } => {
            let dir = cwd()?;
            let target = svc_project_for_init(&mut svc, project.as_deref(), &dir)?;
            let hint = Path::new(&dir)
                .file_name()
                .map_or_else(|| "My project".into(), |n| n.to_string_lossy().into_owned());
            let path = svc.init_file(Path::new(&dir), target.as_ref(), &hint, force)?;
            out(&format!(
                "Wrote {}\nEdit it, then run: projectgui sync\n",
                path.display()
            ));
        }
        Command::Export { out: dest } => {
            let data = svc.export_json()?;
            match dest {
                None => out(&format!("{data}\n")),
                Some(d) => {
                    let mut f = create_new(&d)?;
                    f.write_all(data.as_bytes()).map_err(|e| io_err(&d, e))?;
                    out(&format!("Exported to {d}\n"));
                }
            }
        }
        Command::Import { file } => {
            let text = std::fs::read_to_string(&file).map_err(|e| io_err(&file, e))?;
            let r = svc.import_json(&text)?;
            out(&format!(
                "Imported {} project(s), skipped {} already present\n",
                r.imported, r.skipped_existing
            ));
        }
        Command::Completions { .. } | Command::Config(_) | Command::Version { .. } => {
            unreachable!("handled above")
        }
        #[cfg(feature = "mcp")]
        Command::Mcp | Command::InstallMcp { .. } => unreachable!("handled above"),
        #[cfg(feature = "update")]
        Command::Update { .. } | Command::RefreshUpdateCache => unreachable!("handled above"),
        #[cfg(feature = "gui")]
        Command::Gui => unreachable!("handled above"),
    }
    // On stderr so `--json` and pipes keep clean data on stdout.
    for n in svc.take_notices() {
        eprintln!("note: {n}");
    }
    #[cfg(feature = "update")]
    if cfg.update.check && !json && !crate::update::env_disabled() && update_cli::interactive() {
        for line in crate::update::passive(svc.repo(), now) {
            eprintln!("{line}");
        }
    }
    Ok(0)
}

/// A directory argument, or the current directory.
fn dir_arg(path: Option<String>) -> Result<PathBuf> {
    match path {
        Some(p) => Ok(PathBuf::from(p)),
        None => cwd().map(PathBuf::from),
    }
}

/// The project `init-file` should describe: the named one (it must be linked here, otherwise a later sync would
/// create a second project for this directory), or the one already linked to exactly this directory.
fn svc_project_for_init(
    svc: &mut Svc,
    name: Option<&str>,
    dir: &str,
) -> Result<Option<crate::domain::Project>> {
    let here = |p: &crate::domain::Project| p.path.as_deref() == Some(dir);
    match name {
        Some(n) => {
            let p = svc.find_exact(n)?;
            if !here(&p) {
                return Err(AppError::invalid(
                    "project",
                    format!(
                        "'{}' is not linked to this directory; run `projectgui link {}` here first",
                        p.slug, p.slug
                    ),
                ));
            }
            Ok(Some(p))
        }
        None => Ok(svc.find_for_path(Path::new(dir))?.filter(here)),
    }
}

/// Creates the file, refusing to overwrite an existing one.
fn create_new(path: &str) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                AppError::Conflict(format!("{path} already exists"))
            } else {
                io_err(path, e)
            }
        })
}

fn print_detail(p: &crate::domain::Project, st: &Style, json: bool, now: crate::domain::Timestamp) {
    if json {
        out_json(&render::project_json(p))
    } else {
        out(&render::project_detail(p, st, now))
    }
}

fn archive(svc: &mut Svc, project: &str, archived: bool) -> Result<()> {
    let p = svc.set_archived(project, archived)?;
    out(&format!(
        "{} {}\n",
        if archived { "Archived" } else { "Unarchived" },
        p.title
    ));
    Ok(())
}

fn task_command(cmd: TaskCommand, svc: &mut Svc, st: &Style, json: bool) -> Result<()> {
    let set_status =
        |svc: &mut Svc, project: &str, tasks: Vec<String>, status: TaskStatus| -> Result<()> {
            let p = svc.set_tasks_status(project, &tasks, status)?;
            let pr = p.progress();
            out(&format!(
                "{} task(s) → {status}. {}: {}% ({}/{})\n",
                tasks.len(),
                p.title,
                pr.percent(),
                pr.done,
                pr.total
            ));
            Ok(())
        };
    match cmd {
        TaskCommand::Add {
            project,
            titles,
            description,
            priority,
        } => {
            let tasks = titles
                .into_iter()
                .map(|title| NewTask {
                    title,
                    description: description.clone(),
                    priority,
                    status: None,
                })
                .collect();
            let p = svc.add_tasks(&project, tasks)?;
            out(&format!(
                "Added. {} now has {} task(s)\n",
                p.title,
                p.tasks.len()
            ));
        }
        TaskCommand::List { project } => {
            let p = svc.find(&project)?;
            if json {
                out_json(&serde_json::to_value(&p.tasks).expect("tasks serialize"))
            } else {
                out(&render::task_list(&p, st))
            }
        }
        TaskCommand::Edit {
            project,
            task,
            title,
            description,
            clear_description,
            priority,
            clear_priority,
        } => {
            let patch = TaskPatch {
                title,
                description: if clear_description {
                    Some(None)
                } else {
                    description.map(Some)
                },
                priority: if clear_priority {
                    Some(None)
                } else {
                    priority.map(Some)
                },
                status: None,
            };
            let t = svc.update_task(&project, &task, patch)?;
            out(&format!("Updated task {}\n", t.title));
        }
        TaskCommand::Start { project, tasks } => {
            set_status(svc, &project, tasks, TaskStatus::InProgress)?
        }
        TaskCommand::Block { project, tasks } => {
            set_status(svc, &project, tasks, TaskStatus::Blocked)?
        }
        TaskCommand::Done { project, tasks } => set_status(svc, &project, tasks, TaskStatus::Done)?,
        TaskCommand::Reopen { project, tasks } => {
            set_status(svc, &project, tasks, TaskStatus::Todo)?
        }
        TaskCommand::Rm { project, task, yes } => {
            let p = svc.find_exact(&project)?;
            let t = crate::domain::model::resolve_task(&p, &task)?.clone();
            if !yes
                && !confirm_typed(
                    &format!("Delete task '{}'? Type its title to confirm: ", t.title),
                    &t.title,
                )?
            {
                return Err(AppError::ConfirmationRequired(
                    "title did not match; nothing deleted".into(),
                ));
            }
            svc.remove_task(&p.id, &t.id, true)?;
            out(&format!("Deleted task {}\n", t.title));
        }
    }
    Ok(())
}

fn ref_command(cmd: RefCommand, svc: &mut Svc, st: &Style, json: bool) -> Result<()> {
    match cmd {
        RefCommand::Add {
            project,
            kind,
            value,
            url,
            task,
        } => {
            let r = svc.add_reference(&project, task.as_deref(), kind, &value, url.as_deref())?;
            out(&format!("Added {} {}\n", r.kind, r.value));
        }
        RefCommand::List { project } => {
            let p = svc.find(&project)?;
            if json {
                out_json(&serde_json::to_value(&p.references).expect("refs serialize"))
            } else {
                for r in &p.references {
                    out(&format!(
                        "{} {} {} {}\n",
                        r.kind,
                        r.value,
                        r.url.as_deref().unwrap_or(""),
                        st.paint(color::DIM, crate::domain::model::short_id(&r.id))
                    ));
                }
            }
        }
        RefCommand::Rm { project, id } => {
            svc.remove_reference(&project, &id)?;
            out("Removed\n");
        }
    }
    Ok(())
}

fn config_command(cmd: &ConfigCommand, cfg: &mut Config, path: &Path, json: bool) -> Result<()> {
    match cmd {
        ConfigCommand::Get { key } => out(&format!("{}\n", cfg.get(key)?)),
        ConfigCommand::Set { key, value } => {
            cfg.set(key, value)?;
            cfg.save(path)?;
            out(&format!("{key} = {}\n", cfg.get(key)?));
        }
        ConfigCommand::List => {
            if json {
                out_json(&serde_json::to_value(&*cfg).expect("config serializes"));
            } else {
                for k in config::KEYS {
                    out(&format!("{k} = {}\n", cfg.get(k)?));
                }
            }
        }
        ConfigCommand::Path => out(&format!(
            "config:   {}\ndatabase: {}\n",
            path.display(),
            config::db_path(cfg).display()
        )),
    }
    Ok(())
}
