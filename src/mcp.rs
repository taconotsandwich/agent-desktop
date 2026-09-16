use crate::runtime::Javascript;
use rmcp::{
    ErrorData as McpError, ServerHandler, handler::server::wrapper::Parameters, model::*, tool,
    tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::time::Duration;

#[derive(Clone)]
pub struct AgentDesktop {
    javascript: Javascript,
}

impl AgentDesktop {
    pub fn new(javascript: Javascript) -> Self {
        Self { javascript }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct JsArgs {
    pub code: String,
    pub title: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ResetArgs {}

#[tool_router]
impl AgentDesktop {
    #[tool(
        description = "Persistent JavaScript computer use. Start with await agentdesktop.getState(); select apps with const app = await agentdesktop.getApp(\"Krita\"); observations emit automatically. App methods: getAXState({emit?,disableDiffing?}), getScreenshot({emit?}), getAXStateAndScreenshot(options?), click(elementIndex|[x,y],{mouseButton?,clickCount?}), drag([x,y],[x,y]), pressKey(chord), scroll(elementIndex|[x,y],up|down|left|right,pages?), typeText(text), paste(text,{format?}), setValue(index,value), selectText(index,text,{prefix?,suffix?,selectionType?}), performSecondaryAction(index,action). Coordinates refer to the latest target screenshot. Re-observe after actions; element indices expire after a mutation. Native coordinate input focuses the target. Semantic background actions depend on the app. action_pending means the application has not acknowledged dispatch; observe before acting again. Rich clipboard formats report unsupported. Discovery: agentdesktop.listApps(). nodeRepl.write(value) emits text; nodeRepl.emitImage(bytes) emits a PNG. Timeouts reset JavaScript bindings; ordinary errors preserve them."
    )]
    async fn js(&self, Parameters(args): Parameters<JsArgs>) -> Result<CallToolResult, McpError> {
        let timeout = Duration::from_millis(args.timeout_ms.unwrap_or(30_000).clamp(1, 120_000));
        match self.javascript.evaluate(args.code, timeout).await {
            Ok(items) => {
                let mut content = Vec::new();
                for item in items {
                    match item["type"].as_str() {
                        Some("image") => {
                            if let (Some(data), Some(mime)) =
                                (item["data"].as_str(), item["mimeType"].as_str())
                            {
                                content.push(Content::image(data, mime));
                            }
                        }
                        Some("text") => {
                            content.push(Content::text(item["text"].as_str().unwrap_or("")))
                        }
                        _ => {}
                    }
                }
                if content.is_empty() {
                    content.push(Content::text("Script completed."));
                }
                Ok(CallToolResult::success(content))
            }
            Err(error) => Ok(CallToolResult::error(vec![Content::text(error)])),
        }
    }

    #[tool(
        description = "Reset persistent JavaScript bindings and target observations. Does not close user applications."
    )]
    async fn js_reset(
        &self,
        Parameters(_): Parameters<ResetArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self.javascript.reset().await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(
                "JavaScript session reset.",
            )])),
            Err(error) => Ok(CallToolResult::error(vec![Content::text(error)])),
        }
    }
}

#[tool_handler]
impl ServerHandler for AgentDesktop {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}
