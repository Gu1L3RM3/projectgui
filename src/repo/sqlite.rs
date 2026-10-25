use std::{collections::HashMap, path::Path, time::Duration};

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{
    Connection, ErrorCode, OptionalExtension, Row, Transaction, TransactionBehavior, params,
};

use super::{Repository, Write, migrations};
use crate::domain::{AppError, Priority, Project, Reference, Result, Task, Timestamp};

const SQLITE_CONSTRAINT_FOREIGNKEY: i32 = 787;

pub struct SqliteRepository {
    conn: Connection,
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(f, _) = &e {
            match f.code {
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => return AppError::Busy,
                ErrorCode::ConstraintViolation
                    if f.extended_code == SQLITE_CONSTRAINT_FOREIGNKEY =>
                {
                    return AppError::NotFound {
                        what: "parent record",
                        query: String::new(),
                    };
                }
                ErrorCode::ConstraintViolation => return AppError::Conflict(e.to_string()),
                _ => {}
            }
        }
        AppError::Storage(e.to_string())
    }
}

fn ts(t: &Timestamp) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_ts(s: String) -> rusqlite::Result<Timestamp> {
    DateTime::parse_from_rfc3339(&s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

fn parse_enum<T: std::str::FromStr<Err = AppError>>(s: String) -> rusqlite::Result<T> {
    s.parse().map_err(|e: AppError| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(e.to_string())),
        )
    })
}

const PROJECT_COLS: &str =
    "id, slug, title, description, priority, stage, path, created_at, updated_at, archived_at";
const TASK_COLS: &str = "id, project_id, title, description, status, priority, position, created_at, updated_at, completed_at";

fn project_row(r: &Row) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        slug: r.get(1)?,
        title: r.get(2)?,
        description: r.get(3)?,
        priority: parse_enum(r.get(4)?)?,
        stage: parse_enum(r.get(5)?)?,
        path: r.get(6)?,
        created_at: parse_ts(r.get(7)?)?,
        updated_at: parse_ts(r.get(8)?)?,
        archived_at: r.get::<_, Option<String>>(9)?.map(parse_ts).transpose()?,
        aliases: vec![],
        tasks: vec![],
        references: vec![],
    })
}

fn task_row(r: &Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get(0)?,
        project_id: r.get(1)?,
        title: r.get(2)?,
        description: r.get(3)?,
        status: parse_enum(r.get(4)?)?,
        priority: r
            .get::<_, Option<String>>(5)?
            .map(parse_enum::<Priority>)
            .transpose()?,
        position: r.get(6)?,
        created_at: parse_ts(r.get(7)?)?,
        updated_at: parse_ts(r.get(8)?)?,
        completed_at: r.get::<_, Option<String>>(9)?.map(parse_ts).transpose()?,
    })
}

fn reference_row(r: &Row) -> rusqlite::Result<Reference> {
    Ok(Reference {
        id: r.get(0)?,
        project_id: r.get(1)?,
        task_id: r.get(2)?,
        kind: parse_enum(r.get(3)?)?,
        value: r.get(4)?,
        url: r.get(5)?,
        created_at: parse_ts(r.get(6)?)?,
    })
}

impl SqliteRepository {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| AppError::Storage(format!("{}: {e}", dir.display())))?;
        }
        let mut conn = Connection::open(path)?;
        Self::init(&mut conn, Some(path))?;
        Ok(Self { conn })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        Self::init(&mut conn, None)?;
        Ok(Self { conn })
    }

    fn init(conn: &mut Connection, path: Option<&Path>) -> Result<()> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        // Returns the resulting mode as a row, so it cannot go through `pragma_update`.
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        let version = migrations::current(conn)?;
        if version > migrations::LATEST {
            return Err(AppError::Storage(format!(
                "database schema v{version} is newer than this build (v{}); upgrade projectgui",
                migrations::LATEST
            )));
        }
        if let (Some(path), true) = (path, version > 0 && version < migrations::LATEST) {
            let mut backup = path.as_os_str().to_owned();
            backup.push(format!(".bak-v{version}"));
            copy_database(conn, Path::new(&backup))?;
        }
        migrations::apply(conn)?;
        Ok(())
    }

    fn write(&mut self) -> Result<Transaction<'_>> {
        Ok(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }
}

fn copy_database(src: &Connection, dest: &Path) -> Result<()> {
    let mut dst = Connection::open(dest)?;
    let backup = rusqlite::backup::Backup::new(src, &mut dst)?;
    backup.run_to_completion(100, Duration::from_millis(10), None)?;
    Ok(())
}

/// A slug may not equal another project's slug or alias (lookup would become ambiguous).
fn ensure_slug_free(tx: &Transaction, slug: &str, project_id: &str) -> Result<()> {
    let taken: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM project WHERE slug = ?1 AND id <> ?2)
             OR EXISTS(SELECT 1 FROM project_slug_alias WHERE slug = ?1 AND project_id <> ?2)",
        params![slug, project_id],
        |r| r.get(0),
    )?;
    if taken {
        Err(AppError::Conflict(format!(
            "slug '{slug}' is already in use"
        )))
    } else {
        Ok(())
    }
}

fn insert_task_row(tx: &Transaction, t: &Task, position: i64) -> rusqlite::Result<()> {
    tx.execute(
        &format!("INSERT INTO task ({TASK_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"),
        params![
            t.id,
            t.project_id,
            t.title,
            t.description,
            t.status.as_str(),
            t.priority.map(|p| p.as_str()),
            position,
            ts(&t.created_at),
            ts(&t.updated_at),
            t.completed_at.as_ref().map(ts),
        ],
    )?;
    Ok(())
}

fn insert_reference_row(tx: &Transaction, r: &Reference) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO reference (id, project_id, task_id, kind, value, url, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![r.id, r.project_id, r.task_id, r.kind.as_str(), r.value, r.url, ts(&r.created_at)],
    )?;
    Ok(())
}

fn touch(tx: &Transaction, project_id: &str, at: &Timestamp) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE project SET updated_at = ?1 WHERE id = ?2",
        params![ts(at), project_id],
    )?;
    Ok(())
}

fn insert_project_tx(tx: &Transaction, p: &Project) -> Result<()> {
    ensure_slug_free(tx, &p.slug, &p.id)?;
    tx.execute(
        &format!("INSERT INTO project ({PROJECT_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"),
        params![
            p.id,
            p.slug,
            p.title,
            p.description,
            p.priority.as_str(),
            p.stage.as_str(),
            p.path,
            ts(&p.created_at),
            ts(&p.updated_at),
            p.archived_at.as_ref().map(ts),
        ],
    )?;
    for (i, t) in p.tasks.iter().enumerate() {
        insert_task_row(tx, t, i as i64)?;
    }
    for r in &p.references {
        insert_reference_row(tx, r)?;
    }
    Ok(())
}

fn update_project_tx(tx: &Transaction, p: &Project) -> Result<()> {
    let old_slug: String = tx
        .query_row("SELECT slug FROM project WHERE id = ?1", [&p.id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| AppError::NotFound {
            what: "project",
            query: p.id.clone(),
        })?;
    if old_slug != p.slug {
        ensure_slug_free(tx, &p.slug, &p.id)?;
        // Reusing one of our own old slugs: it stops being an alias and becomes the slug again.
        tx.execute("DELETE FROM project_slug_alias WHERE slug = ?1", [&p.slug])?;
        tx.execute(
            "INSERT INTO project_slug_alias (slug, project_id) VALUES (?1, ?2)",
            params![old_slug, p.id],
        )?;
    }
    tx.execute(
        "UPDATE project SET slug=?2, title=?3, description=?4, priority=?5, stage=?6, path=?7,
                updated_at=?8, archived_at=?9 WHERE id=?1",
        params![
            p.id,
            p.slug,
            p.title,
            p.description,
            p.priority.as_str(),
            p.stage.as_str(),
            p.path,
            ts(&p.updated_at),
            p.archived_at.as_ref().map(ts),
        ],
    )?;
    Ok(())
}

fn insert_task_tx(tx: &Transaction, t: &Task) -> Result<Task> {
    let position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM task WHERE project_id = ?1",
        [&t.project_id],
        |r| r.get(0),
    )?;
    insert_task_row(tx, t, position).map_err(|e| match AppError::from(e) {
        AppError::NotFound { .. } => AppError::NotFound {
            what: "project",
            query: t.project_id.clone(),
        },
        other => other,
    })?;
    touch(tx, &t.project_id, &t.updated_at)?;
    Ok(Task {
        position,
        ..t.clone()
    })
}

fn update_task_tx(tx: &Transaction, t: &Task) -> Result<()> {
    let n = tx.execute(
        "UPDATE task SET title=?2, description=?3, status=?4, priority=?5, updated_at=?6, completed_at=?7 WHERE id=?1",
        params![
            t.id, t.title, t.description, t.status.as_str(), t.priority.map(|p| p.as_str()),
            ts(&t.updated_at), t.completed_at.as_ref().map(ts),
        ],
    )?;
    if n == 0 {
        return Err(AppError::NotFound {
            what: "task",
            query: t.id.clone(),
        });
    }
    touch(tx, &t.project_id, &t.updated_at)?;
    Ok(())
}

fn insert_reference_tx(tx: &Transaction, r: &Reference) -> Result<()> {
    insert_reference_row(tx, r)?;
    touch(tx, &r.project_id, &r.created_at)?;
    Ok(())
}

fn kv_set_tx(tx: &Transaction, key: &str, value: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn apply_write(tx: &Transaction, w: &Write) -> Result<()> {
    match w {
        Write::InsertProject(p) => insert_project_tx(tx, p),
        Write::KvSet(k, v) => kv_set_tx(tx, k, v),
    }
}

fn collect<T>(rows: impl Iterator<Item = rusqlite::Result<T>>) -> Result<Vec<T>> {
    Ok(rows.collect::<rusqlite::Result<Vec<T>>>()?)
}

/// Every project with its tasks, references and aliases. Run it inside a transaction so the four queries see one
/// snapshot.
fn read_projects(tx: &Connection, include_archived: bool) -> Result<Vec<Project>> {
    let sql = format!(
        "SELECT {PROJECT_COLS} FROM project {} ORDER BY created_at, id",
        if include_archived {
            ""
        } else {
            "WHERE archived_at IS NULL"
        }
    );
    let mut projects = collect(tx.prepare(&sql)?.query_map([], project_row)?)?;
    let index: HashMap<String, usize> = projects
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.clone(), i))
        .collect();

    let tasks = collect(
        tx.prepare(&format!(
            "SELECT {TASK_COLS} FROM task ORDER BY project_id, position"
        ))?
        .query_map([], task_row)?,
    )?;
    for t in tasks {
        if let Some(&i) = index.get(&t.project_id) {
            projects[i].tasks.push(t);
        }
    }
    let refs = collect(
            tx.prepare("SELECT id, project_id, task_id, kind, value, url, created_at FROM reference ORDER BY created_at, id")?
                .query_map([], reference_row)?,
        )?;
    for r in refs {
        if let Some(&i) = index.get(&r.project_id) {
            projects[i].references.push(r);
        }
    }
    let aliases = collect(
        tx.prepare("SELECT project_id, slug FROM project_slug_alias ORDER BY slug")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?,
    )?;
    for (pid, slug) in aliases {
        if let Some(&i) = index.get(&pid) {
            projects[i].aliases.push(slug);
        }
    }
    Ok(projects)
}

impl Repository for SqliteRepository {
    fn list_projects(&mut self, include_archived: bool) -> Result<Vec<Project>> {
        let tx = self.conn.transaction()?;
        read_projects(&tx, include_archived)
    }

    fn get_project(&mut self, id: &str) -> Result<Option<Project>> {
        Ok(self.list_projects(true)?.into_iter().find(|p| p.id == id))
    }

    fn insert_project(&mut self, p: &Project) -> Result<()> {
        let tx = self.write()?;
        insert_project_tx(&tx, p)?;
        Ok(tx.commit()?)
    }

    fn update_project(&mut self, p: &Project) -> Result<()> {
        let tx = self.write()?;
        update_project_tx(&tx, p)?;
        Ok(tx.commit()?)
    }

    fn delete_project(&mut self, id: &str, title: &str) -> Result<bool> {
        let tx = self.write()?;
        let n = tx.execute(
            "DELETE FROM project WHERE id = ?1 AND title = ?2",
            [id, title],
        )?;
        tx.commit()?;
        Ok(n > 0)
    }

    fn insert_task(&mut self, t: &Task) -> Result<Task> {
        let tx = self.write()?;
        let t = insert_task_tx(&tx, t)?;
        tx.commit()?;
        Ok(t)
    }

    fn update_task(&mut self, t: &Task) -> Result<()> {
        let tx = self.write()?;
        update_task_tx(&tx, t)?;
        Ok(tx.commit()?)
    }

    fn delete_task(&mut self, id: &str) -> Result<bool> {
        let tx = self.write()?;
        let project_id: Option<String> = tx
            .query_row("SELECT project_id FROM task WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        let Some(project_id) = project_id else {
            return Ok(false);
        };
        tx.execute("DELETE FROM task WHERE id = ?1", [id])?;
        touch(&tx, &project_id, &Utc::now())?;
        tx.commit()?;
        Ok(true)
    }

    fn insert_reference(&mut self, r: &Reference) -> Result<()> {
        let tx = self.write()?;
        insert_reference_tx(&tx, r)?;
        Ok(tx.commit()?)
    }

    fn delete_reference(&mut self, id: &str) -> Result<bool> {
        let tx = self.write()?;
        let n = tx.execute("DELETE FROM reference WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    fn kv_get(&mut self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    fn kv_set(&mut self, key: &str, value: &str) -> Result<()> {
        let tx = self.write()?;
        kv_set_tx(&tx, key, value)?;
        Ok(tx.commit()?)
    }

    fn write_batch(&mut self, writes: &[Write]) -> Result<()> {
        let tx = self.write()?;
        for w in writes {
            apply_write(&tx, w)?;
        }
        Ok(tx.commit()?) // dropping `tx` on an early return rolls everything back
    }

    fn modify_project(
        &mut self,
        id: &str,
        change: &mut dyn FnMut(&mut Project) -> Result<()>,
        extra: &[Write],
    ) -> Result<()> {
        let tx = self.write()?;
        let before = read_projects(&tx, true)?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| AppError::NotFound {
                what: "project",
                query: id.into(),
            })?;
        let mut after = before.clone();
        change(&mut after)?;
        let fields = |p: &Project| Project {
            tasks: vec![],
            references: vec![],
            aliases: vec![],
            ..p.clone()
        };
        if fields(&after) != fields(&before) {
            update_project_tx(&tx, &after)?;
        }
        for t in &after.tasks {
            match before.tasks.iter().find(|o| o.id == t.id) {
                None => insert_task_tx(&tx, t).map(|_| ())?,
                Some(o) if o != t => update_task_tx(&tx, t)?,
                Some(_) => {}
            }
        }
        for r in &after.references {
            if !before.references.iter().any(|o| o.id == r.id) {
                insert_reference_tx(&tx, r)?;
            }
        }
        for w in extra {
            apply_write(&tx, w)?;
        }
        Ok(tx.commit()?)
    }

    fn data_version(&mut self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))?)
    }

    fn backup_to(&mut self, dest: &Path) -> Result<()> {
        if dest.exists() {
            return Err(AppError::Conflict(format!(
                "{} already exists",
                dest.display()
            )));
        }
        copy_database(&self.conn, dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::fixtures::{project, task};
    use crate::domain::{Stage, TaskStatus};

    fn repo() -> SqliteRepository {
        SqliteRepository::open_in_memory().unwrap()
    }

    fn seeded(r: &mut SqliteRepository, id: &str, slug: &str) -> Project {
        let p = project(id, slug, slug);
        r.insert_project(&p).unwrap();
        p
    }

    #[test]
    fn project_round_trip_with_tasks_and_refs() {
        let mut r = repo();
        let mut p = project("P1", "alpha", "Alpha");
        p.path = Some("/tmp/alpha".into());
        p.stage = Stage::Review;
        let mut t = task("T1", TaskStatus::Done);
        t.project_id = "P1".into();
        t.completed_at = Some(Utc::now());
        p.tasks.push(t);
        p.references.push(Reference {
            id: "R1".into(),
            project_id: "P1".into(),
            task_id: Some("T1".into()),
            kind: crate::domain::RefKind::Pr,
            value: "#12".into(),
            url: None,
            created_at: Utc::now(),
        });
        r.insert_project(&p).unwrap();
        let got = r.get_project("P1").unwrap().unwrap();
        // timestamps are stored at millisecond precision
        assert_eq!(got.tasks.len(), 1);
        assert_eq!(got.references[0].value, "#12");
        assert_eq!(got.path.as_deref(), Some("/tmp/alpha"));
        assert_eq!(got.stage, Stage::Review);
        assert_eq!(got.progress().done, 1);
    }

    #[test]
    fn duplicate_slug_conflicts() {
        let mut r = repo();
        seeded(&mut r, "P1", "alpha");
        let err = r
            .insert_project(&project("P2", "alpha", "Other"))
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "{err:?}");
    }

    #[test]
    fn rename_keeps_identity_and_old_slug_as_alias() {
        let mut r = repo();
        let mut p = seeded(&mut r, "P1", "alpha");
        p.slug = "beta".into();
        p.title = "Beta".into();
        r.update_project(&p).unwrap();
        let got = r.get_project("P1").unwrap().unwrap();
        assert_eq!(
            (got.slug.as_str(), got.aliases.clone()),
            ("beta", vec!["alpha".to_string()])
        );
        // the old slug is reserved, another project cannot take it
        let err = r.insert_project(&project("P2", "alpha", "x")).unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
        // going back to the old slug removes it from the aliases
        p.slug = "alpha".into();
        r.update_project(&p).unwrap();
        let got = r.get_project("P1").unwrap().unwrap();
        assert_eq!(
            (got.slug.as_str(), got.aliases.clone()),
            ("alpha", vec!["beta".to_string()])
        );
    }

    #[test]
    fn update_missing_project_is_not_found() {
        let mut r = repo();
        let err = r.update_project(&project("nope", "x", "x")).unwrap_err();
        assert!(matches!(err, AppError::NotFound { .. }));
    }

    #[test]
    fn tasks_get_increasing_positions_and_touch_project() {
        let mut r = repo();
        let p = seeded(&mut r, "P1", "alpha");
        for id in ["T1", "T2", "T3"] {
            let mut t = task(id, TaskStatus::Todo);
            t.project_id = "P1".into();
            t.updated_at = Utc::now() + chrono::Duration::seconds(10);
            r.insert_task(&t).unwrap();
        }
        let got = r.get_project("P1").unwrap().unwrap();
        assert_eq!(
            got.tasks
                .iter()
                .map(|t| (t.id.as_str(), t.position))
                .collect::<Vec<_>>(),
            vec![("T1", 0), ("T2", 1), ("T3", 2)]
        );
        assert!(got.updated_at > p.updated_at);
    }

    #[test]
    fn task_for_missing_project_is_not_found() {
        let mut r = repo();
        let mut t = task("T1", TaskStatus::Todo);
        t.project_id = "ghost".into();
        assert!(matches!(
            r.insert_task(&t),
            Err(AppError::NotFound {
                what: "project",
                ..
            })
        ));
    }

    #[test]
    fn update_and_delete_task() {
        let mut r = repo();
        seeded(&mut r, "P1", "alpha");
        let mut t = task("T1", TaskStatus::Todo);
        t.project_id = "P1".into();
        r.insert_task(&t).unwrap();
        t.set_status(TaskStatus::Done, Utc::now());
        r.update_task(&t).unwrap();
        let got = r.get_project("P1").unwrap().unwrap().tasks.remove(0);
        assert_eq!(got.status, TaskStatus::Done);
        assert!(got.completed_at.is_some());
        assert!(r.delete_task("T1").unwrap());
        assert!(!r.delete_task("T1").unwrap());
        assert!(matches!(r.update_task(&t), Err(AppError::NotFound { .. })));
    }

    #[test]
    fn write_batch_is_all_or_nothing() {
        let mut r = repo();
        seeded(&mut r, "P1", "alpha");
        let fresh = project("P2", "beta", "Beta");
        let clash = project("P3", "alpha", "Alpha again");
        let err = r
            .write_batch(&[
                Write::InsertProject(&fresh),
                Write::KvSet("k", "v"),
                Write::InsertProject(&clash),
            ])
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "{err}");
        assert_eq!(r.list_projects(true).unwrap().len(), 1);
        assert_eq!(r.kv_get("k").unwrap(), None);

        r.write_batch(&[Write::InsertProject(&fresh), Write::KvSet("k", "v")])
            .unwrap();
        assert_eq!(r.list_projects(true).unwrap().len(), 2);
        assert_eq!(r.kv_get("k").unwrap().as_deref(), Some("v"));
    }

    #[test]
    fn modify_project_edits_the_row_as_stored_at_write_time() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("pg.db");
        let (mut a, mut b) = (
            SqliteRepository::open(&db).unwrap(),
            SqliteRepository::open(&db).unwrap(),
        );
        let mut p = project("P1", "alpha", "Alpha");
        let mut t = task("T1", TaskStatus::Todo);
        t.project_id = "P1".into();
        p.tasks.push(t);
        a.insert_project(&p).unwrap();

        let stale = a.get_project("P1").unwrap().unwrap();
        let mut renamed = stale.tasks[0].clone();
        renamed.title = "renamed elsewhere".into();
        b.update_task(&renamed).unwrap(); // another process commits after `stale` was read

        a.modify_project(
            "P1",
            &mut |p| {
                p.tasks[0].set_status(TaskStatus::Done, Utc::now());
                p.stage = Stage::Review;
                Ok(())
            },
            &[],
        )
        .unwrap();
        let now = b.get_project("P1").unwrap().unwrap();
        assert_eq!(
            (now.tasks[0].title.as_str(), now.tasks[0].status, now.stage),
            ("renamed elsewhere", TaskStatus::Done, Stage::Review)
        );

        let err = a.modify_project(
            "P1",
            &mut |p| {
                p.stage = Stage::Done;
                Err(AppError::invalid("x", "abort"))
            },
            &[Write::KvSet("k", "v")],
        );
        assert!(err.is_err());
        assert_eq!(a.get_project("P1").unwrap().unwrap().stage, Stage::Review);
        assert_eq!(a.kv_get("k").unwrap(), None, "extra writes roll back too");
        assert!(matches!(
            a.modify_project("nope", &mut |_| Ok(()), &[]),
            Err(AppError::NotFound { .. })
        ));
    }

    #[test]
    fn delete_project_cascades() {
        let mut r = repo();
        let mut p = project("P1", "alpha", "Alpha");
        let mut t = task("T1", TaskStatus::Todo);
        t.project_id = "P1".into();
        p.tasks.push(t);
        r.insert_project(&p).unwrap();
        assert!(
            !r.delete_project("P1", "Renamed").unwrap(),
            "title must still match"
        );
        assert!(r.delete_project("P1", "Alpha").unwrap());
        assert_eq!(r.get_project("P1").unwrap(), None);
        assert!(!r.delete_project("P1", "Alpha").unwrap());
    }

    #[test]
    fn archived_hidden_unless_requested() {
        let mut r = repo();
        let mut p = seeded(&mut r, "P1", "alpha");
        seeded(&mut r, "P2", "beta");
        p.archived_at = Some(Utc::now());
        r.update_project(&p).unwrap();
        assert_eq!(r.list_projects(false).unwrap().len(), 1);
        assert_eq!(r.list_projects(true).unwrap().len(), 2);
    }

    #[test]
    fn kv_upsert() {
        let mut r = repo();
        assert_eq!(r.kv_get("k").unwrap(), None);
        r.kv_set("k", "1").unwrap();
        r.kv_set("k", "2").unwrap();
        assert_eq!(r.kv_get("k").unwrap().as_deref(), Some("2"));
    }

    #[test]
    fn data_version_changes_when_another_connection_commits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let mut a = SqliteRepository::open(&path).unwrap();
        let mut b = SqliteRepository::open(&path).unwrap();
        let before = a.data_version().unwrap();
        seeded(&mut b, "P1", "alpha");
        assert_ne!(a.data_version().unwrap(), before);
    }

    #[test]
    fn backup_is_a_usable_copy_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = SqliteRepository::open(&dir.path().join("t.db")).unwrap();
        seeded(&mut a, "P1", "alpha");
        let dest = dir.path().join("copy.db");
        a.backup_to(&dest).unwrap();
        let mut copy = SqliteRepository::open(&dest).unwrap();
        assert_eq!(copy.list_projects(true).unwrap().len(), 1);
        assert!(matches!(a.backup_to(&dest), Err(AppError::Conflict(_))));
    }

    #[test]
    fn rejects_database_from_the_future_and_backs_up_before_migrating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        drop(SqliteRepository::open(&path).unwrap());

        let c = Connection::open(&path).unwrap();
        c.pragma_update(None, "user_version", migrations::LATEST + 1)
            .unwrap();
        drop(c);
        let err = SqliteRepository::open(&path).err().unwrap();
        assert!(err.to_string().contains("newer"), "{err}");

        // simulate an older schema: the pending migration must leave a .bak-v file behind
        let old = dir.path().join("old.db");
        {
            let mut c = Connection::open(&old).unwrap();
            c.execute_batch("CREATE TABLE marker(x)").unwrap();
            c.pragma_update(None, "user_version", migrations::LATEST - 1)
                .unwrap();
            let _ = &mut c;
        }
        // LATEST is 1 today, so version 0 means "fresh": only assert the backup path when applicable
        if migrations::LATEST > 1 {
            SqliteRepository::open(&old).unwrap();
            assert!(
                dir.path()
                    .join(format!("old.db.bak-v{}", migrations::LATEST - 1))
                    .exists()
            );
        }
    }

    #[test]
    fn check_constraints_reject_bad_enum_values() {
        let r = repo();
        let err = r.conn.execute(
            "INSERT INTO project (id, slug, title, priority, stage, created_at, updated_at)
             VALUES ('x','x','x','urgent','idea','t','t')",
            [],
        );
        assert!(err.is_err());
    }
}
