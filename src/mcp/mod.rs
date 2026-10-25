//! MCP server over stdio. stdout carries protocol messages only; diagnostics go to stderr.
#![deny(clippy::print_stdout)]

mod args;
mod prompts;
mod resources;
mod tools;

use std::sync::{Arc, Mutex};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams,
    GetPromptResponse, GetPromptResult, Implementation, ListPromptsResult,
    ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    Prompt, PromptArgument, PromptMessage, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, Resource, ResourceContents, ResourceTemplate, Role, ServerCapabilities,
    ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};

use crate::domain::{AppError, Result};
use crate::repo::SqliteRepository;
use crate::service::Service;

const INSTRUCTIONS: &str = "\
ProjectGui tracks the user's projects: priority, stage, tasks and a progress bar derived from completed tasks.
Workflow: call find_project_for_path with your working directory (or resolve_project with a name) before creating anything, \
so projects are not duplicated. Every mutating tool needs full 26-character ids returned by earlier calls; names and slugs \
are rejected. Mark tasks done only when the work is verifiably complete. Deleting a project or task is permanent: \
ask the user, then pass the confirmation argument. Prompts are templates, not background agents.";

type Shared = Arc<Mutex<Service<SqliteRepository>>>;

#[derive(Clone)]
pub struct ProjectGuiServer {
    svc: Shared,
}

impl ProjectGuiServer {
    pub fn new(svc: Service<SqliteRepository>) -> Self {
        Self {
            svc: Arc::new(Mutex::new(svc)),
        }
    }

    /// SQLite calls block, so they run off the async executor.
    async fn with_service<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Service<SqliteRepository>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let svc = Arc::clone(&self.svc);
        tokio::task::spawn_blocking(move || {
            let mut guard = svc
                .lock()
                .map_err(|_| AppError::Storage("internal lock poisoned".into()))?;
            f(&mut guard)
        })
        .await
        .map_err(|e| AppError::Storage(e.to_string()))?
    }
}

fn tool_error(e: &AppError) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(e.to_json().to_string())])
}

fn protocol_error(e: &AppError) -> McpError {
    match e {
        AppError::NotFound { .. } | AppError::Invalid { .. } => {
            McpError::invalid_params(e.to_string(), Some(e.to_json()))
        }
        _ => McpError::internal_error(e.to_string(), Some(e.to_json())),
    }
}

fn to_tool(d: &tools::ToolDef) -> Tool {
    let schema = d
        .schema
        .as_object()
        .cloned()
        .expect("tool schemas are objects");
    Tool::new(d.name, d.description, Arc::new(schema)).with_annotations(
        ToolAnnotations::default()
            .read_only(d.read_only)
            .destructive(d.destructive)
            .idempotent(d.read_only)
            .open_world(false),
    )
}

impl ServerHandler for ProjectGuiServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new("pgui", env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(
            tools::definitions().iter().map(to_tool).collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        // Unknown tool names are a protocol error; everything else is a tool-level result.
        if !tools::definitions().iter().any(|d| d.name == name) {
            return Err(McpError::invalid_params(
                format!("unknown tool '{name}'"),
                None,
            ));
        }
        let result = self
            .with_service(move |svc| tools::call(svc, &name, request.arguments))
            .await;
        Ok(match result {
            Ok(value) => CallToolResult::structured(value),
            Err(e) => tool_error(&e),
        }
        .into())
    }

    async fn list_resources(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ListResourcesResult, McpError> {
        let items = resources::STATIC
            .iter()
            .map(|(uri, name, desc, mime)| {
                Resource::new(*uri, *name)
                    .with_description(*desc)
                    .with_mime_type(*mime)
            })
            .collect();
        Ok(ListResourcesResult::with_all_items(items))
    }

    async fn list_resource_templates(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ListResourceTemplatesResult, McpError> {
        let t = ResourceTemplate::new(resources::PROJECT_TEMPLATE, "project")
            .with_description("One project by its full id, with tasks and references")
            .with_mime_type("application/json");
        Ok(ListResourceTemplatesResult::with_all_items(vec![t]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ReadResourceResponse, McpError> {
        let uri = request.uri.to_string();
        let content = self
            .with_service({
                let uri = uri.clone();
                move |svc| resources::read(svc, &uri)
            })
            .await
            .map_err(|e| protocol_error(&e))?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(content.text, uri).with_mime_type(content.mime),
        ])
        .into())
    }

    async fn list_prompts(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ListPromptsResult, McpError> {
        let items = prompts::PROMPTS
            .iter()
            .map(|p| {
                let args = p
                    .args
                    .iter()
                    .map(|(n, d, req)| {
                        PromptArgument::new(*n)
                            .with_description(*d)
                            .with_required(*req)
                    })
                    .collect::<Vec<_>>();
                Prompt::new(
                    p.name,
                    Some(p.description),
                    (!args.is_empty()).then_some(args),
                )
            })
            .collect();
        Ok(ListPromptsResult::with_all_items(items))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<GetPromptResponse, McpError> {
        let args = request.arguments.unwrap_or_default();
        let (description, text) =
            prompts::get(&request.name, &args).map_err(|e| protocol_error(&e))?;
        Ok(
            GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)])
                .with_description(description)
                .into(),
        )
    }
}

/// Runs until the client closes stdin.
pub fn serve(svc: Service<SqliteRepository>) -> Result<()> {
    // Logs must never reach stdout: it is the protocol channel.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_max_level(if std::env::var_os("PROJECTGUI_DEBUG").is_some() {
            tracing::Level::DEBUG
        } else {
            tracing::Level::WARN
        })
        .try_init();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| AppError::Storage(e.to_string()))?;
    rt.block_on(async {
        let running = ProjectGuiServer::new(svc)
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| AppError::Storage(format!("mcp startup failed: {e}")))?;
        running
            .waiting()
            .await
            .map_err(|e| AppError::Storage(format!("mcp error: {e}")))?;
        Ok(())
    })
}
