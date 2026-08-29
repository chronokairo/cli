use crate::agent::agent_loop::AgentHooks;
use crate::agent::state::{AgentState, TodoItem};
use crate::llm::router::LlmRouter;
use crate::tools::patch::apply_patch;
use crate::tools::shell;
use crate::tools::test::{self, VerificationResult};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Effect class for tools, used for concurrency scheduling and approval policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolEffect {
    ReadOnly,
    Mutation,
    Command,
}

/// Structured outcome of a tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub output: String,
    pub mutated: bool,
    pub changed_file: Option<String>,
    pub verification: Option<VerificationResult>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

impl ToolOutput {
    pub fn output(text: impl Into<String>) -> Self {
        Self {
            output: text.into(),
            mutated: false,
            changed_file: None,
            verification: None,
            exit_code: None,
            timed_out: false,
        }
    }
}

/// Execution context passed to tools
pub struct ToolContext<'a> {
    pub state: &'a mut AgentState,
    pub hooks: &'a AgentHooks,
    pub client: &'a LlmRouter,
}

/// Single universal Tool trait for native and MCP capabilities
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> Value;
    fn effect_class(&self) -> ToolEffect;
    
    fn is_concurrency_safe(&self) -> bool {
        self.effect_class() == ToolEffect::ReadOnly
    }
    
    fn requires_approval(&self) -> bool {
        self.effect_class() != ToolEffect::ReadOnly
    }

    fn execute(
        &self,
        ctx: &mut ToolContext,
        args: &serde_json::Map<String, Value>,
    ) -> Result<ToolOutput>;
}

// -----------------------------------------------------------------------------
// Native Tool Implementations
// -----------------------------------------------------------------------------

pub struct ReadFileTool;
impl Tool for ReadFileTool {
    fn name(&self) -> &str { "read_file" }
    fn description(&self) -> &str { "Read file content by relative path with optional line window" }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative file path" },
                "start_line": { "type": "integer", "description": "1-based starting line number (optional)" },
                "end_line": { "type": "integer", "description": "1-based ending line number (optional)" }
            },
            "required": ["path"]
        })
    }
    fn effect_class(&self) -> ToolEffect { ToolEffect::ReadOnly }
    fn execute(&self, ctx: &mut ToolContext, args: &serde_json::Map<String, Value>) -> Result<ToolOutput> {
        let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing path"))?;
        let start = args.get("start_line").and_then(|v| v.as_u64()).map(|v| v as usize);
        let end = args.get("end_line").and_then(|v| v.as_u64()).map(|v| v as usize);
        
        let out = match (start, end) {
            (Some(s), Some(e)) => ctx.state.files.read_file_range(path, s, e)?,
            (Some(s), None) => ctx.state.files.read_file_range(path, s, s + 199)?,
            _ => ctx.state.files.read_file(path).ok_or_else(|| anyhow!("file not found: {path}"))?,
        };
        Ok(ToolOutput::output(out))
    }
}

pub struct WriteFileTool;
impl Tool for WriteFileTool {
    fn name(&self) -> &str { "write_file" }
    fn description(&self) -> &str { "Write full content to a file, creating parent directories if needed" }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative file path" },
                "content": { "type": "string", "description": "Full file content" }
            },
            "required": ["path", "content"]
        })
    }
    fn effect_class(&self) -> ToolEffect { ToolEffect::Mutation }
    fn execute(&self, ctx: &mut ToolContext, args: &serde_json::Map<String, Value>) -> Result<ToolOutput> {
        let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing path"))?;
        let content = args.get("content").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing content"))?;
        
        if let Some(err) = ctx.state.locked_path_error(path) {
            return Ok(ToolOutput::output(err));
        }

        ctx.state.files.write_file(path, content)?;
        ctx.state.session.add_file(path);
        ctx.state.mark_changed(path);
        Ok(ToolOutput {
            output: format!("wrote {path}"),
            mutated: true,
            changed_file: Some(path.to_string()),
            verification: None,
            exit_code: None,
            timed_out: false,
        })
    }
}

pub struct EditFileTool;
impl Tool for EditFileTool {
    fn name(&self) -> &str { "edit_file" }
    fn description(&self) -> &str { "Replace lines by range or content match" }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative file path" },
                "start_line": { "type": "integer", "description": "1-based starting line" },
                "end_line": { "type": "integer", "description": "1-based ending line" },
                "old_content": { "type": "string", "description": "Content being replaced for verification" },
                "new_content": { "type": "string", "description": "New replacement content" }
            },
            "required": ["path", "new_content"]
        })
    }
    fn effect_class(&self) -> ToolEffect { ToolEffect::Mutation }
    fn execute(&self, ctx: &mut ToolContext, args: &serde_json::Map<String, Value>) -> Result<ToolOutput> {
        let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing path"))?;
        let new_content = args.get("new_content").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing new_content"))?;
        let start = args.get("start_line").and_then(|v| v.as_u64()).map(|v| v as usize);
        let end = args.get("end_line").and_then(|v| v.as_u64()).map(|v| v as usize);
        let old = args.get("old_content").and_then(|v| v.as_str());

        if let Some(err) = ctx.state.locked_path_error(path) {
            return Ok(ToolOutput::output(err));
        }

        if start.is_none() && end.is_none() && old.is_none() {
            ctx.state.files.write_file(path, new_content)?;
        } else {
            ctx.state.files.edit_file(path, start, end, old, new_content)?;
        }

        ctx.state.session.add_file(path);
        ctx.state.mark_changed(path);
        Ok(ToolOutput {
            output: format!("edited {path}"),
            mutated: true,
            changed_file: Some(path.to_string()),
            verification: None,
            exit_code: None,
            timed_out: false,
        })
    }
}

pub struct ApplyPatchTool;
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str { "apply_patch" }
    fn description(&self) -> &str { "Apply a standard unified diff patch to one or more files" }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "description": "Unified diff text (--- a/... +++ b/... @@ ... @@)" }
            },
            "required": ["patch"]
        })
    }
    fn effect_class(&self) -> ToolEffect { ToolEffect::Mutation }
    fn execute(&self, ctx: &mut ToolContext, args: &serde_json::Map<String, Value>) -> Result<ToolOutput> {
        let patch_text = args.get("patch").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing patch"))?;
        let results = apply_patch(&ctx.state.config.workspace_dir, patch_text)?;
        
        let mut summaries = Vec::new();
        let mut last_changed = None;
        for res in results {
            ctx.state.session.add_file(&res.file_path);
            ctx.state.mark_changed(&res.file_path);
            summaries.push(format!("{}: +{} -{}", res.file_path, res.lines_added, res.lines_removed));
            last_changed = Some(res.file_path);
        }

        Ok(ToolOutput {
            output: format!("applied patch:\n{}", summaries.join("\n")),
            mutated: true,
            changed_file: last_changed,
            verification: None,
            exit_code: None,
            timed_out: false,
        })
    }
}

pub struct RunCommandTool;
impl Tool for RunCommandTool {
    fn name(&self) -> &str { "run_command" }
    fn description(&self) -> &str { "Run a shell command in the workspace" }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to run" }
            },
            "required": ["command"]
        })
    }
    fn effect_class(&self) -> ToolEffect { ToolEffect::Command }
    fn execute(&self, ctx: &mut ToolContext, args: &serde_json::Map<String, Value>) -> Result<ToolOutput> {
        let cmd = args.get("command").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing command"))?;
        let output = shell::run_command_raw_with_interrupt(cmd, &ctx.state.config, ctx.hooks.interrupt.as_deref());
        Ok(ToolOutput {
            output: output.combined(),
            mutated: false,
            changed_file: None,
            verification: None,
            exit_code: output.code,
            timed_out: output.timed_out,
        })
    }
}

pub struct RunTestsTool;
impl Tool for RunTestsTool {
    fn name(&self) -> &str { "run_tests" }
    fn description(&self) -> &str { "Run tests in the project" }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Optional test command override" }
            }
        })
    }
    fn effect_class(&self) -> ToolEffect { ToolEffect::Command }
    fn execute(&self, ctx: &mut ToolContext, args: &serde_json::Map<String, Value>) -> Result<ToolOutput> {
        let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let verification = test::run_tests(cmd, &ctx.state.config);
        ctx.state.record_verification(verification.clone());
        Ok(ToolOutput {
            output: verification.output.clone(),
            mutated: false,
            changed_file: None,
            verification: Some(verification.clone()),
            exit_code: verification.exit_code,
            timed_out: verification.timed_out,
        })
    }
}

pub struct ListTreeTool;
impl Tool for ListTreeTool {
    fn name(&self) -> &str { "list_tree" }
    fn description(&self) -> &str { "List directory tree structure" }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative directory path" },
                "depth": { "type": "integer", "description": "Maximum directory depth" }
            }
        })
    }
    fn effect_class(&self) -> ToolEffect { ToolEffect::ReadOnly }
    fn execute(&self, ctx: &mut ToolContext, args: &serde_json::Map<String, Value>) -> Result<ToolOutput> {
        let p = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let depth = args.get("depth").and_then(|v| v.as_u64()).map(|v| v as usize).unwrap_or(2);
        let tree = ctx.state.files.list_tree(p, depth, 200)?;
        Ok(ToolOutput::output(tree))
    }
}

pub struct SearchCodeTool;
impl Tool for SearchCodeTool {
    fn name(&self) -> &str { "search_code" }
    fn description(&self) -> &str { "Search code patterns across files" }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Search pattern or regex" },
                "path": { "type": "string", "description": "Relative directory or file path" }
            },
            "required": ["pattern"]
        })
    }
    fn effect_class(&self) -> ToolEffect { ToolEffect::ReadOnly }
    fn execute(&self, ctx: &mut ToolContext, args: &serde_json::Map<String, Value>) -> Result<ToolOutput> {
        let pattern = args.get("pattern").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing pattern"))?;
        let res = match std::process::Command::new("rg")
            .args(["-n", "--max-count", "20", pattern])
            .current_dir(&ctx.state.config.workspace_dir)
            .output()
        {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
            Ok(out) if out.status.code() == Some(1) => format!("No matches for: {pattern}"),
            Ok(out) => format!("search error: {}", String::from_utf8_lossy(&out.stderr).trim()),
            Err(_) => format!("search for {pattern} completed with no external runner"),
        };
        Ok(ToolOutput::output(res))
    }
}

pub struct TodoTool;
impl Tool for TodoTool {
    fn name(&self) -> &str { "todo" }
    fn description(&self) -> &str { "Manage TODO list for the session" }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["list", "add", "complete", "remove"], "description": "Operation" },
                "text": { "type": "string", "description": "TODO item text" },
                "index": { "type": "integer", "description": "1-based TODO item index" }
            }
        })
    }
    fn effect_class(&self) -> ToolEffect { ToolEffect::Command }
    fn execute(&self, ctx: &mut ToolContext, args: &serde_json::Map<String, Value>) -> Result<ToolOutput> {
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("list");
        let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let index = args.get("index").and_then(|v| v.as_u64()).map(|v| v as usize);
        
        match op {
            "add" => {
                if text.trim().is_empty() {
                    Ok(ToolOutput::output("missing required argument: text"))
                } else {
                    let pos = ctx.state.todos.len();
                    ctx.state.todos.push(TodoItem::new(text));
                    Ok(ToolOutput::output(format!("todo {pos}: {text}")))
                }
            }
            "complete" | "remove" => {
                let position = match index {
                    Some(i) => i.checked_sub(1),
                    None => ctx.state.todos.iter().position(|item| item.text == text),
                };
                match position {
                    Some(pos) if pos < ctx.state.todos.len() => {
                        if op == "complete" {
                            let done = ctx.state.todos[pos].text.clone();
                            ctx.state.todos[pos].done = true;
                            Ok(ToolOutput::output(format!("completed todo: {done}")))
                        } else {
                            let removed = ctx.state.todos.remove(pos);
                            Ok(ToolOutput::output(format!("removed todo: {}", removed.text)))
                        }
                    }
                    _ => Ok(ToolOutput::output("todo not found")),
                }
            }
            _ => {
                let items: Vec<String> = ctx.state.todos.iter().enumerate().map(|(i, item)| {
                    let mark = if item.done { "[x]" } else { "[ ]" };
                    format!("{} {}: {}", mark, i + 1, item.text)
                }).collect();
                Ok(ToolOutput::output(if items.is_empty() { "no todos".into() } else { items.join("\n") }))
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Tool Registry
// -----------------------------------------------------------------------------

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        let mut reg = Self {
            tools: HashMap::new(),
        };
        reg.register_default_tools();
        reg
    }

    pub fn register_default_tools(&mut self) {
        self.register(Box::new(ReadFileTool)).unwrap();
        self.register(Box::new(WriteFileTool)).unwrap();
        self.register(Box::new(EditFileTool)).unwrap();
        self.register(Box::new(ApplyPatchTool)).unwrap();
        self.register(Box::new(RunCommandTool)).unwrap();
        self.register(Box::new(RunTestsTool)).unwrap();
        self.register(Box::new(ListTreeTool)).unwrap();
        self.register(Box::new(SearchCodeTool)).unwrap();
        self.register(Box::new(TodoTool)).unwrap();
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) -> Result<()> {
        let name = tool.name().to_string();
        if self.tools.contains_key(&name) {
            return Err(anyhow!("Tool '{name}' is already registered in registry"));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|b| b.as_ref())
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn execute(
        &self,
        name: &str,
        ctx: &mut ToolContext,
        args: &serde_json::Map<String, Value>,
    ) -> Result<ToolOutput> {
        let tool = self.tools.get(name).ok_or_else(|| anyhow!("Tool '{name}' not found in registry"))?;
        tool.execute(ctx, args)
    }

    pub fn all_schemas(&self) -> Vec<Value> {
        self.tools
            .values()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.schema()
                    }
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_registration_and_dispatch() {
        let registry = ToolRegistry::new();
        assert!(registry.has_tool("read_file"));
        assert!(registry.has_tool("write_file"));
        assert!(registry.has_tool("apply_patch"));
        assert!(!registry.has_tool("nonexistent_tool"));

        let schemas = registry.all_schemas();
        assert!(schemas.len() >= 9);
    }
}
