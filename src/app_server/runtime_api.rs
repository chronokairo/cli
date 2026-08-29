//! Read/query API used by supervisory clients such as ChronoKairo Desktop.
//! All coding-runtime data is owned here; clients only render the result.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const METHODS: &[&str] = &[
    "get_harness_overview",
    "get_harness_skills",
    "get_harness_memory_sessions",
    "get_harness_session_messages",
    "create_harness_session",
    "get_harness_context_files",
    "read_harness_file",
    "write_harness_file",
    "get_harness_automations",
    "get_harness_git_scan",
    "get_harness_git_status",
    "get_context_fragments",
    "assemble_context_fragments",
    "preview_context_for_prompt",
];

pub fn supports(method: &str) -> bool {
    METHODS.contains(&method)
}

pub fn call(
    method: &str,
    params: Option<&Value>,
    default_workspace: &Path,
    default_model: &str,
) -> Result<Value, String> {
    let params = params.cloned().unwrap_or_else(|| json!({}));
    let workspace = params
        .get("workspace")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| default_workspace.to_path_buf());

    match method {
        "get_harness_overview" => overview(&workspace, default_model),
        "get_harness_skills" => skills(&workspace),
        "get_harness_memory_sessions" => memory_sessions(
            &workspace,
            params.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize,
        ),
        "get_harness_session_messages" => session_messages(
            params
                .get("sessionId")
                .or_else(|| params.get("session_id"))
                .and_then(Value::as_i64)
                .ok_or("sessionId is required")?,
        ),
        "create_harness_session" => create_session(&workspace, default_model, &params),
        "get_harness_context_files" => context_files(&workspace),
        "read_harness_file" => read_file(&workspace, &params),
        "write_harness_file" => write_file(&workspace, &params),
        "get_harness_automations" => automations(&workspace),
        "get_harness_git_scan" => git_scan(&workspace),
        "get_harness_git_status" => {
            let repository = params
                .get("path")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| workspace.clone());
            git_status(&repository)
        }
        "get_context_fragments" => context_fragments(&workspace),
        "assemble_context_fragments" => assembled_context(&workspace),
        "preview_context_for_prompt" => assembled_context(&workspace).map(|value| {
            value
                .get("promptContext")
                .cloned()
                .unwrap_or(Value::String(String::new()))
        }),
        _ => Err(format!("unsupported runtime method: {method}")),
    }
}

fn memory() -> Result<crate::memory::log::LongTermMemory, String> {
    crate::memory::log::LongTermMemory::new(
        crate::config::home_dir().join(".anamnesic").join("memory.db"),
    )
    .map_err(|error| error.to_string())
}

fn skills(workspace: &Path) -> Result<Value, String> {
    let mut registry = crate::skills::SkillRegistry::new();
    registry.load_from(crate::skills::default_skill_dirs(workspace));
    Ok(Value::Array(
        registry
            .list()
            .iter()
            .map(|skill| {
                json!({
                    "name": skill.name,
                    "description": skill.description,
                    "body": skill.body,
                    "source": skill.source.to_string_lossy(),
                })
            })
            .collect(),
    ))
}

fn memory_sessions(workspace: &Path, limit: usize) -> Result<Value, String> {
    let sessions = memory()?
        .list_sessions(&workspace.to_string_lossy(), limit)
        .map_err(|error| error.to_string())?;
    Ok(Value::Array(
        sessions
            .into_iter()
            .map(|session| {
                json!({
                    "id": session.id,
                    "timestamp": session.timestamp,
                    "updated_at": session.updated_at,
                    "summary": session.summary,
                    "message_count": session.message_count,
                    "model": session.model,
                })
            })
            .collect(),
    ))
}

fn session_messages(session_id: i64) -> Result<Value, String> {
    let messages = memory()?
        .load_session(session_id)
        .map_err(|error| error.to_string())?;
    Ok(Value::Array(
        messages
            .into_iter()
            .map(|(seq, role, content)| {
                json!({"id": seq, "sessionId": session_id, "role": role, "content": content, "timestamp": ""})
            })
            .collect(),
    ))
}

fn create_session(workspace: &Path, default_model: &str, params: &Value) -> Result<Value, String> {
    let memory = memory()?;
    let model = params
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(default_model);
    let id = memory
        .start_session(&workspace.to_string_lossy(), model)
        .map_err(|error| error.to_string())?;
    let title = params
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("New session");
    if let Some(prompt) = params.get("initialPrompt").and_then(Value::as_str) {
        if !prompt.trim().is_empty() {
            memory
                .append_messages(id, &[(0, "user".to_string(), prompt.trim().to_string())])
                .map_err(|error| error.to_string())?;
        }
    }
    memory
        .update_session(id, title, "", model)
        .map_err(|error| error.to_string())?;
    Ok(json!(id))
}

fn context_files(workspace: &Path) -> Result<Value, String> {
    let candidates = [
        "AGENTS.md",
        "CLAUDE.md",
        "MEMORY.md",
        ".github/copilot-instructions.md",
    ];
    let mut files = Vec::new();
    for name in candidates {
        let path = workspace.join(name);
        if !path.is_file() {
            continue;
        }
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let modified = path
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(|time| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339())
            .unwrap_or_default();
        files.push(json!({
            "name": name,
            "path": path.to_string_lossy(),
            "lines": content.lines().count(),
            "modified": modified,
        }));
    }
    Ok(Value::Array(files))
}

fn resolve_file(workspace: &Path, params: &Value) -> Result<PathBuf, String> {
    let raw = params.get("path").and_then(Value::as_str).ok_or("path is required")?;
    let path = PathBuf::from(raw);
    let path = if path.is_absolute() { path } else { workspace.join(path) };
    let workspace = workspace.canonicalize().map_err(|error| error.to_string())?;
    let parent = path.parent().unwrap_or(&path);
    let resolved_parent = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
    if !resolved_parent.starts_with(&workspace) {
        return Err("path escapes the runtime workspace".to_string());
    }
    Ok(path)
}

fn read_file(workspace: &Path, params: &Value) -> Result<Value, String> {
    let path = resolve_file(workspace, params)?;
    std::fs::read_to_string(path)
        .map(Value::String)
        .map_err(|error| error.to_string())
}

fn write_file(workspace: &Path, params: &Value) -> Result<Value, String> {
    let path = resolve_file(workspace, params)?;
    let content = params.get("content").and_then(Value::as_str).ok_or("content is required")?;
    std::fs::write(path, content).map_err(|error| error.to_string())?;
    Ok(json!({"ok": true}))
}

fn automations(workspace: &Path) -> Result<Value, String> {
    let directory = workspace.join(".github").join("workflows");
    if !directory.is_dir() {
        return Ok(json!([]));
    }
    let mut items = Vec::new();
    for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())?.flatten() {
        let path = entry.path();
        if !matches!(path.extension().and_then(|value| value.to_str()), Some("yml" | "yaml")) {
            continue;
        }
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let file = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let name = content
            .lines()
            .find_map(|line| line.trim().strip_prefix("name:").map(str::trim))
            .filter(|value| !value.is_empty())
            .unwrap_or(&file)
            .trim_matches(['\'', '"'])
            .to_string();
        items.push(json!({"name": name, "file": file, "triggers": [], "jobs": [], "path": path.to_string_lossy()}));
    }
    Ok(Value::Array(items))
}

fn git_output(workspace: &Path, args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim_end().to_string())
        .unwrap_or_default()
}

fn git_status(workspace: &Path) -> Result<Value, String> {
    let branch = git_output(workspace, &["branch", "--show-current"]);
    let porcelain = git_output(workspace, &["status", "--porcelain"]);
    let mut modified = Vec::new();
    let mut untracked = Vec::new();
    for line in porcelain.lines() {
        let path = line.get(3..).unwrap_or("").to_string();
        if line.starts_with("??") { untracked.push(path) } else { modified.push(path) }
    }
    Ok(json!({"branch": branch, "clean": modified.is_empty() && untracked.is_empty(), "modified": modified, "untracked": untracked}))
}

fn git_scan(workspace: &Path) -> Result<Value, String> {
    let status = git_status(workspace)?;
    let name = workspace.file_name().unwrap_or_default().to_string_lossy();
    let branch = status.get("branch").cloned().unwrap_or(Value::String(String::new()));
    let repo = json!({
        "path": workspace.to_string_lossy(), "name": name, "branch": branch,
        "ahead": 0, "behind": 0, "files": 0, "tech_stack": [],
        "build_status": if status.get("clean") == Some(&Value::Bool(true)) { "Clean" } else { "Modified" },
        "last_activity": chrono::Utc::now().to_rfc3339()
    });
    Ok(json!({"repos": [repo], "scanned_dir": workspace.to_string_lossy()}))
}

fn context_fragments(workspace: &Path) -> Result<Value, String> {
    let files = context_files(workspace)?;
    let fragments = files.as_array().into_iter().flatten().filter_map(|file| {
        let path = file.get("path")?.as_str()?;
        let content = std::fs::read_to_string(path).ok()?;
        let token_estimate = content.len() / 4;
        Some(json!({
            "category": "agent_guide", "title": file.get("name")?, "content": content,
            "tokenEstimate": token_estimate, "path": path
        }))
    }).collect();
    Ok(Value::Array(fragments))
}

fn assembled_context(workspace: &Path) -> Result<Value, String> {
    let fragments = context_fragments(workspace)?;
    let items = fragments.as_array().cloned().unwrap_or_default();
    let prompt = items.iter().filter_map(|item| item.get("content").and_then(Value::as_str)).collect::<Vec<_>>().join("\n\n");
    let used = items.iter().filter_map(|item| item.get("path").cloned()).collect::<Vec<_>>();
    let total_bytes = prompt.len();
    Ok(json!({"promptContext": prompt, "assembled": prompt, "fragmentsUsed": used, "totalBytes": total_bytes}))
}

fn overview(workspace: &Path, model: &str) -> Result<Value, String> {
    let skills_count = skills(workspace)?.as_array().map_or(0, Vec::len);
    let memory_sessions = memory_sessions(workspace, 500)?.as_array().map_or(0, Vec::len);
    let git = git_scan(workspace)?;
    Ok(json!({
        "node": null,
        "sync": {"status": "local", "connected": false},
        "providers": {"active_model": model},
        "skills_count": skills_count,
        "memory_sessions": memory_sessions,
        "repos": git.get("repos").cloned().unwrap_or_else(|| json!([])),
    }))
}
