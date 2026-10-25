use rusqlite::{Connection, TransactionBehavior};

/// Ordered; index + 1 is the schema version. Never edit an applied entry, append instead.
const MIGRATIONS: &[&str] = &[include_str!("migrations/0001_init.sql")];

pub const LATEST: i64 = MIGRATIONS.len() as i64;

pub fn current(conn: &Connection) -> rusqlite::Result<i64> {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
}

/// Applies pending migrations, each in its own immediate transaction (safe against another
/// process migrating at the same moment: the loser re-reads the version once it holds the lock).
pub fn apply(conn: &mut Connection) -> rusqlite::Result<()> {
    loop {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let Some(sql) = MIGRATIONS.get(version as usize) else {
            return Ok(());
        };
        tx.execute_batch(sql)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)")?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
            [version + 1],
        )?;
        tx.pragma_update(None, "user_version", version + 1)?;
        tx.commit()?;
    }
}
