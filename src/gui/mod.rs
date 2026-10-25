//! Project overview and editing (projects, tasks, references). Thin: every change goes through the service, then
//! the view reloads from storage.
//!
//! Design: a ledger of rows instead of cards. Each project's bar has one segment per task, coloured by
//! status, so the bar says more than a bare percentage. Priority is a four-step meter, never a hue, which
//! keeps colour free to mean task status only.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::font::Weight;
use iced::widget::{
    Space, button, column, container, pick_list, responsive, row, scrollable, slider, text,
    text_input, tooltip,
};
use iced::{
    Background, Center, Color, Element, Fill, FillPortion, Font, Length, Theme, border, time,
};

use crate::cli::render::{ago, now};
use crate::config::{Config, GUI_THEMES, OPACITY_RANGE};
use crate::domain::{AppError, Priority, Project, RefKind, Result, Stage, Task, TaskStatus};
use crate::repo::{Repository, SqliteRepository};
use crate::service::projectfile::{Action, DEFAULT_DEPTH, ScanItem};
use crate::service::{
    Filter, NewProject, NewTask, ProjectPatch, Service, SortKey, TaskPatch, query,
};

mod theme;
#[cfg(target_os = "linux")]
mod tray;
use theme::{Pal, palette};

type Svc = Service<SqliteRepository>;

/// Past this many tasks one segment per task gets too thin to read; segments group by status instead.
const MAX_TASK_SEGMENTS: usize = 40;

/// Below this the controls would overflow the row they sit in.
const MIN_WIDTH: f32 = 520.0;
/// Below this width the header and footer stack instead of sitting side by side.
const NARROW: f32 = 720.0;

/// Open projects listed in the tray menu.
#[cfg(target_os = "linux")]
const TRAY_ROWS: usize = 8;

/// How often the update cache is re-read; the network refresh behind it runs at most daily.
const UPDATE_EVERY: Duration = Duration::from_secs(3600);

/// A project touched this recently counts as "just changed" and is highlighted.
const FRESH_SECS: i64 = 120;

const BOLD: Font = Font {
    weight: Weight::Bold,
    ..Font::DEFAULT
};
const SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..Font::DEFAULT
};

// shortcut: closing the window quits, tray included. Hiding to the tray needs `iced::daemon` (Wayland cannot
// hide a window); switch when a user wants the tray without the window.
pub fn run(svc: Svc, cfg: Config, cfg_path: PathBuf) -> Result<()> {
    #[cfg(target_os = "linux")]
    let (tx, rx) = iced::futures::channel::mpsc::unbounded();
    #[cfg(target_os = "linux")]
    {
        if !tray::single_instance(
            &tray::socket_path(&crate::config::db_path(&cfg)),
            tx.clone(),
        ) {
            eprintln!("ProjectGui is already open; brought it forward.");
            return Ok(());
        }
        TRAY_EVENTS
            .set(std::sync::Mutex::new(Some(rx)))
            .expect("run is called once");
    }
    // iced's boot closure is `Fn`; it runs once, so the service moves out of a cell.
    let slot = std::cell::Cell::new(Some((svc, cfg)));
    iced::application(
        move || {
            let (svc, cfg) = slot.take().expect("boot runs once");
            #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
            let mut app = App::new(svc, cfg, cfg_path.clone());
            #[cfg(target_os = "linux")]
            {
                app.tray = tray::spawn(tx.clone());
                app.sync_tray();
            }
            (app, iced::Task::none())
        },
        |app: &mut App, msg: Msg| match msg {
            #[cfg(target_os = "linux")]
            Msg::Tray(tray::Event::Quit) => iced::exit(),
            #[cfg(target_os = "linux")]
            Msg::Tray(tray::Event::Open(project)) => {
                if let Some(id) = project {
                    app.expanded.insert(id);
                }
                // Wayland may refuse focus stealing; un-minimizing still brings the window back.
                iced::window::latest().and_then(|id| {
                    iced::Task::batch([
                        iced::window::minimize(id, false),
                        iced::window::gain_focus(id),
                    ])
                })
            }
            msg => {
                app.update(msg);
                iced::Task::none()
            }
        },
        App::view,
    )
    .title("ProjectGui")
    .theme(|app: &App| {
        let p = app.pal;
        Theme::custom(
            "ProjectGui",
            iced::theme::Palette {
                background: p.bg,
                text: p.text,
                primary: p.accent,
                success: p.done,
                warning: p.active,
                danger: p.blocked,
            },
        )
    })
    .style(|app: &App, _| iced::theme::Style {
        background_color: app.pal.bg,
        text_color: app.pal.text,
    })
    .window(iced::window::Settings {
        size: iced::Size::new(900.0, 720.0),
        min_size: Some(iced::Size::new(MIN_WIDTH, 420.0)),
        transparent: true,
        // Matches assets/io.github.Gu1L3RM3.ProjectGui.desktop, so docks show the app's icon for this window.
        #[cfg(target_os = "linux")]
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: "io.github.Gu1L3RM3.ProjectGui".into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .subscription(|_| {
        let tick = time::every(Duration::from_secs(1)).map(|_| Msg::Tick);
        #[cfg(target_os = "linux")]
        return iced::Subscription::batch([tick, iced::Subscription::run(tray_events)]);
        #[cfg(not(target_os = "linux"))]
        tick
    })
    .run()
    .map_err(|e| AppError::Storage(format!("gui: {e}")))
}

// `Subscription::run` takes a plain fn, so the receiver reaches it through a slot filled once by `run`.
#[cfg(target_os = "linux")]
type TrayRx = iced::futures::channel::mpsc::UnboundedReceiver<tray::Event>;
#[cfg(target_os = "linux")]
static TRAY_EVENTS: std::sync::OnceLock<std::sync::Mutex<Option<TrayRx>>> =
    std::sync::OnceLock::new();

#[cfg(target_os = "linux")]
fn tray_events() -> impl iced::futures::Stream<Item = Msg> {
    use iced::futures::StreamExt;
    let rx = TRAY_EVENTS
        .get()
        .and_then(|m| m.lock().ok()?.take())
        .expect("run fills the slot before the subscription starts");
    rx.map(Msg::Tray)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Show {
    Open,
    All,
    Finished,
}

impl Show {
    fn label(self) -> &'static str {
        match self {
            Show::Open => "Open",
            Show::All => "All",
            Show::Finished => "Finished",
        }
    }
    /// Archived projects only show under "All", where each is marked as archived.
    fn archived(self) -> bool {
        self == Show::All
    }
    fn done(self) -> Option<bool> {
        match self {
            Show::Open => Some(false),
            Show::All => None,
            Show::Finished => Some(true),
        }
    }
}

#[derive(Debug, Clone)]
enum Msg {
    Tick,
    Toggle(String),
    Show(Show),
    Search(String),
    Sort(SortKey),
    ToggleReverse,
    FilterPriority(Priority),
    FilterStage(Stage),
    ClearFilters,
    Theme(&'static str),
    Opacity(u8),
    OpacityReleased,
    SetStatus(String, String, TaskStatus),
    EditTask(String, String, String),
    EditText(String),
    SaveTask,
    CancelEdit,
    AskRemove(String, String),
    CancelRemove,
    ConfirmRemove,
    TaskDraft(String, String),
    AddTask(String),
    NewProject(bool),
    EditProject(String),
    ScanOpen(bool),
    ScanPath(String),
    RunScan,
    ApplyScan,
    FormTitle(String),
    FormPriority(Priority),
    FormStage(Stage),
    FormDescription(String),
    FormPath(String),
    SaveProject,
    Archive(String, bool),
    AskDelete(String),
    DeleteText(String),
    CancelDelete,
    ConfirmDelete,
    RefDraft(String, RefDraft),
    AddRef(String),
    RemoveRef(String, String),
    DismissBanner,
    #[cfg(target_os = "linux")]
    Tray(tray::Event),
}

const PRIORITIES: [Priority; 4] = [
    Priority::Low,
    Priority::Medium,
    Priority::High,
    Priority::Critical,
];
const STAGES: [Stage; 6] = [
    Stage::Idea,
    Stage::Planning,
    Stage::InProgress,
    Stage::Review,
    Stage::Done,
    Stage::Paused,
];

/// The "Scan folder" form: a directory to search for `.project-status` files and what a scan found there.
struct ScanForm {
    path: String,
    items: Vec<ScanItem>,
    /// True once the items describe changes already made, not a plan.
    applied: bool,
    scanned: bool,
}

/// A task title being edited in place.
struct TaskEdit {
    project: String,
    task: String,
    title: String,
}

/// The project form while it is open: a new project, or an edit of `before`.
struct Form {
    /// The project as it was when editing began; `None` for a new project.
    before: Option<Project>,
    title: String,
    description: String,
    path: String,
    priority: Priority,
    stage: Stage,
}

impl Form {
    fn new() -> Self {
        Self {
            before: None,
            title: String::new(),
            description: String::new(),
            path: String::new(),
            priority: Priority::Medium,
            stage: Stage::Idea,
        }
    }

    fn edit(p: &Project) -> Self {
        Self {
            title: p.title.clone(),
            description: p.description.clone(),
            path: p.path.clone().unwrap_or_default(),
            priority: p.priority,
            stage: p.stage,
            before: Some(p.clone()),
        }
    }

    /// Only the fields the user changed, so a change another process made meanwhile to the rest is kept.
    fn patch(&self, b: &Project) -> ProjectPatch {
        let changed = |new: &str, old: &str| (new != old).then(|| new.to_string());
        ProjectPatch {
            title: changed(&self.title, &b.title),
            description: changed(&self.description, &b.description),
            priority: (self.priority != b.priority).then_some(self.priority),
            stage: (self.stage != b.stage).then_some(self.stage),
            path: changed(&self.path, b.path.as_deref().unwrap_or(""))
                .map(|p| Some(p).filter(|p| !p.trim().is_empty())),
            slug: None,
        }
    }
}

/// A half-typed reference for one project.
#[derive(Debug, Clone)]
struct RefDraft {
    kind: RefKind,
    value: String,
    url: String,
}

const REF_KINDS: [RefKind; 3] = [RefKind::Commit, RefKind::Pr, RefKind::Issue];

struct App {
    svc: Svc,
    cfg_path: PathBuf,
    theme: &'static str,
    opacity: u8,
    pal: Pal,
    filter: Filter,
    show: Show,
    /// Every project as last read; `projects` is this through `filter`, so typing a search never hits the database.
    all: Vec<Project>,
    projects: Vec<Project>,
    expanded: HashSet<String>,
    /// Half-typed new-task titles, by project id.
    drafts: HashMap<String, String>,
    form: Option<Form>,
    scan: Option<ScanForm>,
    editing: Option<TaskEdit>,
    /// (project id, task id) waiting for the second click that confirms removal.
    removing: Option<(String, String)>,
    /// (project id, title typed so far) while a deletion waits for the title to be typed.
    deleting: Option<(String, String)>,
    /// Half-typed references, by project id.
    ref_drafts: HashMap<String, RefDraft>,
    version: Option<i64>,
    error: Option<String>,
    /// Update and notice lines; dismissing hides them for the rest of the session.
    banner: Vec<String>,
    banner_dismissed: bool,
    update_check: bool,
    update_at: Option<Instant>,
    #[cfg(target_os = "linux")]
    tray: Option<tray::Handle>,
}

impl App {
    fn new(svc: Svc, cfg: Config, cfg_path: PathBuf) -> Self {
        let show = Show::Open;
        let theme = GUI_THEMES
            .iter()
            .copied()
            .find(|t| *t == cfg.gui_theme)
            .unwrap_or(GUI_THEMES[0]);
        #[cfg(feature = "update")]
        let update_check = cfg.update.check && !crate::update::env_disabled();
        #[cfg(not(feature = "update"))]
        let update_check = false;
        let mut app = Self {
            svc,
            cfg_path,
            theme,
            opacity: cfg.gui_opacity,
            pal: palette(theme, cfg.gui_opacity),
            filter: Filter {
                sort: cfg.default_sort.parse().unwrap_or(SortKey::Priority),
                done: show.done(),
                include_archived: show.archived(),
                ..Filter::default()
            },
            show,
            all: Vec::new(),
            projects: Vec::new(),
            expanded: HashSet::new(),
            drafts: HashMap::new(),
            form: None,
            scan: None,
            editing: None,
            removing: None,
            deleting: None,
            ref_drafts: HashMap::new(),
            version: None,
            error: None,
            banner: Vec::new(),
            banner_dismissed: false,
            update_check,
            update_at: None,
            #[cfg(target_os = "linux")]
            tray: None,
        };
        app.reload();
        app.check_update();
        app
    }

    fn reload(&mut self) {
        // data_version only moves when another connection commits, which is exactly the CLI/MCP case.
        let res = self.svc.repo().data_version().and_then(|v| {
            if self.version == Some(v) {
                return Ok(None);
            }
            let every = Filter {
                include_archived: true,
                ..Filter::default()
            };
            Ok(Some((v, self.svc.list(&every)?)))
        });
        match res {
            Ok(Some((v, all))) => {
                self.version = Some(v);
                self.all = all;
                self.refilter();
                self.sync_tray();
                self.error = None;
            }
            Ok(None) => {}
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Plans (or, with `apply`, performs) the scan; the form keeps what it found so the user can review it.
    fn run_scan(&mut self, apply: bool) {
        let Some(f) = &mut self.scan else { return };
        let path = f.path.clone();
        let mut items = Vec::new();
        let ok = self.mutate(|svc| {
            items = svc.scan(std::path::Path::new(&path), DEFAULT_DEPTH, apply)?;
            Ok(())
        });
        if let Some(f) = &mut self.scan {
            f.scanned = ok;
            f.applied = ok && apply;
            f.items = items;
        }
    }

    fn check_update(&mut self) {
        if !self.update_check || self.update_at.is_some_and(|t| t.elapsed() < UPDATE_EVERY) {
            return;
        }
        self.update_at = Some(Instant::now());
        #[cfg(feature = "update")]
        if !self.banner_dismissed {
            let lines = crate::update::passive(self.svc.repo(), now());
            if !lines.is_empty() {
                self.banner = lines;
                self.sync_tray();
            }
        }
    }

    /// The tray lists the most important open projects, whatever the window's search and filters.
    fn sync_tray(&self) {
        #[cfg(target_os = "linux")]
        if let Some(t) = &self.tray {
            let open = Filter {
                done: Some(false),
                ..Filter::default()
            };
            let rows = query::apply(self.all.clone(), &open)
                .into_iter()
                .take(TRAY_ROWS)
                .map(|p| {
                    let label = format!("{}  {}%", p.title, p.progress().percent());
                    (p.id, label)
                })
                .collect();
            // A menu label is one line; the full text is in the window.
            let update = self.banner.first().map(|l| l.chars().take(60).collect());
            t.show(rows, update);
        }
    }

    /// A query changed: re-apply it to the loaded projects; the stored data did not change.
    fn refilter(&mut self) {
        self.projects = crate::service::query::apply(self.all.clone(), &self.filter);
    }

    /// Reload first so a hand-edited config is not clobbered by a stale copy; a broken file is reported, not replaced.
    fn save(&mut self, change: impl FnOnce(&mut Config)) {
        let saved = Config::load(&self.cfg_path).and_then(|mut c| {
            change(&mut c);
            c.save(&self.cfg_path)
        });
        self.error = saved.err().map(|e| format!("setting not saved: {e}"));
    }

    /// Runs one service call, then reloads so the view shows the stored result, not a guess.
    fn mutate(&mut self, call: impl FnOnce(&mut Svc) -> Result<()>) -> bool {
        let res = call(&mut self.svc);
        let ok = res.is_ok();
        // A notice (a project file left untouched, say) shows in the same line as errors: both need reading.
        let notices = self.svc.take_notices();
        self.error = res
            .err()
            .map(|e| e.to_string())
            .or_else(|| (!notices.is_empty()).then(|| notices.join("\n")));
        self.version = None;
        self.reload_keep_error();
        ok
    }

    fn reload_keep_error(&mut self) {
        let err = self.error.take();
        self.reload();
        self.error = err.or(self.error.take());
    }

    fn update(&mut self, msg: Msg) {
        match msg {
            Msg::Tick => {
                self.reload();
                self.check_update();
            }
            Msg::DismissBanner => {
                self.banner.clear();
                self.banner_dismissed = true;
                self.sync_tray();
            }
            #[cfg(target_os = "linux")]
            Msg::Tray(_) => unreachable!("handled in run"),
            Msg::Toggle(id) => {
                if !self.expanded.remove(&id) {
                    self.expanded.insert(id);
                }
            }
            Msg::Show(s) => {
                self.show = s;
                self.filter.done = s.done();
                self.filter.include_archived = s.archived();
                self.refilter();
            }
            Msg::Search(q) => {
                self.filter.search = q;
                self.refilter();
            }
            Msg::Sort(k) => {
                self.filter.sort = k;
                self.refilter();
            }
            Msg::ToggleReverse => {
                self.filter.reverse = !self.filter.reverse;
                self.refilter();
            }
            Msg::FilterPriority(p) => {
                self.filter.priorities = vec![p];
                self.refilter();
            }
            Msg::FilterStage(st) => {
                self.filter.stages = vec![st];
                self.refilter();
            }
            Msg::ClearFilters => {
                self.filter.search.clear();
                self.filter.priorities.clear();
                self.filter.stages.clear();
                self.refilter();
            }
            Msg::Theme(name) => {
                self.theme = name;
                self.pal = palette(name, self.opacity);
                self.save(|c| c.gui_theme = name.into());
            }
            Msg::Opacity(v) => {
                self.opacity = v;
                self.pal = palette(self.theme, v);
            }
            // Saved on release only: dragging would otherwise rewrite the file on every pixel.
            Msg::OpacityReleased => {
                let v = self.opacity;
                self.save(|c| c.gui_opacity = v);
            }
            Msg::SetStatus(project, task, status) => {
                self.mutate(|svc| svc.set_tasks_status(&project, &[task], status).map(|_| ()));
            }
            Msg::EditTask(project, task, title) => {
                self.removing = None;
                self.editing = Some(TaskEdit {
                    project,
                    task,
                    title,
                });
            }
            Msg::EditText(t) => {
                if let Some(e) = &mut self.editing {
                    e.title = t;
                }
            }
            Msg::CancelEdit => self.editing = None,
            Msg::SaveTask => {
                let Some(e) = self.editing.take() else { return };
                let patch = TaskPatch {
                    title: Some(e.title.clone()),
                    ..TaskPatch::default()
                };
                if !self.mutate(|svc| svc.update_task(&e.project, &e.task, patch).map(|_| ())) {
                    // Keep what was typed so a validation error does not cost the user their input.
                    self.editing = Some(e);
                }
            }
            Msg::AskRemove(project, task) => {
                self.editing = None;
                self.removing = Some((project, task));
            }
            Msg::CancelRemove => self.removing = None,
            Msg::ConfirmRemove => {
                let Some((project, task)) = self.removing.take() else {
                    return;
                };
                // The service insists on `confirm`; the second click is the user's explicit confirmation.
                self.mutate(|svc| svc.remove_task(&project, &task, true).map(|_| ()));
            }
            Msg::TaskDraft(project, text) => {
                self.drafts.insert(project, text);
            }
            Msg::AddTask(project) => {
                let title = self.drafts.get(&project).cloned().unwrap_or_default();
                if title.trim().is_empty() {
                    return;
                }
                let new = NewTask {
                    title,
                    ..NewTask::default()
                };
                if self.mutate(|svc| svc.add_tasks(&project, vec![new]).map(|_| ())) {
                    self.drafts.remove(&project);
                }
            }
            Msg::NewProject(open) => self.form = open.then(Form::new),
            Msg::EditProject(id) => {
                self.form = self.all.iter().find(|p| p.id == id).map(Form::edit);
            }
            Msg::ScanOpen(open) => {
                self.scan = open.then(|| ScanForm {
                    path: std::env::current_dir()
                        .map(|d| d.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    items: Vec::new(),
                    applied: false,
                    scanned: false,
                });
            }
            Msg::ScanPath(p) => {
                if let Some(f) = &mut self.scan {
                    f.path = p;
                    f.scanned = false;
                    f.items.clear();
                }
            }
            Msg::RunScan => self.run_scan(false),
            Msg::ApplyScan => self.run_scan(true),
            Msg::FormTitle(t) => {
                if let Some(f) = &mut self.form {
                    f.title = t;
                }
            }
            Msg::FormPriority(v) => {
                if let Some(f) = &mut self.form {
                    f.priority = v;
                }
            }
            Msg::FormStage(v) => {
                if let Some(f) = &mut self.form {
                    f.stage = v;
                }
            }
            Msg::FormDescription(d) => {
                if let Some(f) = &mut self.form {
                    f.description = d;
                }
            }
            Msg::FormPath(p) => {
                if let Some(f) = &mut self.form {
                    f.path = p;
                }
            }
            Msg::SaveProject => {
                let Some(f) = self.form.take() else { return };
                let mut saved = None;
                let ok = self.mutate(|svc| {
                    let p = match &f.before {
                        Some(b) => svc.update_project(&b.id, f.patch(b))?,
                        None => svc.create_project(NewProject {
                            title: f.title.clone(),
                            description: Some(f.description.clone()),
                            priority: Some(f.priority),
                            stage: Some(f.stage),
                            path: Some(f.path.clone()).filter(|p| !p.trim().is_empty()),
                            ..NewProject::default()
                        })?,
                    };
                    saved = Some(p.id);
                    Ok(())
                });
                match (ok, saved) {
                    (true, Some(id)) => {
                        self.expanded.insert(id);
                    }
                    // Keep what was typed so a validation error does not cost the user their input.
                    _ => self.form = Some(f),
                }
            }
            Msg::Archive(id, archived) => {
                self.mutate(|svc| svc.set_archived(&id, archived).map(|_| ()));
            }
            Msg::AskDelete(id) => self.deleting = Some((id, String::new())),
            Msg::DeleteText(t) => {
                if let Some((_, typed)) = &mut self.deleting {
                    *typed = t;
                }
            }
            Msg::CancelDelete => self.deleting = None,
            Msg::ConfirmDelete => {
                let Some((id, typed)) = self.deleting.take() else {
                    return;
                };
                // The service compares the typed title with the stored one; the GUI only passes it on.
                if self.mutate(|svc| svc.delete_project(&id, Some(&typed)).map(|_| ())) {
                    self.expanded.remove(&id);
                } else {
                    self.deleting = Some((id, typed));
                }
            }
            Msg::RefDraft(project, draft) => {
                self.ref_drafts.insert(project, draft);
            }
            Msg::AddRef(project) => {
                let Some(d) = self.ref_drafts.get(&project).cloned() else {
                    return;
                };
                let url = Some(d.url.as_str()).filter(|u| !u.trim().is_empty());
                if self.mutate(|svc| {
                    svc.add_reference(&project, None, d.kind, &d.value, url)
                        .map(|_| ())
                }) {
                    self.ref_drafts.remove(&project);
                }
            }
            Msg::RemoveRef(project, reference) => {
                self.mutate(|svc| svc.remove_reference(&project, &reference).map(|_| ()));
            }
        }
    }

    fn view(&self) -> Element<'_, Msg> {
        responsive(move |size| self.layout(size.width < NARROW)).into()
    }

    fn layout(&self, narrow: bool) -> Element<'_, Msg> {
        let c = self.pal;
        let s = query::summarize(&self.projects);
        let summary = match (self.projects.len(), s.tasks_total) {
            (0, _) => String::new(),
            (n, 0) => format!("{n} {}, no tasks yet", plural(n, "project")),
            (n, t) => format!(
                "{n} {}, {} of {t} tasks done",
                plural(n, "project"),
                s.tasks_done
            ),
        };
        let titles = column![
            text("Projects").size(30).font(BOLD),
            text(summary).size(14).color(c.muted)
        ]
        .spacing(4)
        .width(Fill);
        let new_project = button(text("New project").size(14))
            .on_press(Msg::NewProject(
                !self.form.as_ref().is_some_and(|f| f.before.is_none()),
            ))
            .padding([6, 14])
            .style(move |_: &Theme, status| button::Style {
                background: Some(Background::Color(Color {
                    a: if status == button::Status::Hovered {
                        0.9
                    } else {
                        0.75
                    },
                    ..c.accent
                })),
                text_color: c.bg_opaque(),
                border: border::rounded(6),
                ..Default::default()
            });
        let scan_folder = button(text("Scan folder").size(14))
            .on_press(Msg::ScanOpen(self.scan.is_none()))
            .padding([6, 14])
            .style(move |_: &Theme, status| chip(c, self.scan.is_some(), status));
        let header: Element<'_, Msg> = if narrow {
            column![
                titles,
                row![
                    filters(c, self.show),
                    Space::new().width(Fill),
                    scan_folder,
                    new_project
                ]
                .spacing(12)
                .align_y(Center)
            ]
            .spacing(12)
            .into()
        } else {
            row![titles, filters(c, self.show), scan_folder, new_project]
                .spacing(12)
                .align_y(Center)
                .into()
        };
        // Same inset as the rows' own padding, so the title lines up with the project titles below.
        let header = container(header).padding([0, 12]);

        let toolbar = container(toolbar(c, &self.filter, self.filter.narrows())).padding([0, 12]);
        let mut list = column![].width(Fill);
        if !self.banner.is_empty() {
            list = list.push(banner(c, &self.banner));
        }
        if let Some(f) = &self.scan {
            list = list.push(scan_form(c, f));
        }
        if let Some(f) = self.form.as_ref().filter(|f| f.before.is_none()) {
            list = list.push(project_form(c, f));
        }
        if let Some(e) = &self.error {
            list = list.push(container(text(e).size(14).color(c.blocked)).padding(12));
        }
        if self.projects.is_empty() {
            list = list.push(empty(c, self.show, self.filter.narrows()));
        }
        let t = now();
        for (i, p) in self.projects.iter().enumerate() {
            if i > 0 {
                list = list.push(divider(c));
            }
            list = list.push(self.project_row(p, t));
        }

        let body = column![
            header,
            toolbar,
            scrollable(container(list).padding(iced::Padding {
                right: 16.0,
                ..Default::default()
            }))
            .height(Fill),
            container(footer(c, self.theme, self.opacity, narrow)).padding([0, 12]),
        ]
        .spacing(if narrow { 14 } else { 18 });
        container(
            container(body)
                .max_width(820)
                .height(Fill)
                .padding(if narrow { [18, 12] } else { [28, 24] }),
        )
        .center_x(Fill)
        .into()
    }

    fn project_row<'a>(
        &'a self,
        p: &'a Project,
        t: crate::domain::model::Timestamp,
    ) -> Element<'a, Msg> {
        let c = self.pal;
        let pr = p.progress();
        let open = self.expanded.contains(&p.id);

        let figure: Element<'_, Msg> = if pr.total == 0 {
            text("No tasks yet").size(14).color(c.muted).into()
        } else {
            column![
                text(format!("{}%", pr.percent())).size(24).font(BOLD),
                text(format!("{} of {} done", pr.done, pr.total))
                    .size(12)
                    .color(c.muted),
            ]
            .align_x(iced::Right)
            .into()
        };
        // The state line repeats the bar in words, in the bar's colours.
        let count = |s| p.tasks.iter().filter(|t| t.status == s).count();
        let mut meta = row![text(humanize(p.stage.as_str())).size(13).color(c.muted)].spacing(14);
        if p.archived_at.is_some() {
            meta = meta.push(text("archived").size(13).color(c.muted));
        }
        // (wrapped below so a long state line drops to a second line instead of squeezing the title)
        for (n, label, color) in [
            (count(TaskStatus::Blocked), "blocked", c.blocked),
            (count(TaskStatus::InProgress), "in progress", c.active),
        ] {
            if n > 0 {
                meta = meta.push(text(format!("{n} {label}")).size(13).color(color));
            }
        }
        // A change made by the CLI or an agent while the window is open should catch the eye.
        let fresh = (t - p.updated_at).num_seconds() < FRESH_SECS;
        meta = meta.push(
            text(format!("updated {}", ago(p.updated_at, t)))
                .size(13)
                .color(if fresh { c.accent } else { c.muted }),
        );
        let top = row![
            tooltip(
                priority_meter(c, p.priority),
                container(text(format!("{} priority", humanize(p.priority.as_str()))).size(12))
                    .padding([3, 8])
                    .style(move |_: &Theme| container::Style {
                        background: Some(c.bg_opaque().into()),
                        text_color: Some(c.text),
                        border: border::rounded(4).color(c.line).width(1),
                        ..Default::default()
                    }),
                tooltip::Position::Right,
            ),
            column![
                text(&p.title).size(18).font(SEMIBOLD),
                meta.wrap().vertical_spacing(2)
            ]
            .spacing(3)
            .width(Fill),
            figure,
        ]
        .spacing(14)
        .align_y(Center);

        let head = button(column![top, segment_bar(c, p)].spacing(12))
            .on_press(Msg::Toggle(p.id.clone()))
            .padding([14, 12])
            .width(Fill)
            .style(move |_: &Theme, status| button::Style {
                background: (!open && status == button::Status::Hovered)
                    .then_some(Background::Color(c.surface)),
                text_color: c.text,
                border: border::rounded(8),
                ..Default::default()
            });
        let mut item = column![head];
        if open {
            item = item.push(container(self.details(c, p)).padding(iced::Padding {
                top: 0.0,
                right: 12.0,
                bottom: 14.0,
                left: 12.0,
            }));
        }
        container(item)
            .width(Fill)
            .style(move |_: &Theme| container::Style {
                background: open.then_some(Background::Color(c.surface)),
                border: border::rounded(8),
                ..Default::default()
            })
            .into()
    }

    fn task_row<'a>(&'a self, c: Pal, p: &'a Project, task: &'a Task) -> Element<'a, Msg> {
        let at = |project: &str, id: &str| project == p.id && id == task.id;
        if let Some(e) = self.editing.as_ref().filter(|e| at(&e.project, &e.task)) {
            return row![
                text_input("Task title", &e.title)
                    .on_input(Msg::EditText)
                    .on_submit(Msg::SaveTask)
                    .size(14)
                    .padding(6),
                text_button(c, "Save", c.accent, Some(Msg::SaveTask)),
                text_button(c, "Cancel", c.muted, Some(Msg::CancelEdit)),
            ]
            .spacing(8)
            .align_y(Center)
            .into();
        }
        if self.removing.as_ref().is_some_and(|(pr, t)| at(pr, t)) {
            return row![
                text(format!("Remove \"{}\"? This cannot be undone.", task.title))
                    .size(14)
                    .color(c.blocked)
                    .width(Fill),
                text_button(c, "Remove", c.blocked, Some(Msg::ConfirmRemove)),
                text_button(c, "Keep", c.muted, Some(Msg::CancelRemove)),
            ]
            .spacing(8)
            .align_y(Center)
            .into();
        }
        let done = task.status == TaskStatus::Done;
        row![
            text(&task.title)
                .size(15)
                .color(if done { c.muted } else { c.text })
                .width(Fill),
            text(humanize(task.status.as_str()))
                .size(12)
                .color(c.muted)
                .width(76)
                .align_x(iced::Right),
            status_picker(c, &p.id, &task.id, task.status),
            text_button(
                c,
                "Edit",
                c.muted,
                Some(Msg::EditTask(
                    p.id.clone(),
                    task.id.clone(),
                    task.title.clone()
                ))
            ),
            text_button(
                c,
                "Remove",
                c.muted,
                Some(Msg::AskRemove(p.id.clone(), task.id.clone()))
            ),
        ]
        .spacing(10)
        .align_y(Center)
        .into()
    }

    fn details<'a>(&'a self, c: Pal, p: &'a Project) -> Element<'a, Msg> {
        let mut d = column![].spacing(8);
        let editing = self
            .form
            .as_ref()
            .filter(|f| f.before.as_ref().is_some_and(|b| b.id == p.id));
        if let Some(f) = editing {
            d = d.push(project_form(c, f));
        } else {
            if !p.description.is_empty() {
                d = d.push(text(&p.description).size(14).color(c.muted));
            }
            if let Some(path) = &p.path {
                d = d.push(text(path).size(12).color(c.muted));
            }
        }
        d = d.push(self.project_actions(c, p));
        for task in &p.tasks {
            d = d.push(self.task_row(c, p, task));
        }
        let draft = self.drafts.get(&p.id).map(String::as_str).unwrap_or("");
        d.push(
            text_input("Add a task and press Enter", draft)
                .on_input(|t| Msg::TaskDraft(p.id.clone(), t))
                .on_submit(Msg::AddTask(p.id.clone()))
                .size(14)
                .padding(8),
        )
        .push(references(c, p, self.ref_drafts.get(&p.id)))
        .into()
    }

    /// Edit, archive and delete; deleting swaps the row for a field where the exact title must be typed.
    fn project_actions<'a>(&'a self, c: Pal, p: &'a Project) -> Element<'a, Msg> {
        if let Some((_, typed)) = self.deleting.as_ref().filter(|(id, _)| *id == p.id) {
            return row![
                text_input(
                    &format!("Type \"{}\" to delete it and its tasks", p.title),
                    typed
                )
                .on_input(Msg::DeleteText)
                .on_submit(Msg::ConfirmDelete)
                .size(14)
                .padding(6),
                text_button(c, "Delete", c.blocked, Some(Msg::ConfirmDelete)),
                text_button(c, "Keep", c.muted, Some(Msg::CancelDelete)),
            ]
            .spacing(8)
            .align_y(Center)
            .into();
        }
        let archived = p.archived_at.is_some();
        row![
            Space::new().width(Fill),
            text_button(
                c,
                "Edit project",
                c.muted,
                Some(Msg::EditProject(p.id.clone()))
            ),
            text_button(
                c,
                if archived { "Unarchive" } else { "Archive" },
                c.muted,
                Some(Msg::Archive(p.id.clone(), !archived))
            ),
            text_button(c, "Delete", c.muted, Some(Msg::AskDelete(p.id.clone()))),
        ]
        .spacing(4)
        .align_y(Center)
        .into()
    }
}

/// The project's references (commit, PR, issue), each removable, and a row to add one.
fn references<'a>(c: Pal, p: &'a Project, draft: Option<&RefDraft>) -> Element<'a, Msg> {
    let mut col = column![].spacing(6);
    for r in &p.references {
        let task = r
            .task_id
            .as_ref()
            .and_then(|id| p.tasks.iter().find(|t| &t.id == id))
            .map(|t| format!("on \"{}\"", t.title));
        col = col.push(
            row![
                text(r.kind.as_str()).size(12).color(c.muted).width(48),
                text(&r.value).size(14),
                text(r.url.as_deref().unwrap_or(""))
                    .size(12)
                    .color(c.muted)
                    .width(Fill),
                text(task.unwrap_or_default()).size(12).color(c.muted),
                text_button(
                    c,
                    "Remove",
                    c.muted,
                    Some(Msg::RemoveRef(p.id.clone(), r.id.clone()))
                ),
            ]
            .spacing(10)
            .align_y(Center),
        );
    }
    let d = draft.cloned().unwrap_or(RefDraft {
        kind: RefKind::Commit,
        value: String::new(),
        url: String::new(),
    });
    let ready = !d.value.trim().is_empty();
    let add = ready.then(|| Msg::AddRef(p.id.clone()));
    let (d1, d2, d3) = (d.clone(), d.clone(), d.clone());
    col.push(
        row![
            pick_list(&REF_KINDS[..], Some(d.kind), move |kind| Msg::RefDraft(
                p.id.clone(),
                RefDraft { kind, ..d1.clone() }
            ))
            .text_size(14),
            text_input("Commit, PR or issue", &d.value)
                .on_input(move |value| Msg::RefDraft(
                    p.id.clone(),
                    RefDraft {
                        value,
                        ..d2.clone()
                    }
                ))
                .on_submit_maybe(add.clone())
                .size(14)
                .padding(8),
            text_input("URL (optional)", &d.url)
                .on_input(move |url| Msg::RefDraft(p.id.clone(), RefDraft { url, ..d3.clone() }))
                .on_submit_maybe(add.clone())
                .size(14)
                .padding(8),
            text_button(c, "Add", c.accent, add),
        ]
        .spacing(8)
        .align_y(Center),
    )
    .into()
}

/// Search on its own line so it keeps its width; the pickers wrap beneath it on narrow windows.
fn toolbar<'a>(c: Pal, f: &'a Filter, filtered: bool) -> Element<'a, Msg> {
    let mut controls = row![
        text("Sort").size(13).color(c.muted),
        pick_list(&SortKey::ALL[..], Some(f.sort), Msg::Sort).text_size(14),
        button(text(if f.reverse { "Reversed" } else { "Reverse" }).size(14))
            .on_press(Msg::ToggleReverse)
            .padding([5, 12])
            .style(move |_: &Theme, status| chip(c, f.reverse, status)),
        pick_list(
            &PRIORITIES[..],
            f.priorities.first().copied(),
            Msg::FilterPriority
        )
        .placeholder("Any priority")
        .text_size(14),
        pick_list(&STAGES[..], f.stages.first().copied(), Msg::FilterStage)
            .placeholder("Any stage")
            .text_size(14),
    ]
    .spacing(8)
    .align_y(Center);
    if filtered {
        controls = controls.push(
            button(text("Clear").size(14))
                .on_press(Msg::ClearFilters)
                .padding([5, 12])
                .style(move |_: &Theme, status| chip(c, false, status)),
        );
    }
    column![
        text_input("Search projects and tasks", &f.search)
            .on_input(Msg::Search)
            .size(14)
            .padding(8),
        controls.wrap().vertical_spacing(8),
    ]
    .spacing(10)
    .into()
}

/// Quiet inline action: coloured text, a faint background only on hover.
fn text_button<'a>(
    c: Pal,
    label: &'a str,
    color: Color,
    on_press: Option<Msg>,
) -> Element<'a, Msg> {
    button(text(label).size(13))
        .on_press_maybe(on_press)
        .padding([3, 8])
        .style(move |_: &Theme, status| button::Style {
            background: (status == button::Status::Hovered).then_some(Background::Color(c.surface)),
            text_color: color,
            border: border::rounded(5),
            ..Default::default()
        })
        .into()
}

fn chip(c: Pal, on: bool, status: button::Status) -> button::Style {
    button::Style {
        background: if on {
            Some(Background::Color(Color {
                a: 0.18,
                ..c.accent
            }))
        } else if status == button::Status::Hovered {
            Some(Background::Color(c.surface))
        } else {
            None
        },
        text_color: if on { c.accent } else { c.muted },
        border: border::rounded(6).color(c.line).width(1),
        ..Default::default()
    }
}

fn filters<'a>(c: Pal, current: Show) -> Element<'a, Msg> {
    let mut r = row![].spacing(4);
    for s in [Show::Open, Show::All, Show::Finished] {
        let on = s == current;
        r = r.push(
            button(text(s.label()).size(14))
                .on_press(Msg::Show(s))
                .padding([6, 14])
                .style(move |_: &Theme, status| button::Style {
                    background: if on {
                        Some(Background::Color(Color {
                            a: 0.18,
                            ..c.accent
                        }))
                    } else if status == button::Status::Hovered {
                        Some(Background::Color(c.surface))
                    } else {
                        None
                    },
                    text_color: if on { c.accent } else { c.muted },
                    border: border::rounded(6),
                    ..Default::default()
                }),
        );
    }
    r.into()
}

/// Four small squares, one per status: the filled one is current, click another to change it.
fn status_picker<'a>(c: Pal, project: &str, task: &str, current: TaskStatus) -> Element<'a, Msg> {
    let mut r = row![].spacing(6).align_y(Center);
    for status in [
        TaskStatus::Todo,
        TaskStatus::InProgress,
        TaskStatus::Blocked,
        TaskStatus::Done,
    ] {
        let color = status_color(c, status);
        let on = status == current;
        let square = container(Space::new())
            .width(14)
            .height(14)
            .style(move |_: &Theme| container::Style {
                background: on.then_some(Background::Color(color)),
                border: border::rounded(3).color(color).width(2),
                ..Default::default()
            });
        r = r.push(tooltip(
            button(square)
                .on_press(Msg::SetStatus(project.into(), task.into(), status))
                .padding(0)
                .style(|_: &Theme, _| button::Style::default()),
            container(text(humanize(status.as_str())).size(12))
                .padding([3, 8])
                .style(move |_: &Theme| container::Style {
                    background: Some(c.bg_opaque().into()),
                    text_color: Some(c.text),
                    border: border::rounded(4).color(c.line).width(1),
                    ..Default::default()
                }),
            tooltip::Position::Top,
        ));
    }
    r.into()
}

fn scan_form<'a>(c: Pal, f: &'a ScanForm) -> Element<'a, Msg> {
    let plan_has_work = f.items.iter().any(|i| {
        i.report
            .as_ref()
            .is_some_and(|r| r.plan.action != Action::InSync)
    });
    let can_scan = !f.path.trim().is_empty();
    let mut col = column![
        text_input("Folder to search for .project-status files", &f.path)
            .on_input(Msg::ScanPath)
            .on_submit_maybe(can_scan.then_some(Msg::RunScan))
            .size(15)
            .padding(9),
    ]
    .spacing(12);
    for i in &f.items {
        let (label, color, detail) = match (&i.report, &i.error) {
            (Some(r), _) => {
                let (label, color) = match (r.plan.action, f.applied) {
                    (Action::Create, true) => ("created", c.done),
                    (Action::Create, false) => ("new", c.done),
                    (Action::Update, true) => ("updated", c.active),
                    (Action::Update, false) => ("changed", c.active),
                    (Action::InSync, _) => ("in sync", c.muted),
                };
                (label, color, r.title.clone())
            }
            (None, e) => ("invalid", c.blocked, e.clone().unwrap_or_default()),
        };
        col = col.push(
            row![
                text(label).size(13).color(color).width(64),
                column![text(detail).size(14), text(&i.dir).size(12).color(c.muted)]
                    .spacing(2)
                    .width(Fill),
            ]
            .spacing(10),
        );
    }
    if f.scanned && f.items.is_empty() {
        col = col.push(
            text("No .project-status file found. Create one with: pgui init-file")
                .size(14)
                .color(c.muted),
        );
    }
    col = col.push(
        row![
            Space::new().width(Fill),
            button(text("Close").size(14))
                .on_press(Msg::ScanOpen(false))
                .padding([7, 14])
                .style(button::text),
            button(text("Scan").size(14))
                .on_press_maybe(can_scan.then_some(Msg::RunScan))
                .padding([7, 14]),
            button(text("Apply").size(14))
                .on_press_maybe((plan_has_work && !f.applied).then_some(Msg::ApplyScan))
                .padding([7, 14]),
        ]
        .spacing(12)
        .align_y(Center),
    );
    container(col)
        .padding(14)
        .width(Fill)
        .style(move |_: &Theme| container::Style {
            background: Some(c.surface.into()),
            border: border::rounded(8).color(c.line).width(1),
            ..Default::default()
        })
        .into()
}

fn banner<'a>(c: Pal, lines: &'a [String]) -> Element<'a, Msg> {
    let mut col = column![].spacing(4).width(Fill);
    for l in lines {
        col = col.push(text(l).size(14));
    }
    container(
        row![
            col,
            text_button(c, "Dismiss", c.muted, Some(Msg::DismissBanner))
        ]
        .spacing(12)
        .align_y(Center),
    )
    .padding(12)
    .width(Fill)
    .style(move |_: &Theme| container::Style {
        background: Some(
            Color {
                a: 0.12,
                ..c.accent
            }
            .into(),
        ),
        border: border::rounded(8).color(c.accent).width(1),
        ..Default::default()
    })
    .into()
}

fn project_form<'a>(c: Pal, f: &'a Form) -> Element<'a, Msg> {
    let ready = !f.title.trim().is_empty();
    let save = ready.then_some(Msg::SaveProject);
    let label = if f.before.is_some() {
        "Save"
    } else {
        "Create project"
    };
    let create = button(text(label).size(14)).padding([7, 14]);
    container(
        column![
            text_input("Project title", &f.title)
                .on_input(Msg::FormTitle)
                .on_submit_maybe(save.clone())
                .size(15)
                .padding(9),
            text_input("Description (optional)", &f.description)
                .on_input(Msg::FormDescription)
                .on_submit_maybe(save.clone())
                .size(14)
                .padding(8),
            text_input("Folder path (optional, absolute)", &f.path)
                .on_input(Msg::FormPath)
                .on_submit_maybe(save.clone())
                .size(14)
                .padding(8),
            row![
                row![
                    text("Priority").size(13).color(c.muted),
                    pick_list(&PRIORITIES[..], Some(f.priority), Msg::FormPriority).text_size(14),
                ]
                .spacing(8)
                .align_y(Center),
                row![
                    text("Stage").size(13).color(c.muted),
                    pick_list(&STAGES[..], Some(f.stage), Msg::FormStage).text_size(14),
                ]
                .spacing(8)
                .align_y(Center),
                Space::new().width(Fill),
                button(text("Cancel").size(14))
                    .on_press(Msg::NewProject(false))
                    .padding([7, 14])
                    .style(button::text),
                create.on_press_maybe(save),
            ]
            .spacing(12)
            .align_y(Center),
        ]
        .spacing(12),
    )
    .padding(14)
    .width(Fill)
    .style(move |_: &Theme| container::Style {
        background: Some(c.surface.into()),
        border: border::rounded(8).color(c.line).width(1),
        ..Default::default()
    })
    .into()
}

fn status_color(c: Pal, s: TaskStatus) -> Color {
    match s {
        TaskStatus::Done => c.done,
        TaskStatus::InProgress => c.active,
        TaskStatus::Blocked => c.blocked,
        TaskStatus::Todo => c.todo,
    }
}

fn swatch<'a>(color: Color, size: u32) -> Element<'a, Msg> {
    container(Space::new())
        .width(size)
        .height(size)
        .style(move |_: &Theme| container::Style {
            background: Some(color.into()),
            border: border::rounded(2),
            ..Default::default()
        })
        .into()
}

/// One segment per task, ordered done -> in progress -> blocked -> to do, so progress reads left to right.
fn segment_bar<'a>(c: Pal, p: &Project) -> Element<'a, Msg> {
    const ORDER: [TaskStatus; 4] = [
        TaskStatus::Done,
        TaskStatus::InProgress,
        TaskStatus::Blocked,
        TaskStatus::Todo,
    ];
    let seg = |color: Color, portion: u16| -> Element<'a, Msg> {
        container(Space::new())
            .width(FillPortion(portion))
            .height(10)
            .style(move |_: &Theme| container::Style {
                background: Some(color.into()),
                border: border::rounded(3),
                ..Default::default()
            })
            .into()
    };
    let mut bar = row![].spacing(2);
    if p.tasks.is_empty() {
        return bar.push(seg(c.todo, 1)).into();
    }
    for status in ORDER {
        let n = p.tasks.iter().filter(|t| t.status == status).count();
        if p.tasks.len() <= MAX_TASK_SEGMENTS {
            for _ in 0..n {
                bar = bar.push(seg(status_color(c, status), 1));
            }
        } else if n > 0 {
            bar = bar.push(seg(
                status_color(c, status),
                u16::try_from(n).unwrap_or(u16::MAX),
            ));
        }
    }
    bar.into()
}

/// Four rising bars; filled count = importance. Colour stays reserved for task status.
fn priority_meter<'a>(c: Pal, p: Priority) -> Element<'a, Msg> {
    let filled = match p {
        Priority::Low => 1,
        Priority::Medium => 2,
        Priority::High => 3,
        Priority::Critical => 4,
    };
    let mut r = row![].spacing(2).align_y(iced::alignment::Vertical::Bottom);
    for i in 1..=4u32 {
        let color = if i <= filled { c.accent } else { c.line };
        r = r.push(
            container(Space::new())
                .width(4)
                .height(4 + 4 * i)
                .style(move |_: &Theme| container::Style {
                    background: Some(color.into()),
                    border: border::rounded(1),
                    ..Default::default()
                }),
        );
    }
    container(r).height(Length::Shrink).into()
}

fn divider<'a>(c: Pal) -> Element<'a, Msg> {
    container(Space::new())
        .width(Fill)
        .height(1)
        .style(move |_: &Theme| container::Style {
            background: Some(c.line.into()),
            ..Default::default()
        })
        .into()
}

/// Theme picker on the left, status legend on the right.
fn footer<'a>(c: Pal, current: &str, opacity: u8, narrow: bool) -> Element<'a, Msg> {
    let mut dots = row![].spacing(8).align_y(Center);
    for &name in GUI_THEMES {
        let p = palette(name, 100);
        let on = name == current;
        // Ring in the text colour marks the active theme; the fill previews that theme's background and accent.
        let dot = container(container(Space::new()).width(14).height(14).style(
            move |_: &Theme| container::Style {
                background: Some(Color { a: 1.0, ..p.bg }.into()),
                border: border::rounded(7).color(p.accent).width(3),
                ..Default::default()
            },
        ))
        .padding(2)
        .style(move |_: &Theme| container::Style {
            border: border::rounded(10)
                .color(if on { c.text } else { Color::TRANSPARENT })
                .width(1),
            ..Default::default()
        });
        dots = dots.push(tooltip(
            button(dot)
                .on_press(Msg::Theme(name))
                .padding(0)
                .style(|_: &Theme, _| button::Style::default()),
            container(text(humanize(name)).size(12))
                .padding([3, 8])
                .style(move |_: &Theme| container::Style {
                    background: Some(c.bg_opaque().into()),
                    text_color: Some(c.text),
                    border: border::rounded(4).color(c.line).width(1),
                    ..Default::default()
                }),
            tooltip::Position::Top,
        ));
    }
    let mut legend = row![].spacing(16).align_y(Center);
    for (label, color) in [
        ("done", c.done),
        ("in progress", c.active),
        ("blocked", c.blocked),
        ("to do", c.todo),
    ] {
        legend = legend.push(
            row![swatch(color, 10), text(label).size(12).color(c.muted)]
                .spacing(6)
                .align_y(Center),
        );
    }
    let mut left = row![dots].spacing(20).align_y(Center);
    // Opacity only means something for the translucent theme.
    if current == GUI_THEMES[0] {
        left = left.push(
            row![
                text("Opacity").size(12).color(c.muted),
                slider(OPACITY_RANGE, opacity, Msg::Opacity)
                    .on_release(Msg::OpacityReleased)
                    .width(110),
                text(format!("{opacity}%"))
                    .size(12)
                    .color(c.muted)
                    .width(34),
            ]
            .spacing(8)
            .align_y(Center),
        );
    }
    if narrow {
        return column![left, legend].spacing(10).into();
    }
    row![left, Space::new().width(Fill), legend]
        .spacing(16)
        .align_y(Center)
        .into()
}

fn empty<'a>(c: Pal, show: Show, filtered: bool) -> Element<'a, Msg> {
    let msg = match show {
        _ if filtered => "No project matches. Change the search or press Clear.",
        Show::Open => "Nothing open. Add a project with: pgui add \"Project name\"",
        Show::All => "No projects yet. Add one with: pgui add \"Project name\"",
        Show::Finished => {
            "No finished projects yet. A project finishes when all its tasks are done."
        }
    };
    container(text(msg).size(15).color(c.muted))
        .padding([40, 12])
        .into()
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.into()
    } else {
        format!("{word}s")
    }
}

/// "in-progress" -> "In progress"
fn humanize(s: &str) -> String {
    let s = s.replace('-', " ");
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let svc = Service::new(SqliteRepository::open_in_memory().unwrap());
        let mut cfg = Config::default();
        // A stale cache would spawn this test binary as the refresh child.
        cfg.update.check = false;
        App::new(svc, cfg, PathBuf::from("/nonexistent/pg.toml"))
    }

    #[cfg(feature = "update")]
    #[test]
    fn banner_shows_a_cached_release_until_dismissed() {
        let mut repo = SqliteRepository::open_in_memory().unwrap();
        let cache = crate::update::Cache {
            checked_at: Some(now()),
            release: Some(crate::update::check::Release {
                version: "99.0.0".parse().unwrap(),
                url: "https://example.com/r".into(),
            }),
            notice: None,
        };
        repo.kv_set("update.cache", &serde_json::to_string(&cache).unwrap())
            .unwrap();
        let mut app = App::new(
            Service::new(repo),
            Config::default(),
            PathBuf::from("/nonexistent/pg.toml"),
        );
        assert!(app.banner[0].contains("99.0.0"), "{:?}", app.banner);
        app.update(Msg::DismissBanner);
        app.update_at = None;
        app.check_update();
        assert!(app.banner.is_empty(), "dismissed stays hidden");
    }

    fn create(app: &mut App, title: &str) {
        app.update(Msg::NewProject(true));
        app.update(Msg::FormTitle(title.into()));
        app.update(Msg::SaveProject);
    }

    #[test]
    fn form_creates_a_project_and_expands_it() {
        let mut app = app();
        create(&mut app, "Demo");
        assert!(app.form.is_none() && app.error.is_none());
        assert_eq!(app.projects.len(), 1);
        assert!(app.expanded.contains(&app.projects[0].id));
    }

    #[test]
    fn rejected_input_keeps_the_form_and_shows_the_error() {
        let mut app = app();
        create(&mut app, "bad\u{7}title");
        assert!(app.error.is_some());
        assert_eq!(app.form.as_ref().unwrap().title, "bad\u{7}title");
        assert!(app.projects.is_empty());
    }

    #[test]
    fn tasks_can_be_added_and_change_status() {
        let mut app = app();
        // "Open" hides a project once its last task is done, which would empty the list mid-test.
        app.update(Msg::Show(Show::All));
        create(&mut app, "Demo");
        let id = app.projects[0].id.clone();
        app.update(Msg::TaskDraft(id.clone(), "first".into()));
        app.update(Msg::AddTask(id.clone()));
        assert!(app.drafts.is_empty(), "draft is cleared once stored");
        let task = app.projects[0].tasks[0].id.clone();
        app.update(Msg::SetStatus(id, task, TaskStatus::Done));
        assert_eq!(app.projects[0].tasks[0].status, TaskStatus::Done);
        assert_eq!(app.projects[0].progress().percent(), 100);
    }

    #[test]
    fn search_sort_and_filters_narrow_the_list_and_clear_restores_it() {
        let mut app = app();
        create(&mut app, "Alpha");
        create(&mut app, "Beta");
        let beta = app
            .projects
            .iter()
            .find(|p| p.title == "Beta")
            .unwrap()
            .id
            .clone();
        app.update(Msg::TaskDraft(beta, "write docs".into()));
        app.update(Msg::AddTask(
            app.projects
                .iter()
                .find(|p| p.title == "Beta")
                .unwrap()
                .id
                .clone(),
        ));

        let loaded = app.version;
        app.update(Msg::Search("DOCS".into()));
        assert_eq!(app.projects.len(), 1, "matches a task title, ignoring case");
        assert_eq!(
            app.version, loaded,
            "typing filters the loaded list, no reload"
        );
        assert!(app.filter.narrows());

        app.update(Msg::ClearFilters);
        assert_eq!(app.projects.len(), 2);
        app.update(Msg::Sort(SortKey::Title));
        app.update(Msg::ToggleReverse);
        assert_eq!(app.projects[0].title, "Beta");
        app.update(Msg::FilterPriority(Priority::Critical));
        assert!(app.projects.is_empty(), "both projects are medium priority");
        app.update(Msg::ClearFilters);
        assert_eq!(app.projects.len(), 2);
    }

    #[test]
    fn task_title_can_be_edited_and_a_bad_edit_keeps_the_input() {
        let mut app = app();
        create(&mut app, "Demo");
        let id = app.projects[0].id.clone();
        app.update(Msg::TaskDraft(id.clone(), "first".into()));
        app.update(Msg::AddTask(id.clone()));
        let task = app.projects[0].tasks[0].id.clone();

        app.update(Msg::EditTask(id.clone(), task.clone(), "first".into()));
        app.update(Msg::EditText("second".into()));
        app.update(Msg::SaveTask);
        assert!(app.editing.is_none() && app.error.is_none());
        assert_eq!(app.projects[0].tasks[0].title, "second");

        app.update(Msg::EditTask(id, task, "second".into()));
        app.update(Msg::EditText("bad\u{7}".into()));
        app.update(Msg::SaveTask);
        assert!(app.error.is_some());
        assert_eq!(app.editing.as_ref().unwrap().title, "bad\u{7}");
        assert_eq!(app.projects[0].tasks[0].title, "second");
    }

    #[test]
    fn removing_a_task_needs_the_second_click() {
        let mut app = app();
        create(&mut app, "Demo");
        let id = app.projects[0].id.clone();
        app.update(Msg::TaskDraft(id.clone(), "first".into()));
        app.update(Msg::AddTask(id.clone()));
        let task = app.projects[0].tasks[0].id.clone();

        app.update(Msg::ConfirmRemove);
        assert_eq!(
            app.projects[0].tasks.len(),
            1,
            "confirm without asking does nothing"
        );
        app.update(Msg::AskRemove(id.clone(), task.clone()));
        app.update(Msg::CancelRemove);
        app.update(Msg::ConfirmRemove);
        assert_eq!(app.projects[0].tasks.len(), 1, "keep leaves the task");
        app.update(Msg::AskRemove(id, task));
        app.update(Msg::ConfirmRemove);
        assert!(app.projects[0].tasks.is_empty() && app.removing.is_none());
    }

    #[test]
    fn scan_form_plans_first_and_only_writes_on_apply() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".project-status"),
            "title = \"Filed\"\n[[task]]\ntitle = \"a\"\n",
        )
        .unwrap();
        let mut app = app();
        app.update(Msg::ScanOpen(true));
        app.update(Msg::ScanPath(dir.path().to_string_lossy().into_owned()));
        app.update(Msg::RunScan);
        let f = app.scan.as_ref().unwrap();
        assert!(f.scanned && !f.applied && f.items.len() == 1);
        assert_eq!(
            f.items[0].report.as_ref().unwrap().plan.action,
            Action::Create
        );
        assert!(app.projects.is_empty(), "scanning alone writes nothing");

        app.update(Msg::ApplyScan);
        assert!(app.scan.as_ref().unwrap().applied && app.error.is_none());
        assert_eq!(app.projects.len(), 1);
        assert_eq!(app.projects[0].title, "Filed");

        app.update(Msg::ScanPath("/definitely/not/here".into()));
        app.update(Msg::RunScan);
        assert!(app.error.is_some() && !app.scan.as_ref().unwrap().scanned);
        app.update(Msg::ScanOpen(false));
        assert!(app.scan.is_none());
    }

    #[test]
    fn project_edit_sends_only_changed_fields() {
        let mut app = app();
        create(&mut app, "Demo");
        let id = app.projects[0].id.clone();
        app.update(Msg::EditProject(id.clone()));
        // Another process raises the priority while the form is open; saving must not revert it.
        app.svc
            .update_project(
                &id,
                ProjectPatch {
                    priority: Some(Priority::High),
                    ..ProjectPatch::default()
                },
            )
            .unwrap();
        app.update(Msg::FormTitle("Renamed".into()));
        app.update(Msg::FormDescription("what it is".into()));
        app.update(Msg::SaveProject);
        assert!(app.form.is_none() && app.error.is_none());
        let p = &app.projects[0];
        assert_eq!(
            (p.title.as_str(), p.description.as_str(), p.priority),
            ("Renamed", "what it is", Priority::High)
        );

        app.update(Msg::EditProject(id));
        app.update(Msg::FormPath("relative/path".into()));
        app.update(Msg::SaveProject);
        assert!(
            app.error.is_some() && app.form.is_some(),
            "bad path keeps the form"
        );
    }

    #[test]
    fn archive_hides_outside_all_and_delete_needs_the_exact_title() {
        let mut app = app();
        create(&mut app, "Demo");
        let id = app.projects[0].id.clone();
        app.update(Msg::Archive(id.clone(), true));
        assert!(app.projects.is_empty(), "Open hides archived projects");
        app.update(Msg::Show(Show::All));
        assert!(app.projects[0].archived_at.is_some());
        app.update(Msg::Archive(id.clone(), false));
        assert!(app.projects[0].archived_at.is_none());

        app.update(Msg::AskDelete(id.clone()));
        app.update(Msg::DeleteText("demo".into()));
        app.update(Msg::ConfirmDelete);
        assert!(app.error.is_some() && app.deleting.is_some());
        assert_eq!(app.projects.len(), 1, "wrong title deletes nothing");
        app.update(Msg::DeleteText("Demo".into()));
        app.update(Msg::ConfirmDelete);
        assert!(app.projects.is_empty() && app.deleting.is_none());
    }

    #[test]
    fn references_can_be_added_and_removed() {
        let mut app = app();
        create(&mut app, "Demo");
        let id = app.projects[0].id.clone();
        let draft = |url: &str| RefDraft {
            kind: RefKind::Pr,
            value: "#12".into(),
            url: url.into(),
        };
        app.update(Msg::RefDraft(id.clone(), draft("javascript:x")));
        app.update(Msg::AddRef(id.clone()));
        assert!(app.error.is_some() && app.ref_drafts.contains_key(&id));
        app.update(Msg::RefDraft(
            id.clone(),
            draft("https://example.com/pr/12"),
        ));
        app.update(Msg::AddRef(id.clone()));
        assert!(app.error.is_none() && app.ref_drafts.is_empty());
        let r = &app.projects[0].references[0];
        assert_eq!((r.kind, r.value.as_str()), (RefKind::Pr, "#12"));
        app.update(Msg::RemoveRef(id, r.id.clone()));
        assert!(app.projects[0].references.is_empty());
    }

    #[test]
    fn blank_task_draft_is_ignored() {
        let mut app = app();
        create(&mut app, "Demo");
        let id = app.projects[0].id.clone();
        app.update(Msg::TaskDraft(id.clone(), "   ".into()));
        app.update(Msg::AddTask(id));
        assert!(app.projects[0].tasks.is_empty() && app.error.is_none());
    }
}
