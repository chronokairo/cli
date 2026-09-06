use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use serde::Deserialize;
use tokio::task;

use crate::terminal::session::TerminalSessionManager;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClientControlMessage {
    Input { data: String },
    Resize { cols: u16, rows: u16 },
}

/// Axum WebSocket endpoint handler for upgrading connection to interactive PTY terminal.
pub async fn ws_terminal_handler(
    ws: WebSocketUpgrade,
    State(manager): State<TerminalSessionManager>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_terminal_socket(socket, manager))
}

async fn handle_terminal_socket(socket: WebSocket, manager: TerminalSessionManager) {
    let session_id = match crate::random::system_u64() { Ok(id) => format!("term-{id:016x}"), Err(error) => { crate::cki_error!("Cannot create terminal session identifier: {error}"); return; } };

    let reader = match manager.create_session(&session_id, 120, 30, None) {
        Ok(reader) => reader,
        Err(err) => {
            crate::cki_error!("Failed to create PTY session {session_id}: {err}");
            return;
        }
    };

    let pty_session = match manager.get_session(&session_id) {
        Some(session) => session,
        None => return,
    };

    let (tx_bytes, mut rx_bytes) = tokio::sync::mpsc::channel::<Vec<u8>>(256);

    // Blocking reader task for PTY stdout -> channel
    let reader_task = task::spawn_blocking(move || loop {
        let data = reader.read_available();
        if data.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }
        if tx_bytes.blocking_send(data).is_err() {
            break;
        }
    });

    let mut socket = socket;
    loop {
        tokio::select! {
            Some(bytes) = rx_bytes.recv() => {
                if let Ok(text) = String::from_utf8(bytes.clone()) {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                } else if socket.send(Message::Binary(bytes.into())).await.is_err() {
                    break;
                }
            }
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(ctrl) = serde_json::from_str::<ClientControlMessage>(&text) {
                        match ctrl {
                            ClientControlMessage::Input { data } => {
                                let _ = pty_session.write_input(data.as_bytes());
                            }
                            ClientControlMessage::Resize { cols, rows } => {
                                let _ = pty_session.resize(cols, rows);
                            }
                        }
                    } else {
                        let _ = pty_session.write_input(text.as_bytes());
                    }
                }
                Some(Ok(Message::Binary(bytes))) => {
                    let _ = pty_session.write_input(&bytes);
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    }

    reader_task.abort();
    manager.remove_session(&session_id);
}
