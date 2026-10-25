//! Colour palettes. One arm per name in `config::GUI_THEMES`.
use iced::Color;

/// Every colour the UI uses. Copy, so view code takes it by value.
#[derive(Clone, Copy)]
pub(super) struct Pal {
    /// Window background. Alpha below 1 lets the desktop show through.
    pub(super) bg: Color,
    pub(super) surface: Color,
    pub(super) line: Color,
    pub(super) text: Color,
    pub(super) muted: Color,
    pub(super) accent: Color,
    pub(super) done: Color,
    pub(super) active: Color,
    pub(super) blocked: Color,
    pub(super) todo: Color,
}

impl Pal {
    /// Tooltips and popups must stay legible even when the window itself is see-through.
    pub(super) fn bg_opaque(self) -> Color {
        Color { a: 1.0, ..self.bg }
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}

const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::from_rgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a)
}

/// `opacity` (percent) only affects the translucent default theme.
pub(super) fn palette(name: &str, opacity: u8) -> Pal {
    match name {
        "slate" => Pal {
            bg: rgb(21, 26, 36),
            surface: rgb(29, 36, 51),
            line: rgb(40, 50, 70),
            text: rgb(230, 234, 242),
            muted: rgb(135, 145, 167),
            accent: rgb(143, 168, 255),
            done: rgb(70, 184, 160),
            active: rgb(229, 178, 93),
            blocked: rgb(227, 107, 116),
            todo: rgb(56, 67, 89),
        },
        "paper" => Pal {
            bg: rgb(243, 245, 248),
            surface: rgb(230, 234, 241),
            line: rgb(208, 214, 225),
            text: rgb(27, 34, 48),
            muted: rgb(97, 107, 128),
            accent: rgb(48, 84, 214),
            done: rgb(24, 140, 116),
            active: rgb(184, 120, 16),
            blocked: rgb(198, 56, 72),
            todo: rgb(203, 209, 221),
        },
        "dusk" => Pal {
            bg: rgb(31, 26, 46),
            surface: rgb(42, 35, 63),
            line: rgb(60, 51, 88),
            text: rgb(236, 231, 247),
            muted: rgb(150, 141, 175),
            accent: rgb(201, 160, 255),
            done: rgb(94, 204, 170),
            active: rgb(245, 190, 104),
            blocked: rgb(240, 112, 128),
            todo: rgb(70, 60, 100),
        },
        "contrast" => Pal {
            bg: rgb(0, 0, 0),
            surface: rgb(28, 28, 28),
            line: rgb(110, 110, 110),
            text: rgb(255, 255, 255),
            muted: rgb(200, 200, 200),
            accent: rgb(255, 214, 10),
            done: rgb(0, 230, 118),
            active: rgb(255, 171, 64),
            blocked: rgb(255, 82, 82),
            todo: rgb(90, 90, 90),
        },
        // frosted (default): translucent so the desktop shows through; accent is the Pop teal.
        _ => Pal {
            bg: rgba(18, 22, 32, f32::from(opacity) / 100.0),
            surface: rgba(255, 255, 255, 0.07),
            line: rgba(255, 255, 255, 0.12),
            text: rgb(238, 243, 250),
            muted: rgb(166, 176, 194),
            accent: rgb(99, 208, 223),
            done: rgb(99, 220, 170),
            active: rgb(240, 192, 100),
            blocked: rgb(240, 112, 124),
            todo: rgba(255, 255, 255, 0.16),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GUI_THEMES;

    #[test]
    fn every_theme_name_has_its_own_palette() {
        let frosted = palette("frosted", 80).bg;
        for &name in GUI_THEMES {
            // Anything unknown falls back to frosted, so a missing arm shows up as a non-frosted name sharing its bg.
            let same = palette(name, 80).bg == frosted;
            assert_eq!(same, name == "frosted", "{name}");
        }
    }
}
