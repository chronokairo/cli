//! Persistent command rules owned and enforced by the CLI runtime.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision { Allow, Prompt, Forbidden }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecRule {
    pub command: String,
    pub decision: Decision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExecPolicy { pub rules: Vec<ExecRule> }

impl ExecPolicy {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() { return Ok(Self::default()); }
        let content = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
        toml::from_str(&content).map_err(|error| error.to_string())
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let content = toml::to_string_pretty(self).map_err(|error| error.to_string())?;
        std::fs::write(path, content).map_err(|error| error.to_string())
    }

    pub fn check(&self, command: &[String], workspace: Option<&Path>) -> (Decision, Option<&ExecRule>) {
        let full = command.join(" ");
        let first = command.first().map(String::as_str).unwrap_or("");
        let mut matches = self.rules.iter().filter(|rule| {
            if let Some(expected) = rule.scope.as_deref().and_then(|scope| scope.strip_prefix("workspace:")) {
                if !workspace.is_some_and(|path| path.starts_with(expected)) { return false; }
            }
            let pattern = rule.command.trim();
            pattern == "*"
                || pattern == full
                || pattern == first
                || pattern.strip_suffix(" *").is_some_and(|prefix| full.starts_with(prefix))
                || pattern.strip_prefix("* ").is_some_and(|suffix| full.ends_with(suffix))
        }).collect::<Vec<_>>();
        matches.sort_by_key(|rule| rule.decision);
        matches.last().map_or((Decision::Prompt, None), |rule| (rule.decision, Some(*rule)))
    }
}

pub fn default_path() -> PathBuf {
    crate::config::home_dir().join(".anamnesic").join("exec_policy.toml")
}

pub fn load_default() -> ExecPolicy { ExecPolicy::load(&default_path()).unwrap_or_default() }

pub fn command_is_forbidden(command: &str, workspace: &Path) -> bool {
    let tokens = command.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    load_default().check(&tokens, Some(workspace)).0 == Decision::Forbidden
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn most_restrictive_matching_rule_wins() {
        let policy = ExecPolicy { rules: vec![
            ExecRule { command: "cargo *".into(), decision: Decision::Allow, scope: None, justification: None },
            ExecRule { command: "cargo test".into(), decision: Decision::Forbidden, scope: None, justification: None },
        ]};
        assert_eq!(policy.check(&["cargo".into(), "test".into()], None).0, Decision::Forbidden);
    }
}
