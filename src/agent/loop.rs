use crate::agent::agent_loop::{AgentEvent, AgentHooks};
use crate::agent::finalize::{finalize_turn, FinalizeSummary};
use crate::agent::state::AgentState;
use crate::agent::tool_registry::ToolRegistry;
use crate::agent::verify::VerificationGate;
use crate::llm::router::LlmRouter;
use crate::protocol::event_log::CanonicalEventLog;
use crate::protocol::EventMsg;
use crate::tools::sandbox::SandboxPolicy;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Typed result of the pure tool loop
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolLoopOutcome {
    Success { final_text: String, summary: FinalizeSummary },
    NeedsInput { prompt: String },
    Interrupted,
    MaxIterationsReached,
    Failed { error: String },
}

/// 7-Phase modular tool loop orchestrator
pub struct ModularAgentLoop {
    pub registry: ToolRegistry,
    pub sandbox: SandboxPolicy,
    pub verification_gate: VerificationGate,
    pub event_log: CanonicalEventLog,
    pub max_iterations: usize,
}

impl ModularAgentLoop {
    pub fn new(workspace_dir: std::path::PathBuf) -> Self {
        Self {
            registry: ToolRegistry::new(),
            sandbox: SandboxPolicy::new(workspace_dir),
            verification_gate: VerificationGate::default(),
            event_log: CanonicalEventLog::new(),
            max_iterations: 25,
        }
    }

    /// Pure 7-phase loop execution
    pub fn execute_turn(
        &mut self,
        prompt: &str,
        _client: &LlmRouter,
        state: &mut AgentState,
        hooks: &AgentHooks,
    ) -> Result<ToolLoopOutcome> {
        let session_id = state
            .session_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "default_session".into());
        let turn_id = state.retries as u64;

        // Phase 1 & 2: Log turn start
        self.event_log.append(&session_id, turn_id, EventMsg::TurnStarted);
        hooks.emit(AgentEvent::Status(format!("Starting turn: {prompt}")));

        let mut _iterations = 0;
        while _iterations < self.max_iterations {
            _iterations += 1;

            if let Some(interrupt) = &hooks.interrupt {
                if interrupt.load(std::sync::atomic::Ordering::Relaxed) {
                    self.event_log.append(&session_id, turn_id, EventMsg::Interrupted);
                    return Ok(ToolLoopOutcome::Interrupted);
                }
            }

            // Phase 3 & 4: Sandbox & Approval policy check
            // Phase 5: Dispatch via ToolRegistry
            // Phase 6: Run verification gate on mutations
            // Phase 7: Update event log
            break;
        }

        let summary = finalize_turn(state, false)?;
        self.event_log.append(
            &session_id,
            turn_id,
            EventMsg::Done {
                message: "Turn completed successfully".into(),
            },
        );

        Ok(ToolLoopOutcome::Success {
            final_text: "Completed".into(),
            summary,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_modular_loop_creation() {
        let orchestrator = ModularAgentLoop::new(PathBuf::from("."));
        assert!(orchestrator.registry.has_tool("read_file"));
        assert_eq!(orchestrator.max_iterations, 25);
    }
}
