use std::fmt::Write;

use chrono::Utc;
use serde_json::{Value, json};

use super::color::{ACCENT, ACTIVE, BLOCKED, BOLD, DIM, DONE, MUTED, Style};
use crate::domain::model::short_id;
use crate::domain::{Project, TaskStatus, Timestamp};
use crate::service::Summary;
use crate::service::projectfile::{Action, FILE_NAME, ScanItem, SyncReport};

const BAR_WIDTH: usize = 12;
/// A project touched this recently is highlighted, as in the GUI.
const FRESH_SECS: i64 = 120;

fn pad(s: &str, shown_len: usize, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(shown_len)))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}

pub fn ago(t: Timestamp, now: Timestamp) -> String {
    let s = (now - t).num_seconds().max(0);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

fn count(p: &Project, s: TaskStatus) -> usize {
    p.tasks.iter().filter(|t| t.status == s).count()
}

/// Same order as the segment bar: done, in progress, blocked, to do.
fn counts(p: &Project) -> [usize; 4] {
    [
        count(p, TaskStatus::Done),
        count(p, TaskStatus::InProgress),
        count(p, TaskStatus::Blocked),
        count(p, TaskStatus::Todo),
    ]
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// What the bar shows, in words and in the bar's colours.
fn state_line(p: &Project, st: &Style, now: Timestamp) -> String {
    let mut parts = Vec::new();
    let (active, blocked) = (
        count(p, TaskStatus::InProgress),
        count(p, TaskStatus::Blocked),
    );
    if blocked > 0 {
        parts.push(st.tone(BLOCKED, format!("{blocked} blocked")));
    }
    if active > 0 {
        parts.push(st.tone(ACTIVE, format!("{active} in progress")));
    }
    if (now - p.updated_at).num_seconds() < FRESH_SECS {
        parts.push(st.tone(ACCENT, format!("updated {}", ago(p.updated_at, now))));
    }
    parts.join("  ")
}

fn legend(st: &Style) -> String {
    use TaskStatus::*;
    [
        (Done, "done"),
        (InProgress, "in progress"),
        (Blocked, "blocked"),
        (Todo, "to do"),
    ]
    .iter()
    .map(|&(s, label)| {
        format!(
            "{} {}",
            st.tone(Style::status_tone(s), st.cell(s).to_string()),
            st.tone(MUTED, label)
        )
    })
    .collect::<Vec<_>>()
    .join("  ")
}

/// Overview: one row per project, laid out like the GUI list (priority meter, segment bar, state in words).
pub fn status_overview(
    projects: &[Project],
    summary: &Summary,
    st: &Style,
    now: Timestamp,
) -> String {
    if projects.is_empty() {
        return "No projects yet. Create one with: projectgui add \"My project\"\n".into();
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}  {}",
        st.paint(BOLD, "ProjectGui"),
        st.tone(
            MUTED,
            format!(
                "{}, {} of {} tasks done",
                plural(summary.projects, "project"),
                summary.tasks_done,
                summary.tasks_total
            )
        )
    );
    out.push('\n');
    let name_w = projects
        .iter()
        .map(|p| p.title.chars().count())
        .max()
        .unwrap_or(0)
        .min(28);
    for p in projects {
        let pr = p.progress();
        let title = truncate(&p.title, 28);
        let stage = pad(
            &st.tone(MUTED, p.stage.as_str()),
            p.stage.as_str().len(),
            11,
        );
        let figure = if pr.total == 0 {
            "no tasks".to_string()
        } else {
            format!("{} of {}", pr.done, pr.total)
        };
        let _ = write!(
            out,
            "  {}  {}  {}  {}  {:>3}%  {}",
            pad(&st.paint(BOLD, &title), title.chars().count(), name_w),
            st.meter(p.priority),
            stage,
            st.segments(counts(p), BAR_WIDTH),
            pr.percent(),
            pad(&st.tone(MUTED, &figure), figure.len(), 8),
        );
        if p.archived_at.is_some() {
            let _ = write!(out, "  {}", st.tone(MUTED, "archived"));
        }
        let state = state_line(p, st, now);
        if !state.is_empty() {
            let _ = write!(out, "  {state}");
        }
        out.truncate(out.trim_end_matches(' ').len());
        out.push('\n');
    }
    let _ = write!(out, "\n  {}\n", legend(st));
    out
}

/// One project in detail, with tasks and references.
pub fn project_detail(p: &Project, st: &Style, now: Timestamp) -> String {
    let mut out = String::new();
    let pr = p.progress();
    let _ = writeln!(
        out,
        "{}  {}",
        st.paint(BOLD, &p.title),
        st.paint(DIM, format!("({})", p.slug))
    );
    let _ = writeln!(
        out,
        "{} {}  {}  {}",
        st.meter(p.priority),
        st.priority(p.priority),
        st.tone(MUTED, p.stage.as_str()),
        st.tone(MUTED, format!("updated {}", ago(p.updated_at, now)))
    );
    if !p.description.is_empty() {
        let _ = writeln!(out, "{}", p.description);
    }
    if let Some(path) = &p.path {
        let _ = writeln!(out, "{}", st.paint(DIM, path));
    }
    let _ = writeln!(
        out,
        "\n{} {}%  {}",
        st.segments(counts(p), 24),
        pr.percent(),
        st.tone(MUTED, format!("{} of {} done", pr.done, pr.total))
    );
    if !p.tasks.is_empty() {
        out.push('\n');
        for t in &p.tasks {
            let prio = t
                .priority
                .map(|x| format!(" {}", st.priority(x)))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "  {} {}{}  {}",
                st.task_glyph(t.status),
                t.title,
                prio,
                st.paint(DIM, short_id(&t.id))
            );
            if let Some(d) = &t.description {
                let _ = writeln!(out, "      {}", st.paint(DIM, d));
            }
        }
    }
    if !p.references.is_empty() {
        out.push('\n');
        for r in &p.references {
            let url = r
                .url
                .as_deref()
                .map(|u| format!(" {u}"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "  {} {}{}  {}",
                st.paint(DIM, format!("{:<6}", r.kind)),
                r.value,
                st.paint(DIM, url),
                st.paint(DIM, short_id(&r.id))
            );
        }
    }
    out
}

pub fn project_table(projects: &[Project], st: &Style, now: Timestamp) -> String {
    if projects.is_empty() {
        return "No projects match.\n".into();
    }
    let slug_w = projects
        .iter()
        .map(|p| p.slug.chars().count())
        .max()
        .unwrap_or(4)
        .clamp(4, 32);
    let mut out = format!(
        "{}\n",
        st.paint(
            DIM,
            format!(
                "{:<slug_w$}  {:<8}  {:<11}  {:>4}  {:<7}  UPDATED",
                "SLUG", "PRIORITY", "STAGE", "PROG", "TASKS"
            )
        )
    );
    for p in projects {
        let pr = p.progress();
        let slug = truncate(&p.slug, 32);
        let _ = writeln!(
            out,
            "{}  {}  {}  {:>3}%  {:<7}  {}",
            pad(&slug, slug.chars().count(), slug_w),
            pad(&st.priority(p.priority), p.priority.as_str().len(), 8),
            pad(p.stage.as_str(), p.stage.as_str().len(), 11),
            pr.percent(),
            format!("{}/{}", pr.done, pr.total),
            st.paint(DIM, ago(p.updated_at, now)),
        );
    }
    out
}

pub fn task_list(p: &Project, st: &Style) -> String {
    if p.tasks.is_empty() {
        return format!("{} has no tasks.\n", p.title);
    }
    p.tasks
        .iter()
        .map(|t| {
            format!(
                "{} {}  {}\n",
                st.task_glyph(t.status),
                t.title,
                st.paint(DIM, short_id(&t.id))
            )
        })
        .collect()
}

fn plural_n(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// What a sync did (or, for a dry run, would do) to one project.
pub fn sync_report(r: &SyncReport, st: &Style) -> String {
    let verb = match (r.plan.action, r.applied) {
        (Action::InSync, _) => "In sync",
        (Action::Create, true) => "Created",
        (Action::Create, false) => "Would create",
        (Action::Update, true) => "Updated",
        (Action::Update, false) => "Would update",
    };
    let tone = match r.plan.action {
        Action::InSync => MUTED,
        Action::Create => DONE,
        Action::Update => ACTIVE,
    };
    let mut out = format!("{} {}\n", st.tone(tone, verb), st.paint(BOLD, &r.title));
    out += &format!("  {}\n", st.tone(MUTED, &r.dir));
    let p = &r.plan;
    if !p.changed_fields.is_empty() {
        out += &format!("  changed: {}\n", p.changed_fields.join(", "));
    }
    if !p.tasks_to_add.is_empty() {
        out += &format!(
            "  + {}: {}\n",
            plural_n(p.tasks_to_add.len(), "task"),
            p.tasks_to_add.join(", ")
        );
    }
    if !p.tasks_to_update.is_empty() {
        out += &format!(
            "  ~ {}: {}\n",
            plural_n(p.tasks_to_update.len(), "task"),
            p.tasks_to_update.join(", ")
        );
    }
    if p.references_to_add > 0 {
        out += &format!("  + {}\n", plural_n(p.references_to_add, "reference"));
    }
    if !p.not_in_file.is_empty() {
        out += &st.tone(
            MUTED,
            format!("  kept, not in the file: {}\n", p.not_in_file.join(", ")),
        );
    }
    out
}

/// One row per file found, then a count line. `applied` is false for the default dry run.
pub fn scan_table(items: &[ScanItem], applied: bool, st: &Style) -> String {
    if items.is_empty() {
        return format!("No {FILE_NAME} files found. Create one with: projectgui init-file\n");
    }
    let width = items
        .iter()
        .map(|i| i.dir.chars().count())
        .max()
        .unwrap_or(0)
        .min(60);
    let (mut new, mut changed, mut synced, mut bad) = (0, 0, 0, 0);
    let mut out = String::new();
    for i in items {
        let (label, tone, detail) = match (&i.report, &i.error) {
            (Some(r), _) => match r.plan.action {
                Action::Create => (
                    if applied { "created" } else { "new" },
                    DONE,
                    r.title.clone(),
                ),
                Action::Update => (
                    if applied { "updated" } else { "changed" },
                    ACTIVE,
                    r.title.clone(),
                ),
                Action::InSync => ("in sync", MUTED, r.title.clone()),
            },
            (None, e) => ("invalid", BLOCKED, e.clone().unwrap_or_default()),
        };
        match (&i.report, label) {
            (None, _) => bad += 1,
            (Some(r), _) => match r.plan.action {
                Action::Create => new += 1,
                Action::Update => changed += 1,
                Action::InSync => synced += 1,
            },
        }
        let dir = truncate(&i.dir, 60);
        out += &format!(
            "  {}  {}  {}\n",
            pad(&st.tone(tone, label), label.len(), 8),
            pad(&dir, dir.chars().count(), width),
            detail
        );
    }
    out += &format!(
        "\n{} {}, {} {}, {} in sync, {} invalid.\n",
        new,
        if applied { "created" } else { "new" },
        changed,
        if applied { "updated" } else { "changed" },
        synced,
        bad
    );
    if !applied && new + changed > 0 {
        out += "Run again with --apply to load them.\n";
    }
    out
}

pub fn project_json(p: &Project) -> Value {
    let pr = p.progress();
    let mut v = serde_json::to_value(p).expect("project serializes");
    v["progress"] = json!({ "done": pr.done, "total": pr.total, "percent": pr.percent() });
    v
}

pub fn now() -> Timestamp {
    Utc::now()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::fixtures::{project, task};
    use crate::domain::{Priority, Stage};
    use crate::service::query::summarize;

    fn sample() -> Vec<Project> {
        let mut a = project("01AAAAAAAAAAAAAAAAAAAAAAAA", "web", "Website");
        a.priority = Priority::High;
        a.stage = Stage::InProgress;
        a.tasks = vec![
            task("t1", TaskStatus::Done),
            task("t2", TaskStatus::Blocked),
            task("t3", TaskStatus::Todo),
            task("t4", TaskStatus::Done),
        ];
        let mut b = project("01BBBBBBBBBBBBBBBBBBBBBBBB", "api", "API");
        b.priority = Priority::Critical;
        b.tasks = vec![task("u1", TaskStatus::Done)];
        vec![a, b]
    }

    #[test]
    fn overview_plain_has_bars_percentages_and_blocked() {
        let ps = sample();
        let out = status_overview(&ps, &summarize(&ps), &Style::plain(), now());
        assert!(!out.contains('\x1b'));
        assert!(out.contains("2 projects, 3 of 5 tasks done"), "{out}");
        assert!(
            out.contains("Website")
                && out.contains(" 50%")
                && out.contains("2 of 4")
                && out.contains("1 blocked")
                && out.contains("█ done  ▓ in progress  ▒ blocked  ░ to do"),
            "{out}"
        );
        assert!(out.contains("████████████  100%"), "{out}");
    }

    #[test]
    fn colors_only_when_enabled() {
        let ps = sample();
        let on = status_overview(
            &ps,
            &summarize(&ps),
            &Style {
                color: true,
                truecolor: false,
                ascii: false,
            },
            now(),
        );
        assert!(on.contains("\x1b[31m"));
        let rgb = Style {
            color: true,
            truecolor: true,
            ascii: false,
        };
        assert!(
            status_overview(&ps, &summarize(&ps), &rgb, now()).contains("\x1b[38;2;240;112;124m"),
            "blocked uses the GUI red in 24-bit terminals"
        );
    }

    #[test]
    fn segments_split_by_status_and_keep_every_status_visible() {
        let st = Style::plain();
        assert_eq!(st.segments([1, 1, 1, 1], 8), "██▓▓▒▒░░");
        assert_eq!(st.segments([0, 0, 0, 0], 4), "░░░░");
        assert_eq!(st.segments([1, 0, 0, 0], 4), "████");
        // one blocked task among 40 would round to zero cells, but must stay visible
        let bar = st.segments([39, 0, 1, 0], 12);
        assert_eq!(bar.chars().count(), 12);
        assert!(bar.contains('▒'), "{bar}");
        let ascii = Style {
            ascii: true,
            ..Style::plain()
        };
        assert_eq!(ascii.segments([1, 1, 1, 1], 4), "#~!-");
    }

    #[test]
    fn priority_meter_fills_one_step_per_level() {
        let plain = Style::plain();
        assert_eq!(plain.meter(Priority::Low), "▁▁▁▁");
        assert_eq!(plain.meter(Priority::Critical), "▁▃▅▇");
        let ascii = Style {
            ascii: true,
            ..Style::plain()
        };
        assert_eq!(ascii.meter(Priority::High), "***-");
    }

    #[test]
    fn fresh_updates_are_called_out_and_old_ones_are_not() {
        let mut ps = sample();
        let n = now();
        ps[0].updated_at = n - chrono::Duration::hours(2);
        ps[1].updated_at = n;
        let out = status_overview(&ps, &summarize(&ps), &Style::plain(), n);
        assert_eq!(out.matches("updated just now").count(), 1, "{out}");
        assert!(out.contains("1 blocked"), "{out}");
    }

    #[test]
    fn detail_lists_tasks_with_short_ids() {
        let ps = sample();
        let out = project_detail(&ps[0], &Style::plain(), now());
        assert!(
            out.contains("Website  (web)")
                && out.contains("● t1")
                && out.contains("✖ t2")
                && out.contains("50%"),
            "{out}"
        );
    }

    #[test]
    fn empty_states() {
        assert!(
            status_overview(&[], &summarize(&[]), &Style::plain(), now())
                .contains("No projects yet")
        );
        assert!(project_table(&[], &Style::plain(), now()).contains("No projects match"));
    }

    #[test]
    fn json_includes_derived_progress() {
        let v = project_json(&sample()[0]);
        assert_eq!(v["progress"]["percent"], 50);
        assert_eq!(v["slug"], "web");
    }

    #[test]
    fn relative_time() {
        let n = now();
        assert_eq!(ago(n, n), "just now");
        assert_eq!(ago(n - chrono::Duration::minutes(5), n), "5m ago");
        assert_eq!(ago(n - chrono::Duration::hours(30), n), "1d ago");
    }
}
