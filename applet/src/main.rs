// SPDX-License-Identifier: MIT

mod app;
mod projectgui;

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<app::AppModel>(())
}
