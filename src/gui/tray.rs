//! Status-area icon (a StatusNotifierItem over D-Bus, via ksni) and the single-instance guard. Both only send
//! `Event`s to the window; the window owns all state and pushes menu contents back through `Handle::show`.
use std::io::ErrorKind;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use iced::futures::channel::mpsc::UnboundedSender;
use ksni::blocking::TrayMethods;
use ksni::menu::{MenuItem, StandardItem};

#[derive(Debug, Clone)]
pub enum Event {
    /// Bring the window forward; `Some(id)` also expands that project.
    Open(Option<String>),
    Quit,
}

/// One menu row: a project id and its label.
pub type Row = (String, String);

struct Tray {
    tx: UnboundedSender<Event>,
    rows: Vec<Row>,
    update: Option<String>,
}

impl Tray {
    fn item(label: String, ev: Event) -> MenuItem<Self> {
        StandardItem {
            label,
            activate: Box::new(move |t: &mut Self| {
                let _ = t.tx.unbounded_send(ev.clone());
            }),
            ..Default::default()
        }
        .into()
    }
}

/// The app icon's three progress bars (assets/io.github.Gu1L3RM3.ProjectGui.svg, without the background), drawn in code
/// so the tray shows it even when no icon files are installed. ARGB32, network byte order, as StatusNotifierItem wants.
fn icon(size: i32) -> ksni::Icon {
    const TODO: [u8; 3] = [140, 150, 168];
    const ACTIVE: [u8; 3] = [229, 178, 93];
    const DONE: [u8; 3] = [70, 184, 160];
    // In the SVG's 128-unit box every bar starts at x 26 and is 14 high; strokes as (center y, right end, color), later
    // ones painted over earlier ones.
    const STROKES: [(f32, f32, [u8; 3]); 7] = [
        (39.0, 102.0, TODO),
        (39.0, 102.0, DONE),
        (64.0, 102.0, TODO),
        (64.0, 78.0, ACTIVE),
        (64.0, 62.0, DONE),
        (89.0, 102.0, TODO),
        (89.0, 48.0, DONE),
    ];
    let (left, r) = (26.0, 7.0);
    let color_at = |x: f32, y: f32| {
        let mut hit = None;
        for (cy, right, color) in STROKES {
            let dx = x - x.clamp(left + r, right - r);
            if dx * dx + (y - cy) * (y - cy) <= r * r {
                hit = Some(color);
            }
        }
        hit
    };
    // 4x4 samples per pixel smooth the rounded ends.
    const N: usize = 4;
    // Only the 104-unit square around the bars is drawn: the SVG's margin would make them tiny at tray size.
    let (origin, unit) = (12.0, 104.0 / size as f32);
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut sum, mut hits) = ([0u32; 3], 0u32);
            for s in 0..N * N {
                let x = origin + (px as f32 + ((s % N) as f32 + 0.5) / N as f32) * unit;
                let y = origin + (py as f32 + ((s / N) as f32 + 0.5) / N as f32) * unit;
                if let Some(c) = color_at(x, y) {
                    (0..3).for_each(|k| sum[k] += c[k] as u32);
                    hits += 1;
                }
            }
            let avg = |k: usize| sum[k].checked_div(hits).unwrap_or(0) as u8;
            data.extend([(hits * 255 / (N * N) as u32) as u8, avg(0), avg(1), avg(2)]);
        }
    }
    ksni::Icon {
        width: size,
        height: size,
        data,
    }
}

impl ksni::Tray for Tray {
    // Without this the host gets ItemIsMenu=false and a left click calls the empty `activate`.
    const MENU_ON_ACTIVATE: bool = true;
    fn id(&self) -> String {
        "projectgui".into()
    }
    fn title(&self) -> String {
        "ProjectGui".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![icon(32), icon(64)]
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<_> = self
            .rows
            .iter()
            .map(|(id, label)| Self::item(label.clone(), Event::Open(Some(id.clone()))))
            .collect();
        if items.is_empty() {
            items.push(
                StandardItem {
                    label: "No open projects".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);
        if let Some(u) = &self.update {
            items.push(Self::item(u.clone(), Event::Open(None)));
        }
        items.push(Self::item("Open ProjectGui".into(), Event::Open(None)));
        items.push(Self::item("Quit".into(), Event::Quit));
        items
    }
}

pub struct Handle(ksni::blocking::Handle<Tray>);

impl Handle {
    pub fn show(&self, rows: Vec<Row>, update: Option<String>) {
        self.0.update(|t| {
            t.rows = rows;
            t.update = update;
        });
    }
}

/// `None` when the desktop has no status area (no StatusNotifierWatcher); the window works without it.
pub fn spawn(tx: UnboundedSender<Event>) -> Option<Handle> {
    let tray = Tray {
        tx,
        rows: Vec::new(),
        update: None,
    };
    match tray.spawn() {
        Ok(h) => Some(Handle(h)),
        Err(e) => {
            eprintln!("note: no tray icon ({e})");
            None
        }
    }
}

/// One window per database, so a test database never meets the real one. Named by a hash of the database path
/// because a socket path is capped at about 100 bytes; same binary on both sides, so the hash matches.
pub fn socket_path(db: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    db.hash(&mut h);
    dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("projectgui-{:016x}.sock", h.finish()))
}

/// Becomes the running instance (listening on `path`) or, if one already runs, asks it to come forward and
/// returns false. Any other socket error only costs the guard: the window still opens.
pub fn single_instance(path: &Path, tx: UnboundedSender<Event>) -> bool {
    if UnixStream::connect(path).is_ok() {
        return false; // the running instance treats any connection as "open"
    }
    // Nobody answered, so a socket file left here is stale (a crash or a kill).
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != ErrorKind::NotFound
    {
        eprintln!("note: single-instance guard off ({e})");
        return true;
    }
    match UnixListener::bind(path) {
        Ok(listener) => {
            std::thread::spawn(move || {
                for _ in listener.incoming().flatten() {
                    if tx.unbounded_send(Event::Open(None)).is_err() {
                        break;
                    }
                }
            });
        }
        Err(e) => eprintln!("note: single-instance guard off ({e})"),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::channel::mpsc;

    #[test]
    fn second_instance_signals_the_first_and_a_stale_socket_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("a".repeat(150)).join("pg.db");
        assert_ne!(socket_path(&db), socket_path(&dir.path().join("other.db")));
        // A real socket path stays short whatever the database path; this test uses its own directory.
        assert!(socket_path(&db).as_os_str().len() < 100);
        let sock = dir.path().join("t.sock");
        std::fs::write(&sock, "").unwrap(); // stale leftover

        let (tx, mut rx) = mpsc::unbounded();
        assert!(single_instance(&sock, tx.clone()), "first instance");
        assert!(!single_instance(&sock, tx), "second instance defers");
        let ev = iced::futures::executor::block_on(iced::futures::StreamExt::next(&mut rx));
        assert!(matches!(ev, Some(Event::Open(None))));
    }

    #[test]
    fn icon_draws_three_bars_on_a_transparent_background() {
        let px = |i: &ksni::Icon, x: i32, y: i32| {
            let o = ((y * i.width + x) * 4) as usize;
            [i.data[o], i.data[o + 1], i.data[o + 2], i.data[o + 3]]
        };
        let i = icon(32);
        assert_eq!((i.width, i.height, i.data.len()), (32, 32, 32 * 32 * 4));
        assert_eq!(px(&i, 0, 0)[0], 0, "corner is transparent");
        // Middle of the first (finished) bar is opaque "done" green, ARGB order.
        assert_eq!(px(&i, 16, 9), [255, 70, 184, 160]);
        // Right end of the last bar is the grey "to do" track.
        assert_eq!(px(&i, 22, 22), [255, 140, 150, 168]);
    }
}
