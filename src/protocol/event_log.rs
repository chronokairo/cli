use super::EventMsg;
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// Versioned canonical event log record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLogEntry {
    pub schema_version: u32,
    pub sequence_id: u64,
    pub timestamp: String,
    pub session_id: String,
    pub turn_id: u64,
    pub event: EventMsg,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// Replay summary representing reconstructed state from an event log
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplayedSession {
    pub session_id: String,
    pub total_turns: usize,
    pub tool_calls: Vec<(String, String)>,
    pub files_changed: Vec<String>,
    pub verifications: Vec<String>,
    pub approvals: Vec<String>,
    pub total_prompt_tokens: usize,
    pub total_completion_tokens: usize,
    pub total_reasoning_tokens: usize,
    pub completed_successfully: bool,
    pub final_message: Option<String>,
}

/// Append-only canonical event log
#[derive(Debug, Clone, Default)]
pub struct CanonicalEventLog {
    entries: Vec<EventLogEntry>,
    next_sequence: u64,
}

impl CanonicalEventLog {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_sequence: 1,
        }
    }

    /// Appends a new event and returns its monotonic sequence ID
    pub fn append(&mut self, session_id: &str, turn_id: u64, event: EventMsg) -> u64 {
        let seq = self.next_sequence;
        self.next_sequence += 1;

        let entry = EventLogEntry {
            schema_version: 1,
            sequence_id: seq,
            timestamp: Utc::now().to_rfc3339(),
            session_id: session_id.to_string(),
            turn_id,
            event,
            metadata: HashMap::new(),
        };
        self.entries.push(entry);
        seq
    }

    pub fn append_with_metadata(
        &mut self,
        session_id: &str,
        turn_id: u64,
        event: EventMsg,
        metadata: HashMap<String, String>,
    ) -> u64 {
        let seq = self.next_sequence;
        self.next_sequence += 1;

        let entry = EventLogEntry {
            schema_version: 1,
            sequence_id: seq,
            timestamp: Utc::now().to_rfc3339(),
            session_id: session_id.to_string(),
            turn_id,
            event,
            metadata,
        };
        self.entries.push(entry);
        seq
    }

    pub fn entries(&self) -> &[EventLogEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Persists entire event log to a JSONL file
    pub fn persist_jsonl(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = File::create(path)
            .with_context(|| format!("Creating event log file {}", path.display()))?;

        for entry in &self.entries {
            let line = serde_json::to_string(entry)?;
            writeln!(file, "{}", line)?;
        }
        Ok(())
    }

    /// Appends a single entry directly to a JSONL file
    pub fn append_to_file(entry: &EventLogEntry, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("Opening event log file {}", path.display()))?;

        let line = serde_json::to_string(entry)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// Loads canonical event log from a JSONL file
    pub fn load_jsonl(path: &Path) -> Result<Self> {
        let file = File::open(path)
            .with_context(|| format!("Opening event log file {}", path.display()))?;
        let reader = BufReader::new(file);

        let mut entries = Vec::new();
        let mut max_seq = 0;

        for (idx, line_res) in reader.lines().enumerate() {
            let line = line_res?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: EventLogEntry = serde_json::from_str(&line)
                .with_context(|| format!("Parsing event log JSON at line {}", idx + 1))?;
            if entry.sequence_id > max_seq {
                max_seq = entry.sequence_id;
            }
            entries.push(entry);
        }

        Ok(Self {
            entries,
            next_sequence: max_seq + 1,
        })
    }

    /// Replays events deterministically and reconstructs session state
    pub fn replay(&self) -> ReplayedSession {
        Self::replay_entries(&self.entries)
    }

    /// Replays a slice of entries deterministically
    pub fn replay_entries(entries: &[EventLogEntry]) -> ReplayedSession {
        let mut summary = ReplayedSession::default();
        let mut seen_files = std::collections::HashSet::new();

        for entry in entries {
            if summary.session_id.is_empty() {
                summary.session_id = entry.session_id.clone();
            }

            match &entry.event {
                EventMsg::TurnStarted => {
                    summary.total_turns += 1;
                }
                EventMsg::ToolCall { name, summary: s } => {
                    summary.tool_calls.push((name.clone(), s.clone()));
                }
                EventMsg::FileChanged { path } => {
                    if seen_files.insert(path.clone()) {
                        summary.files_changed.push(path.clone());
                    }
                }
                EventMsg::Verification { status, summary: s, .. } => {
                    summary.verifications.push(format!("{status}: {s}"));
                }
                EventMsg::ExecApprovalRequest { tool, summary: s, .. } => {
                    summary.approvals.push(format!("{tool}: {s}"));
                }
                EventMsg::TokenUsage {
                    prompt_tokens,
                    completion_tokens,
                    reasoning_tokens,
                    ..
                } => {
                    summary.total_prompt_tokens += prompt_tokens;
                    summary.total_completion_tokens += completion_tokens;
                    summary.total_reasoning_tokens += reasoning_tokens;
                }
                EventMsg::Done { message } => {
                    summary.completed_successfully = true;
                    summary.final_message = Some(message.clone());
                }
                EventMsg::Failed { message } => {
                    summary.completed_successfully = false;
                    summary.final_message = Some(message.clone());
                }
                _ => {}
            }
        }

        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_log_replay() {
        let mut log = CanonicalEventLog::new();
        log.append("sess_1", 1, EventMsg::TurnStarted);
        log.append(
            "sess_1",
            1,
            EventMsg::ToolCall {
                name: "read_file".into(),
                summary: "main.rs".into(),
            },
        );
        log.append(
            "sess_1",
            1,
            EventMsg::FileChanged {
                path: "src/main.rs".into(),
            },
        );
        log.append(
            "sess_1",
            1,
            EventMsg::TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                reasoning_tokens: 20,
                total_tokens: 170,
            },
        );
        log.append(
            "sess_1",
            1,
            EventMsg::Done {
                message: "all tasks complete".into(),
            },
        );

        let replay = log.replay();
        assert_eq!(replay.session_id, "sess_1");
        assert_eq!(replay.total_turns, 1);
        assert_eq!(replay.tool_calls.len(), 1);
        assert_eq!(replay.files_changed, vec!["src/main.rs"]);
        assert_eq!(replay.total_prompt_tokens, 100);
        assert_eq!(replay.total_completion_tokens, 50);
        assert!(replay.completed_successfully);
        assert_eq!(replay.final_message.as_deref(), Some("all tasks complete"));
    }
}
