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

impl ksni::Tray for Tray {
    // Without this the host gets ItemIsMenu=false and a left click calls the empty `activate`.
    const MENU_ON_ACTIVATE: bool = true;
    fn id(&self) -> String {
        "projectgui".into()
    }
    fn title(&self) -> String {
        "ProjectGui".into()
    }
    fn icon_name(&self) -> String {
        "view-list-symbolic".into()
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
}
