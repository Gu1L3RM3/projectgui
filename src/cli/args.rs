use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;

use crate::domain::{Priority, RefKind, Stage};

const LONG_ABOUT: &str = "\
ProjectGui keeps track of your projects: priority, stage, tasks and a derived progress bar.
The same data is available from this CLI, the MCP server (`projectgui mcp`) and the GUI.

PROJECT arguments accept the project's slug, an old slug, or its ULID. Read-only commands
(status, show, list) also accept any unique prefix or fragment of the slug/title; commands that
change data need the exact slug or ULID. Ambiguous names are rejected, never guessed.
TASK and REFERENCE arguments accept the ULID or its last 4+ characters (shown dimmed in `show`).

DATA
  Stored in a local SQLite database (WAL mode, safe for concurrent use by CLI, MCP and GUI).
  Default location is shown by `projectgui config path`; override with PROJECTGUI_DB.

ENVIRONMENT
  PROJECTGUI_DB       path of the database file
  PROJECTGUI_CONFIG   path of the config file
  NO_COLOR            disable colors (https://no-color.org)

EXIT CODES
  0 ok   1 storage/other   2 not found   3 ambiguous   4 invalid input
  5 confirmation required   6 conflict   7 database busy";

const AFTER_HELP: &str = "\
EXAMPLES
  projectgui add \"Website redesign\" -p high -s in-progress --here
  projectgui task add website \"Write copy\" \"Pick fonts\"
  projectgui task done website a1b2c3
  projectgui status                      # overview of every project
  projectgui status --here               # project linked to the current directory
  projectgui status website              # one project in detail
  projectgui list --priority high,critical --min-progress 50 --sort progress";

#[derive(Parser, Debug)]
#[command(name = "projectgui", version, about = "Local project tracker with CLI, MCP server and GUI",
          long_about = LONG_ABOUT, after_long_help = AFTER_HELP, propagate_version = true)]
pub struct Cli {
    /// Machine-readable JSON output (where the command prints data)
    #[arg(long, global = true)]
    pub json: bool,
    /// Never use ANSI colors
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Draw progress bars with plain ASCII
    #[arg(long, global = true)]
    pub ascii: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args, Debug, Default, Clone)]
pub struct FilterArgs {
    /// Only these priorities (comma separated: low,medium,high,critical)
    #[arg(long, value_delimiter = ',')]
    pub priority: Vec<Priority>,
    /// Only these stages (comma separated: idea,planning,in-progress,review,done,paused)
    #[arg(long, value_delimiter = ',')]
    pub stage: Vec<Stage>,
    /// Minimum progress percentage (0-100)
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub min_progress: Option<u8>,
    /// Maximum progress percentage (0-100)
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub max_progress: Option<u8>,
    /// Only unfinished projects
    #[arg(long, conflicts_with = "done")]
    pub open: bool,
    /// Only finished projects (every task done)
    #[arg(long)]
    pub done: bool,
    /// Include archived projects
    #[arg(long)]
    pub archived: bool,
    /// Sort by: priority, progress, updated, title
    #[arg(long)]
    pub sort: Option<String>,
    /// Reverse the sort order
    #[arg(long)]
    pub reverse: bool,
    /// Only projects whose title, slug, description or task titles contain this text (case-insensitive)
    #[arg(short = 's', long)]
    pub search: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Colored overview of all projects, or detail of one (NAME / --here)
    Status {
        /// Project to show in detail (slug, id or unique fragment)
        name: Option<String>,
        /// Show the project linked to the current directory (or a parent of it)
        #[arg(short = 'C', long, conflicts_with = "name")]
        here: bool,
        #[command(flatten)]
        filters: FilterArgs,
    },
    /// Compact table of projects with filters
    List {
        #[command(flatten)]
        filters: FilterArgs,
    },
    /// Full detail of one project, including tasks and references
    Show { project: String },
    /// Create a project
    Add {
        title: String,
        #[arg(short, long)]
        description: Option<String>,
        #[arg(short, long)]
        priority: Option<Priority>,
        #[arg(short, long)]
        stage: Option<Stage>,
        /// Directory to link (absolute path)
        #[arg(long, conflicts_with = "here")]
        path: Option<String>,
        /// Link the current directory
        #[arg(long)]
        here: bool,
        /// Initial task (repeatable)
        #[arg(short, long = "task")]
        tasks: Vec<String>,
    },
    /// Change project fields
    Edit {
        project: String,
        #[arg(long)]
        title: Option<String>,
        /// New slug (the old one keeps working as an alias)
        #[arg(long)]
        slug: Option<String>,
        #[arg(short, long)]
        description: Option<String>,
        #[arg(short, long)]
        priority: Option<Priority>,
        #[arg(short, long)]
        stage: Option<Stage>,
        #[arg(long, conflicts_with_all = ["here", "clear_path"])]
        path: Option<String>,
        #[arg(long)]
        here: bool,
        #[arg(long)]
        clear_path: bool,
    },
    /// Delete a project and its tasks (asks you to type its title)
    Rm {
        project: String,
        /// Skip the prompt
        #[arg(short, long)]
        yes: bool,
    },
    /// Hide a project from listings without deleting it
    Archive { project: String },
    /// Bring an archived project back
    Unarchive { project: String },
    /// Link the current directory to a project (enables `status --here`)
    Link { project: String },
    /// Manage tasks
    #[command(subcommand)]
    Task(TaskCommand),
    /// Attach commits, pull requests and issues (stored locally, no sync)
    #[command(subcommand, name = "ref")]
    Reference(RefCommand),
    /// Write a consistent copy of the database
    Backup {
        /// Destination file (default: next to the database, timestamped)
        #[arg(long)]
        out: Option<String>,
    },
    /// Load the .project-status file of a directory (default: the current one) into the database
    Sync {
        /// Directory that holds the .project-status file
        path: Option<String>,
        /// Show what would change without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Find .project-status files under a directory and load them (a dry run unless --apply)
    Scan {
        /// Directory to search (default: the current one)
        dir: Option<String>,
        /// How many directory levels to descend (default 3, max 6)
        #[arg(long)]
        depth: Option<usize>,
        /// Write the changes; without it only the plan is shown
        #[arg(long)]
        apply: bool,
    },
    /// Write a starter .project-status file in the current directory
    InitFile {
        /// Project to describe; it must be linked to this directory (`projectgui link`)
        project: Option<String>,
        /// Overwrite an existing file
        #[arg(long)]
        force: bool,
    },
    /// Export everything as JSON
    Export {
        #[arg(long)]
        out: Option<String>,
    },
    /// Merge a JSON export into the database (existing ids are kept)
    Import { file: String },
    /// Read or change settings
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Register the MCP server with Claude Code, Codex and/or OpenCode
    #[cfg(feature = "mcp")]
    InstallMcp {
        target: super::install::Target,
        /// Actually change the client's configuration (default: only print what would be done)
        #[arg(long)]
        write: bool,
    },
    /// Run the MCP server on stdio (for Claude, Codex, OpenCode and other MCP clients)
    #[cfg(feature = "mcp")]
    Mcp,
    /// Open the graphical project overview (task status, new tasks and projects)
    #[cfg(feature = "gui")]
    Gui,
    /// Print shell completions
    Completions { shell: Shell },
    /// Show version and install details
    Version {
        /// Also look up the latest release
        #[cfg(feature = "update")]
        #[arg(long)]
        check: bool,
    },
    /// Check for a newer release and update in place when installed with the official installer
    #[cfg(feature = "update")]
    Update {
        /// Only report; exit code 10 means an update is available
        #[arg(long)]
        check: bool,
        /// Do not ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
    #[cfg(feature = "update")]
    #[command(name = "__refresh-update-cache", hide = true)]
    RefreshUpdateCache,
}

#[derive(Subcommand, Debug)]
pub enum TaskCommand {
    /// Add one or more tasks
    Add {
        project: String,
        #[arg(required = true)]
        titles: Vec<String>,
        #[arg(short, long)]
        description: Option<String>,
        #[arg(short, long)]
        priority: Option<Priority>,
    },
    /// List a project's tasks
    List { project: String },
    /// Change a task
    Edit {
        project: String,
        task: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(short, long, conflicts_with = "clear_description")]
        description: Option<String>,
        #[arg(long)]
        clear_description: bool,
        #[arg(short, long, conflicts_with = "clear_priority")]
        priority: Option<Priority>,
        #[arg(long)]
        clear_priority: bool,
    },
    /// Mark tasks in progress
    Start {
        project: String,
        #[arg(required = true)]
        tasks: Vec<String>,
    },
    /// Mark tasks blocked
    Block {
        project: String,
        #[arg(required = true)]
        tasks: Vec<String>,
    },
    /// Mark tasks done
    Done {
        project: String,
        #[arg(required = true)]
        tasks: Vec<String>,
    },
    /// Move tasks back to todo
    Reopen {
        project: String,
        #[arg(required = true)]
        tasks: Vec<String>,
    },
    /// Delete a task
    Rm {
        project: String,
        task: String,
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum RefCommand {
    /// Attach a reference
    Add {
        project: String,
        kind: RefKind,
        /// Commit hash, PR/issue number or title
        value: String,
        #[arg(long)]
        url: Option<String>,
        /// Attach to a task instead of the whole project
        #[arg(long)]
        task: Option<String>,
    },
    /// List references
    List { project: String },
    /// Remove a reference by id (or its last characters)
    Rm { project: String, id: String },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Print one setting
    Get { key: String },
    /// Change a setting
    Set { key: String, value: String },
    /// Print all settings
    List,
    /// Print the config file and database locations
    Path,
}
