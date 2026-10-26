// SPDX-License-Identifier: MIT

use std::time::Duration;

use cosmic::Theme;
use cosmic::app;
use cosmic::iced::platform_specific::shell::wayland::commands::popup::destroy_popup;
use cosmic::iced::window::Id;
use cosmic::iced::{Alignment, Border, Color, Length, Subscription};
use cosmic::prelude::*;
use cosmic::surface;
use cosmic::widget::{self, container};

use crate::pgui::{self, Project};

const ICON_SVG: &[u8] = include_bytes!("../../assets/io.github.Gu1L3RM3.ProjectGui-symbolic.svg");

/// Changes from the CLI, agents or the window show up within this delay; one `list` call is a few milliseconds.
const REFRESH: Duration = Duration::from_secs(5);

// The window's "slate" palette, so the bars mean the same thing in the panel and in the app.
const DONE: Color = Color::from_rgb(70.0 / 255.0, 184.0 / 255.0, 160.0 / 255.0);
const ACTIVE: Color = Color::from_rgb(229.0 / 255.0, 178.0 / 255.0, 93.0 / 255.0);
const BLOCKED: Color = Color::from_rgb(227.0 / 255.0, 107.0 / 255.0, 116.0 / 255.0);

#[derive(Default)]
pub struct AppModel {
    core: cosmic::Core,
    popup: Option<Id>,
    projects: Option<Vec<Project>>,
    error: Option<String>,
    loading: bool,
    /// Project whose tasks are shown in the popup.
    expanded: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    PopupClosed(Id),
    Refresh,
    Fetched(Result<Vec<Project>, String>),
    Expand(String),
    SetDone {
        slug: String,
        task_id: String,
        done: bool,
    },
    Changed(Result<(), String>),
    OpenGui,
}

impl AppModel {
    fn fetch(&mut self) -> app::Task<Message> {
        self.loading = true;
        cosmic::Task::perform(pgui::fetch(), |r| {
            cosmic::action::Action::App(Message::Fetched(r))
        })
    }
}

fn segment<'a>(portion: u16, color: Color) -> Element<'a, Message> {
    widget::container(widget::space())
        .width(Length::FillPortion(portion))
        .height(Length::Fixed(6.0))
        .class(cosmic::theme::Container::custom(move |_t: &Theme| {
            container::Style {
                background: Some(color.into()),
                border: Border {
                    radius: 3.0.into(),
                    ..Default::default()
                },
                ..container::Style::default()
            }
        }))
        .into()
}

/// Done, in-progress and blocked shares of a project's tasks; `FillPortion(0)` is degenerate in iced, so empty parts
/// are skipped.
fn meter<'a>(p: &Project) -> Element<'a, Message> {
    let total = p.progress.total.max(1) as u16;
    let done = p.progress.done as u16;
    let active = (p.count("in-progress") as u16).min(total.saturating_sub(done));
    let blocked = (p.count("blocked") as u16).min(total.saturating_sub(done + active));
    let rest = total.saturating_sub(done + active + blocked);
    let mut bar = widget::Row::new().height(Length::Fixed(6.0)).spacing(2);
    for (portion, color) in [
        (done, DONE),
        (active, ACTIVE),
        (blocked, BLOCKED),
        (rest, Color::from_rgba(1.0, 1.0, 1.0, 0.12)),
    ] {
        if portion > 0 {
            bar = bar.push(segment(portion, color));
        }
    }
    bar.into()
}

fn project_view<'a>(p: &Project, expanded: bool) -> Element<'a, Message> {
    let header = widget::Row::new()
        .push(widget::text::heading(p.title.clone()))
        .push(widget::space().width(Length::Fill))
        .push(widget::text::body(format!("{}%", p.progress.percent)))
        .align_y(Alignment::Center);

    let mut detail = format!(
        "{} · {} · {} of {} done",
        p.priority, p.stage, p.progress.done, p.progress.total
    );
    for (status, label) in [("blocked", "blocked"), ("in-progress", "in progress")] {
        let n = p.count(status);
        if n > 0 {
            detail.push_str(&format!(" · {n} {label}"));
        }
    }

    let mut column = widget::Column::new()
        .push(header)
        .push(meter(p))
        .push(widget::text::caption(detail))
        .spacing(6);

    if expanded {
        for t in &p.tasks {
            let (slug, task_id) = (p.slug.clone(), t.id.clone());
            let label = match t.status.as_str() {
                "in-progress" => format!("{}  (in progress)", t.title),
                "blocked" => format!("{}  (blocked)", t.title),
                _ => t.title.clone(),
            };
            column = column.push(widget::checkbox(t.status == "done").label(label).on_toggle(
                move |done| Message::SetDone {
                    slug: slug.clone(),
                    task_id: task_id.clone(),
                    done,
                },
            ));
        }
        if p.tasks.is_empty() {
            column = column.push(widget::text::caption("No tasks yet."));
        }
    }

    widget::button::custom(column)
        .on_press(Message::Expand(p.id.clone()))
        .class(cosmic::theme::Button::MenuItem)
        .width(Length::Fill)
        .padding(8)
        .into()
}

impl cosmic::Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = "io.github.Gu1L3RM3.ProjectGui.Applet";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, _flags: Self::Flags) -> (Self, app::Task<Message>) {
        let mut app = AppModel {
            core,
            ..Default::default()
        };
        let task = app.fetch();
        (app, task)
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn view(&self) -> Element<'_, Message> {
        let icon = widget::icon(widget::icon::from_svg_bytes(ICON_SVG).symbolic(true))
            .width(Length::Fixed(16.0))
            .height(Length::Fixed(16.0));
        let mut row = widget::Row::new()
            .push(icon)
            .align_y(Alignment::Center)
            .spacing(6);
        if self.error.is_some() && self.projects.is_none() {
            row = row.push(widget::text("!").size(14.0));
        } else if let Some(pct) = self.projects.as_deref().and_then(pgui::overall) {
            row = row.push(widget::text(format!("{pct}%")).size(14.0));
        }
        let content = widget::container(row)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .padding([0, 4]);
        widget::button::custom(self.core.applet.autosize_window(content))
            .on_press(Message::TogglePopup)
            .class(cosmic::theme::Button::AppletIcon)
            .into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Message> {
        let mut content = widget::Column::new().spacing(12).padding(16);

        if let Some(error) = &self.error {
            content = content.push(widget::text::caption(error.clone()).class(
                cosmic::theme::Text::Color(Color::from_rgb(
                    227.0 / 255.0,
                    107.0 / 255.0,
                    116.0 / 255.0,
                )),
            ));
        }
        match &self.projects {
            Some(projects) if projects.is_empty() => {
                content = content
                    .push(widget::text::heading("No open projects"))
                    .push(widget::text::caption(
                        "Create one with `pgui add` or in the window.",
                    ));
            }
            Some(projects) => {
                let done: u32 = projects.iter().map(|p| p.progress.done).sum();
                let total: u32 = projects.iter().map(|p| p.progress.total).sum();
                content = content.push(widget::text::caption(format!(
                    "{} open projects · {done} of {total} tasks done",
                    projects.len()
                )));
                let mut list = widget::Column::new().spacing(4);
                for p in projects {
                    list = list.push(project_view(
                        p,
                        self.expanded.as_deref() == Some(p.id.as_str()),
                    ));
                }
                content = content.push(widget::scrollable(list).height(Length::Shrink));
            }
            None if self.error.is_none() => content = content.push(widget::text::body("Loading…")),
            None => {}
        }

        content = content.push(
            widget::Row::new()
                .push(
                    // A fixed label: the background refresh every few seconds made a changing one flicker.
                    // `Refresh` is already ignored while a fetch runs.
                    widget::button::standard("Refresh")
                        .on_press(Message::Refresh)
                        .width(Length::Fill),
                )
                .push(
                    widget::button::suggested("Open ProjectGui")
                        .on_press(Message::OpenGui)
                        .width(Length::Fill),
                )
                .spacing(8),
        );

        self.core.applet.popup_container(content).into()
    }

    fn subscription(&self) -> Subscription<Message> {
        struct RefreshTick;
        Subscription::run_with(std::any::TypeId::of::<RefreshTick>(), |_| {
            cosmic::iced::stream::channel::<Message>(1, async |mut channel| {
                use cosmic::iced::futures::SinkExt;
                loop {
                    tokio::time::sleep(REFRESH).await;
                    _ = channel.send(Message::Refresh).await;
                }
            })
        })
    }

    fn update(&mut self, message: Message) -> app::Task<Message> {
        match message {
            Message::Refresh => {
                if !self.loading {
                    return self.fetch();
                }
            }
            Message::Fetched(result) => {
                self.loading = false;
                match result {
                    Ok(all) => {
                        self.projects = Some(pgui::open(all));
                        self.error = None;
                    }
                    // Keep the last good list on screen.
                    Err(e) => self.error = Some(e),
                }
            }
            Message::Expand(id) => {
                self.expanded = (self.expanded.as_deref() != Some(id.as_str())).then_some(id);
            }
            Message::SetDone {
                slug,
                task_id,
                done,
            } => {
                return cosmic::Task::perform(pgui::set_done(slug, task_id, done), |r| {
                    cosmic::action::Action::App(Message::Changed(r))
                });
            }
            Message::Changed(result) => {
                if let Err(e) = result {
                    self.error = Some(e);
                }
                return self.fetch();
            }
            Message::OpenGui => {
                pgui::open_gui();
                if let Some(p) = self.popup.take() {
                    return destroy_popup(p);
                }
            }
            Message::TogglePopup => {
                if let Some(p) = self.popup.take() {
                    return destroy_popup(p);
                }
                let refresh = self.fetch();
                let open = surface::surface_task(surface::action::app_popup(
                    |_| Default::default(),
                    |app: &mut Self| {
                        let new_id = Id::unique();
                        app.popup = Some(new_id);
                        let mut settings = app.core.applet.get_popup_settings(
                            app.core.main_window_id().unwrap(),
                            new_id,
                            None,
                            None,
                            None,
                        );
                        settings.positioner.size_limits = cosmic::iced::Limits::NONE
                            .max_width(420.0)
                            .min_width(340.0)
                            .min_height(120.0)
                            .max_height(640.0);
                        settings
                    },
                    None,
                ));
                return cosmic::Task::batch([open, refresh]);
            }
            Message::PopupClosed(id) => {
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                }
            }
        }
        cosmic::Task::none()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}
