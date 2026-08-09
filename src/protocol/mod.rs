use crate::agent::agent_loop::{
    AgentEvent, AgentHooks, ApprovalDecision, ApprovalRequest, PlanApprovalDecision,
    PlanApprovalRequest,
};
use crate::agent::state::AgentState;
use crate::llm::router::LlmRouter;
use crate::ui::AgentMode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Op {
    UserTurn { prompt: String, mode: String },
    Interrupt,
    ExecApproval { id: u64, decision: String },
    PlanApproval { id: u64, decision: String },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EventMsg {
    TurnStarted,
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCall {
        name: String,
        summary: String,
    },
    ToolCallDelta {
        index: usize,
        name: Option<String>,
        args_delta: String,
    },
    PlanStep {
        index: usize,
        total: usize,
        description: String,
    },
    PlanApprovalRequest {
        id: u64,
        steps: Vec<PlanStepSummary>,
    },
    ExecApprovalRequest {
        id: u64,
        tool: String,
        summary: String,
        risk: String,
    },
    FileChanged {
        path: String,
    },
    Verification {
        status: String,
        command: Option<String>,
        summary: String,
    },
    Transaction {
        action: String,
        summary: String,
    },
    TokenUsage {
        prompt_tokens: usize,
        completion_tokens: usize,
        reasoning_tokens: usize,
        total_tokens: usize,
    },
    Note {
        text: String,
    },
    Routing {
        summary: String,
    },
    Done {
        message: String,
    },
    Failed {
        message: String,
    },
    Interrupted,
    TurnComplete {
        final_text: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStepSummary {
    pub step_type: String,
    pub description: String,
    pub filename: Option<String>,
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApprovalDecisionSerde {
    AllowOnce,
    AllowSession,
    Deny,
}

impl From<ApprovalDecision> for ApprovalDecisionSerde {
    fn from(d: ApprovalDecision) -> Self {
        match d {
            ApprovalDecision::AllowOnce => Self::AllowOnce,
            ApprovalDecision::AllowSession => Self::AllowSession,
            ApprovalDecision::Deny => Self::Deny,
        }
    }
}

impl From<AgentEvent> for EventMsg {
    fn from(ev: AgentEvent) -> Self {
        match ev {
            AgentEvent::Status(text) => EventMsg::Note { text },
            AgentEvent::ToolCall { name, summary } => EventMsg::ToolCall { name, summary },
            AgentEvent::ToolCallDelta {
                index,
                name,
                args_delta,
            } => EventMsg::ToolCallDelta {
                index,
                name,
                args_delta,
            },
            AgentEvent::PlanStep {
                index,
                total,
                description,
            } => EventMsg::PlanStep {
                index,
                total,
                description,
            },
            AgentEvent::FileChanged { path } => EventMsg::FileChanged { path },
            AgentEvent::Verification {
                status,
                command,
                summary,
            } => EventMsg::Verification {
                status,
                command,
                summary,
            },
            AgentEvent::Transaction { action, summary } => {
                EventMsg::Transaction { action, summary }
            }
            AgentEvent::TextDelta { text } => EventMsg::TextDelta { text },
            AgentEvent::TokenUsage {
                prompt_tokens,
                completion_tokens,
                reasoning_tokens,
                total_tokens,
            } => EventMsg::TokenUsage {
                prompt_tokens,
                completion_tokens,
                reasoning_tokens,
                total_tokens,
            },
            AgentEvent::ReasoningDelta { text } => EventMsg::ReasoningDelta { text },
            AgentEvent::ResetReasoning => EventMsg::Note {
                text: "[reset_reasoning]".into(),
            },
            AgentEvent::Routing { summary } => EventMsg::Routing { summary },
            AgentEvent::Done { message } => EventMsg::Done { message },
            AgentEvent::Failed { message } => EventMsg::Failed { message },
            AgentEvent::Interrupted => EventMsg::Interrupted,
        }
    }
}

struct ApprovalWaiter {
    tx: mpsc::Sender<ApprovalDecision>,
}

struct PlanWaiter {
    tx: mpsc::Sender<PlanApprovalDecision>,
}

pub struct Session {
    op_tx: mpsc::Sender<Op>,
    event_rx: Arc<Mutex<mpsc::Receiver<EventMsg>>>,
    event_tx: mpsc::Sender<EventMsg>,
    exec_waiters: Arc<Mutex<HashMap<u64, ApprovalWaiter>>>,
    plan_waiters: Arc<Mutex<HashMap<u64, PlanWaiter>>>,
    interrupt: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Session {
    pub fn spawn(client: LlmRouter, mut state: AgentState) -> Self {
        let (op_tx, op_rx) = mpsc::channel::<Op>();
        let (event_tx, event_rx) = mpsc::channel::<EventMsg>();
        let event_rx = Arc::new(Mutex::new(event_rx));

        let exec_waiters = Arc::new(Mutex::new(HashMap::<u64, ApprovalWaiter>::new()));
        let plan_waiters = Arc::new(Mutex::new(HashMap::<u64, PlanWaiter>::new()));
        let interrupt = Arc::new(AtomicBool::new(false));

        let client_clone = client.clone();
        let exec_waiters_clone = exec_waiters.clone();
        let plan_waiters_clone = plan_waiters.clone();
        let interrupt_clone = interrupt.clone();
        let event_tx = Arc::new(event_tx);
        let event_tx_for_session = event_tx.clone();

        let handle = thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
            runtime.block_on(async {
                loop {
                    match op_rx.recv() {
                        Ok(Op::UserTurn { prompt, mode }) => {
                            interrupt_clone.store(false, Ordering::Relaxed);
                            let agent_mode = match mode.as_str() {
                                "plan" => AgentMode::Plan,
                                _ => AgentMode::Agent,
                            };
                            // Clone senders for this turn
                            let event_tx = event_tx.clone();
                            let event_tx_clone = (*event_tx).clone();
                            let event_tx_text = (*event_tx).clone();
                            let event_tx_reasoning = (*event_tx).clone();
                            let event_tx_tool_delta = (*event_tx).clone();
                            let event_tx_tool_call = (*event_tx).clone();
                            let event_tx_plan_step = (*event_tx).clone();
                            let event_tx_file_changed = (*event_tx).clone();
                            let event_tx_verification = (*event_tx).clone();
                            let event_tx_transaction = (*event_tx).clone();
                            let event_tx_token = (*event_tx).clone();
                            let event_tx_note = (*event_tx).clone();
                            let event_tx_done = (*event_tx).clone();
                            let event_tx_failed = (*event_tx).clone();
                            let event_tx_interrupt = (*event_tx).clone();

                            let hooks = build_protocol_hooks(
                                event_tx_clone,
                                event_tx_text,
                                event_tx_reasoning,
                                event_tx_tool_delta,
                                event_tx_tool_call,
                                event_tx_plan_step,
                                event_tx_file_changed,
                                event_tx_verification,
                                event_tx_transaction,
                                event_tx_token,
                                event_tx_note,
                                event_tx_done,
                                event_tx_failed,
                                event_tx_interrupt,
                                exec_waiters_clone.clone(),
                                plan_waiters_clone.clone(),
                                interrupt_clone.clone(),
                            );
                            crate::agent::agent_loop::run_agent_loop_with_hooks(
                                &client_clone,
                                &mut state,
                                &prompt,
                                &hooks,
                                agent_mode,
                            )
                            .await;
                        }
                        Ok(Op::Interrupt) => {
                            interrupt_clone.store(true, Ordering::Relaxed);
                        }
                        Ok(Op::ExecApproval { id, decision }) => {
                            if let Some(waiter) = exec_waiters_clone.lock().unwrap().remove(&id) {
                                let decision = match decision.as_str() {
                                    "allow_once" => ApprovalDecision::AllowOnce,
                                    "allow_session" => ApprovalDecision::AllowSession,
                                    _ => ApprovalDecision::Deny,
                                };
                                let _ = waiter.tx.send(decision);
                            }
                        }
                        Ok(Op::PlanApproval { id, decision }) => {
                            if let Some(waiter) = plan_waiters_clone.lock().unwrap().remove(&id) {
                                let decision = match decision.as_str() {
                                    "approve" => PlanApprovalDecision::Approve,
                                    _ => PlanApprovalDecision::Deny,
                                };
                                let _ = waiter.tx.send(decision);
                            }
                        }
                        Ok(Op::Shutdown) | Err(_) => {
                            break;
                        }
                    }
                }
            });
        });

        Self {
            op_tx,
            event_rx,
            event_tx: event_tx_for_session.as_ref().clone(),
            exec_waiters,
            plan_waiters,
            interrupt,
            handle: Some(handle),
        }
    }

    pub fn submit(&self, op: Op) -> Result<(), mpsc::SendError<Op>> {
        self.op_tx.send(op)
    }

    pub fn next_event(&self) -> Option<EventMsg> {
        self.event_rx.lock().unwrap().recv().ok()
    }

    pub fn try_next_event(&self) -> Option<EventMsg> {
        self.event_rx.lock().unwrap().try_recv().ok()
    }

    pub fn interrupt(&self) {
        self.interrupt.store(true, Ordering::Relaxed);
    }

    pub fn approve_exec(&self, id: u64, decision: ApprovalDecisionSerde) {
        if let Some(waiter) = self.exec_waiters.lock().unwrap().remove(&id) {
            let decision = match decision {
                ApprovalDecisionSerde::AllowOnce => ApprovalDecision::AllowOnce,
                ApprovalDecisionSerde::AllowSession => ApprovalDecision::AllowSession,
                ApprovalDecisionSerde::Deny => ApprovalDecision::Deny,
            };
            let _ = waiter.tx.send(decision);
        }
    }

    pub fn approve_plan(&self, id: u64, decision: PlanApprovalDecision) {
        if let Some(waiter) = self.plan_waiters.lock().unwrap().remove(&id) {
            let _ = waiter.tx.send(decision);
        }
    }

    pub fn shutdown(&mut self) {
        let _ = self.submit(Op::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[allow(clippy::too_many_arguments)]
fn build_protocol_hooks(
    event_tx: mpsc::Sender<EventMsg>,
    event_tx_text: mpsc::Sender<EventMsg>,
    _event_tx_reasoning: mpsc::Sender<EventMsg>,
    _event_tx_tool_delta: mpsc::Sender<EventMsg>,
    event_tx_tool_call: mpsc::Sender<EventMsg>,
    event_tx_plan_step: mpsc::Sender<EventMsg>,
    _event_tx_file_changed: mpsc::Sender<EventMsg>,
    _event_tx_verification: mpsc::Sender<EventMsg>,
    _event_tx_transaction: mpsc::Sender<EventMsg>,
    _event_tx_token: mpsc::Sender<EventMsg>,
    _event_tx_note: mpsc::Sender<EventMsg>,
    _event_tx_done: mpsc::Sender<EventMsg>,
    _event_tx_failed: mpsc::Sender<EventMsg>,
    _event_tx_interrupt: mpsc::Sender<EventMsg>,
    exec_waiters: Arc<Mutex<HashMap<u64, ApprovalWaiter>>>,
    plan_waiters: Arc<Mutex<HashMap<u64, PlanWaiter>>>,
    interrupt: Arc<AtomicBool>,
) -> AgentHooks {
    AgentHooks {
        on_event: Some(Arc::new(move |ev: AgentEvent| {
            let _ = event_tx.send(ev.into());
        })),
        on_tool_call_delta: None,
        on_text_delta: Some(Arc::new(move |text: &str| {
            let _ = event_tx_text.send(EventMsg::TextDelta {
                text: text.to_string(),
            });
        })),
        on_approval: Some(Arc::new(move |req: ApprovalRequest| {
            let (tx, rx) = mpsc::channel::<ApprovalDecision>();
            exec_waiters
                .lock()
                .unwrap()
                .insert(req.id, ApprovalWaiter { tx });
            let _ = event_tx_tool_call.send(EventMsg::ExecApprovalRequest {
                id: req.id,
                tool: req.tool,
                summary: req.summary,
                risk: req.risk,
            });
            rx.recv().unwrap_or(ApprovalDecision::Deny)
        })),
        on_plan_approval: Some(Arc::new(move |req: PlanApprovalRequest| {
            let (tx, rx) = mpsc::channel::<PlanApprovalDecision>();
            plan_waiters
                .lock()
                .unwrap()
                .insert(req.id, PlanWaiter { tx });
            let steps: Vec<PlanStepSummary> = req
                .steps
                .iter()
                .map(|s| PlanStepSummary {
                    step_type: s.step_type.clone(),
                    description: s.description.clone(),
                    filename: s.filename.clone(),
                    command: s.command.clone(),
                })
                .collect();
            let _ = event_tx_plan_step.send(EventMsg::PlanApprovalRequest { id: req.id, steps });
            rx.recv().unwrap_or(PlanApprovalDecision::Deny)
        })),
        interrupt: Some(interrupt),
    }
}
