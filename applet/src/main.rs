// SPDX-License-Identifier: MIT

mod app;
mod pgui;

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<app::AppModel>(())
}
