use std::io::IsTerminal;

use crate::domain::{Priority, TaskStatus};

/// Same hues as the GUI's default theme. `ansi` is the fallback for terminals without 24-bit colour.
/// shortcut: one dark-terminal palette, the GUI's light themes are not mirrored; add a `cli_theme` setting if needed.
#[derive(Debug, Clone, Copy)]
pub struct Tone {
    rgb: (u8, u8, u8),
    ansi: &'static str,
}

pub const DONE: Tone = Tone {
    rgb: (99, 220, 170),
    ansi: "32",
};
pub const ACTIVE: Tone = Tone {
    rgb: (240, 192, 100),
    ansi: "33",
};
pub const BLOCKED: Tone = Tone {
    rgb: (240, 112, 124),
    ansi: "31",
};
pub const ACCENT: Tone = Tone {
    rgb: (99, 208, 223),
    ansi: "36",
};
pub const MUTED: Tone = Tone {
    rgb: (140, 150, 168),
    ansi: "2",
};

pub const BOLD: &str = "1";
pub const DIM: &str = "2";

#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub color: bool,
    pub truecolor: bool,
    pub ascii: bool,
}

impl Style {
    #[cfg(any(feature = "mcp", test))]
    pub fn plain() -> Self {
        Self {
            color: false,
            truecolor: false,
            ascii: false,
        }
    }

    /// Colors need a terminal on stdout, `NO_COLOR` unset and neither `--no-color` nor `color = "never"`.
    pub fn detect(no_color_flag: bool, color_setting: &str, ascii: bool) -> Self {
        let color = match color_setting {
            "never" => false,
            "always" => !no_color_flag,
            _ => {
                !no_color_flag
                    && std::env::var_os("NO_COLOR").is_none()
                    && std::io::stdout().is_terminal()
            }
        };
        let truecolor =
            color && std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit");
        Self {
            color,
            truecolor,
            ascii,
        }
    }

    pub fn paint(&self, code: &str, text: impl AsRef<str>) -> String {
        if self.color && !text.as_ref().is_empty() {
            format!("\x1b[{code}m{}\x1b[0m", text.as_ref())
        } else {
            text.as_ref().to_string()
        }
    }

    pub fn tone(&self, t: Tone, text: impl AsRef<str>) -> String {
        if self.truecolor {
            let (r, g, b) = t.rgb;
            self.paint(&format!("38;2;{r};{g};{b}"), text)
        } else {
            self.paint(t.ansi, text)
        }
    }

    pub fn status_tone(s: TaskStatus) -> Tone {
        match s {
            TaskStatus::Done => DONE,
            TaskStatus::InProgress => ACTIVE,
            TaskStatus::Blocked => BLOCKED,
            TaskStatus::Todo => MUTED,
        }
    }

    pub fn priority(&self, p: Priority) -> String {
        match p {
            Priority::Critical | Priority::High => self.tone(ACCENT, p.as_str()),
            Priority::Medium => p.as_str().to_string(),
            Priority::Low => self.tone(MUTED, p.as_str()),
        }
    }

    /// Four rising steps, filled count = importance. Colour stays reserved for task status, as in the GUI.
    pub fn meter(&self, p: Priority) -> String {
        let filled = p as usize + 1;
        let steps = if self.ascii {
            ['*'; 4]
        } else {
            ['▁', '▃', '▅', '▇']
        };
        let off = if self.ascii { '-' } else { '▁' };
        let on: String = steps[..filled].iter().collect();
        let rest: String = std::iter::repeat_n(off, 4 - filled).collect();
        format!("{}{}", self.tone(ACCENT, on), self.tone(MUTED, rest))
    }

    pub fn task_glyph(&self, s: TaskStatus) -> String {
        let glyph = match (s, self.ascii) {
            (TaskStatus::Todo, false) => "○",
            (TaskStatus::InProgress, false) => "◐",
            (TaskStatus::Blocked, false) => "✖",
            (TaskStatus::Done, false) => "●",
            (TaskStatus::Todo, true) => "[ ]",
            (TaskStatus::InProgress, true) => "[~]",
            (TaskStatus::Blocked, true) => "[!]",
            (TaskStatus::Done, true) => "[x]",
        };
        self.tone(Self::status_tone(s), glyph)
    }

    /// One cell of the segmented bar. Density differs per status so the bar still reads without colour.
    pub fn cell(&self, s: TaskStatus) -> char {
        match (s, self.ascii) {
            (TaskStatus::Done, false) => '█',
            (TaskStatus::InProgress, false) => '▓',
            (TaskStatus::Blocked, false) => '▒',
            (TaskStatus::Todo, false) => '░',
            (TaskStatus::Done, true) => '#',
            (TaskStatus::InProgress, true) => '~',
            (TaskStatus::Blocked, true) => '!',
            (TaskStatus::Todo, true) => '-',
        }
    }

    /// The GUI's segment bar: `width` cells split between done, in progress, blocked and to do, in that order.
    /// Every status that has tasks keeps at least one cell, so a lone blocked task is never hidden.
    pub fn segments(&self, counts: [usize; 4], width: usize) -> String {
        const ORDER: [TaskStatus; 4] = [
            TaskStatus::Done,
            TaskStatus::InProgress,
            TaskStatus::Blocked,
            TaskStatus::Todo,
        ];
        let total = std::num::NonZeroUsize::new(counts.iter().sum::<usize>());
        let mut cells = [0usize; 4];
        if let Some(total) = total.map(|t| t.get()) {
            // largest-remainder split, then guarantee one cell per non-empty status
            let mut rem = [0usize; 4];
            for i in 0..4 {
                cells[i] = counts[i] * width / total;
                rem[i] = counts[i] * width % total;
            }
            let left = width - cells.iter().sum::<usize>();
            let mut by_rem: Vec<usize> = (0..4).collect();
            by_rem.sort_by_key(|&i| std::cmp::Reverse(rem[i]));
            for &i in by_rem.iter().cycle().take(left) {
                cells[i] += 1;
            }
            for i in 0..4 {
                if counts[i] > 0 && cells[i] == 0 {
                    let big = (0..4).max_by_key(|&j| cells[j]).unwrap_or(i);
                    if cells[big] > 1 {
                        cells[big] -= 1;
                        cells[i] = 1;
                    }
                }
            }
        } else {
            cells[3] = width;
        }
        ORDER
            .iter()
            .zip(cells)
            .filter(|(_, n)| *n > 0)
            .map(|(&s, n)| self.tone(Self::status_tone(s), self.cell(s).to_string().repeat(n)))
            .collect()
    }
}
