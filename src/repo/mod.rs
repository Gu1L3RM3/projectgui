mod migrations;
mod sqlite;

use std::path::Path;

use crate::domain::Project;
use crate::domain::{Reference, Result, Task};

pub use sqlite::SqliteRepository;

/// One write inside `Repository::write_batch`.
/// Edits of an existing project go through `modify_project` instead.
pub enum Write<'a> {
    InsertProject(&'a Project),
    KvSet(&'a str, &'a str),
}

impl Write<'_> {
    /// A copy borrowing the same data, so one list of writes can be reused across retries.
    pub fn reborrow<'b>(&'b self) -> Write<'b> {
        match *self {
            Write::InsertProject(p) => Write::InsertProject(p),
            Write::KvSet(k, v) => Write::KvSet(k, v),
        }
    }
}

/// Persistence boundary. Every method is atomic; implementations must be safe for several
/// processes using the same store at once.
// `data_version` is consumed by the GUI (feature `gui`).
#[allow(dead_code)]
pub trait Repository {
    fn list_projects(&mut self, include_archived: bool) -> Result<Vec<Project>>;
    fn get_project(&mut self, id: &str) -> Result<Option<Project>>;
    /// Inserts the project with its tasks and references. `Conflict` if the slug is taken.
    fn insert_project(&mut self, project: &Project) -> Result<()>;
    /// Updates project fields only (not tasks). A changed slug keeps the old one as alias.
    /// `NotFound` if the project is gone, `Conflict` if the new slug is taken.
    fn update_project(&mut self, project: &Project) -> Result<()>;
    /// Deletes the project only while its title is still `title`, so a confirmation typed against one title can never
    /// remove a project renamed meanwhile. False when nothing was deleted.
    fn delete_project(&mut self, id: &str, title: &str) -> Result<bool>;

    /// Appends the task at the end of its project; returns it with the assigned position.
    fn insert_task(&mut self, task: &Task) -> Result<Task>;
    fn update_task(&mut self, task: &Task) -> Result<()>;
    fn delete_task(&mut self, id: &str) -> Result<bool>;

    fn insert_reference(&mut self, reference: &Reference) -> Result<()>;
    fn delete_reference(&mut self, id: &str) -> Result<bool>;

    /// Applies every write in one transaction: all of them or none. Same errors as the single methods.
    fn write_batch(&mut self, writes: &[Write]) -> Result<()>;

    /// Read-modify-write in one transaction: reads project `id`, lets `change` edit it, then stores the project
    /// fields and every task that is new or differs. No other process can commit in between, so a concurrent change
    /// is never overwritten with a stale copy. New references are inserted; removed tasks and references are ignored.
    /// `extra` writes land in the same transaction. `change` may fail to abort everything.
    fn modify_project(
        &mut self,
        id: &str,
        change: &mut dyn FnMut(&mut Project) -> Result<()>,
        extra: &[Write],
    ) -> Result<()>;

    fn kv_get(&mut self, key: &str) -> Result<Option<String>>;
    fn kv_set(&mut self, key: &str, value: &str) -> Result<()>;

    /// Changes whenever another connection commits; cheap change detection for UIs.
    fn data_version(&mut self) -> Result<i64>;
    /// Consistent snapshot, safe while other processes write.
    fn backup_to(&mut self, dest: &Path) -> Result<()>;
}
