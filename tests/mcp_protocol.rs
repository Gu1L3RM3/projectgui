#![cfg(feature = "mcp")]
//! Speaks real JSON-RPC to `projectgui mcp` over stdio.
mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    /// Every line the server printed; the test fails if any is not JSON-RPC.
    lines: Vec<String>,
}

impl Client {
    fn start(home: &std::path::Path) -> Self {
        let mut child = Command::new(assert_cmd::cargo::cargo_bin("projectgui"))
            .arg("mcp")
            .env("PROJECTGUI_DB", home.join("test.db"))
            .env("PROJECTGUI_CONFIG", home.join("config.toml"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut c = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
            lines: vec![],
        };
        let init = c.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }),
        );
        assert_eq!(init["serverInfo"]["name"], "projectgui");
        c.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        c
    }

    fn send(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn read_line(&mut self) -> Value {
        let mut line = String::new();
        assert!(
            self.stdout.read_line(&mut line).unwrap() > 0,
            "server closed stdout"
        );
        let v: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("non-JSON on stdout ({e}): {line:?}"));
        assert_eq!(v["jsonrpc"], "2.0", "not a JSON-RPC message: {line}");
        self.lines.push(line);
        v
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let v = self.read_line();
            if v["id"] == id {
                assert!(
                    v.get("error").is_none() || method == "tools/call",
                    "{method} failed: {v}"
                );
                return if v.get("error").is_some() {
                    v
                } else {
                    v["result"].clone()
                };
            }
        }
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        self.request("tools/call", json!({ "name": name, "arguments": args }))
    }

    fn finish(mut self) {
        drop(self.stdin);
        let status = self.child.wait().unwrap();
        assert!(
            status.success(),
            "server should exit cleanly when stdin closes"
        );
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut self.stdout, &mut rest).unwrap();
        assert!(
            rest.lines()
                .all(|l| serde_json::from_str::<Value>(l).is_ok()),
            "stray output on stdout: {rest:?}"
        );
    }
}

#[test]
fn lists_tools_resources_prompts() {
    let home = tempfile::tempdir().unwrap();
    let mut c = Client::start(home.path());

    let tools = c.request("tools/list", json!({}));
    let names: Vec<_> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for n in [
        "list_projects",
        "get_project",
        "create_project",
        "update_project",
        "add_tasks",
        "set_tasks_status",
        "delete_project",
        "resolve_project",
        "find_project_for_path",
        "get_summary",
        "scan_projects",
        "sync_project_file",
    ] {
        assert!(names.contains(&n.to_string()), "missing tool {n}");
    }
    let delete = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "delete_project")
        .unwrap();
    assert_eq!(delete["annotations"]["destructiveHint"], true);
    let list = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "list_projects")
        .unwrap();
    assert_eq!(list["annotations"]["readOnlyHint"], true);

    let res = c.request("resources/list", json!({}));
    let uris: Vec<_> = res["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        uris,
        [
            "projectgui://projects",
            "projectgui://summary",
            "projectgui://project-file-format"
        ]
    );
    let tpl = c.request("resources/templates/list", json!({}));
    assert_eq!(
        tpl["resourceTemplates"][0]["uriTemplate"],
        "projectgui://projects/{id}"
    );

    let prompts = c.request("prompts/list", json!({}));
    assert_eq!(prompts["prompts"].as_array().unwrap().len(), 5);
    let p = c.request(
        "prompts/get",
        json!({ "name": "weekly_review", "arguments": {} }),
    );
    assert!(
        p["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("get_summary")
    );
    c.finish();
}

#[test]
fn agent_workflow_end_to_end_and_visible_to_the_cli() {
    let home = tempfile::tempdir().unwrap();
    let mut c = Client::start(home.path());

    let created = c.tool("create_project", json!({ "title": "From agent", "priority": "high", "tasks": [{ "title": "a" }, { "title": "b" }] }));
    assert_eq!(created["isError"], false, "{created}");
    let project = &created["structuredContent"];
    let (pid, tid) = (
        project["id"].as_str().unwrap().to_string(),
        project["tasks"][0]["id"].as_str().unwrap().to_string(),
    );

    let r = c.tool(
        "set_tasks_status",
        json!({ "project_id": pid, "task_ids": [tid], "status": "done" }),
    );
    assert_eq!(r["structuredContent"]["progress"]["percent"], 50);

    // the CLI, a separate process, sees the agent's change immediately
    let out = common::stdout(common::cmd(home.path()).arg("status"));
    assert!(out.contains("From agent") && out.contains(" 50%"), "{out}");

    let read = c.request(
        "resources/read",
        json!({ "uri": format!("projectgui://projects/{pid}") }),
    );
    assert!(
        read["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("From agent")
    );
    let summary = c.request("resources/read", json!({ "uri": "projectgui://summary" }));
    assert!(
        summary["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("From agent")
    );
    c.finish();
}

#[test]
fn errors_are_structured_and_destructive_actions_need_confirmation() {
    let home = tempfile::tempdir().unwrap();
    let mut c = Client::start(home.path());
    let created = c.tool("create_project", json!({ "title": "Keep" }));
    let pid = created["structuredContent"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let err = c.tool(
        "update_project",
        json!({ "project_id": "keep", "stage": "review" }),
    );
    assert_eq!(err["isError"], true, "{err}");
    let body: Value = serde_json::from_str(err["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        (body["code"].as_str(), body["field"].as_str()),
        (Some("invalid"), Some("project_id"))
    );

    let denied = c.tool(
        "delete_project",
        json!({ "project_id": pid, "confirm_title": "nope" }),
    );
    let body: Value = serde_json::from_str(denied["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["code"], "confirmation_required");
    assert_eq!(
        c.tool("list_projects", json!({}))["structuredContent"]["projects"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let unknown = c.tool("does_not_exist", json!({}));
    assert!(
        unknown.get("error").is_some(),
        "unknown tool is a protocol error: {unknown}"
    );
    c.finish();
}

#[test]
fn project_files_are_scanned_then_synced_and_visible_to_the_cli() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("app");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(".project-status"),
        "title = \"Filed\"\npriority = \"high\"\n[[task]]\ntitle = \"a\"\nstatus = \"done\"\n[[task]]\ntitle = \"b\"\n",
    )
    .unwrap();
    let mut c = Client::start(home.path());

    let scan = c.tool("scan_projects", json!({ "path": root.path() }));
    assert_eq!(scan["isError"], false, "{scan}");
    let files = scan["structuredContent"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["report"]["action"], "create");
    assert_eq!(files[0]["report"]["applied"], false);
    assert!(
        common::stdout(common::cmd(home.path()).arg("list")).contains("No projects"),
        "scan writes nothing"
    );

    let synced = c.tool("sync_project_file", json!({ "path": dir }));
    assert_eq!(synced["isError"], false, "{synced}");
    assert_eq!(synced["structuredContent"]["applied"], true);
    let again = c.tool("sync_project_file", json!({ "path": dir }));
    assert_eq!(again["structuredContent"]["action"], "in_sync");

    let out = common::stdout(common::cmd(home.path()).arg("status"));
    assert!(out.contains("Filed") && out.contains(" 50%"), "{out}");

    for bad in [
        json!({ "path": "relative/dir" }),
        json!({ "path": root.path().join("none") }),
    ] {
        let r = c.tool("sync_project_file", bad);
        assert_eq!(r["isError"], true, "{r}");
    }
    let format = c.request(
        "resources/read",
        json!({ "uri": "projectgui://project-file-format" }),
    );
    assert!(
        format["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("[[task]]")
    );
    let prompt = c.request(
        "prompts/get",
        json!({ "name": "sync_project_files", "arguments": { "path": root.path() } }),
    );
    let text = prompt["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("scan_projects") && text.contains("sync_project_file"));
    c.finish();
}
