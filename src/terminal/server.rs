//! HTTP + WebSocket server exposing a real PTY in the browser without Axum.
//!
//! Routes:
//!   GET /    → xterm.js single-page terminal
//!   GET /ws  → WebSocket bridge: browser stdin/stdout/resize ↔ ConPTY/PTY
//!
//! Protocol over the WebSocket:
//!   client → server: binary frame = terminal input bytes
//!   client → server: text frame = JSON `{"resize":{"cols":120,"rows":30}}`
//!   server → client: binary frame = terminal output bytes

use super::pty::TerminalSession;
use super::ws::{compute_accept, WsMessage, WsStream};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use crate::async_rt::io::{AsyncReadExt, AsyncWriteExt};
use crate::async_rt::net::{TcpListener, TcpStream};

const INDEX_HTML: &str = include_str!("index.html");

#[derive(Clone)]
struct AppState {
    argv: Arc<Vec<String>>,
    cwd: PathBuf,
}

#[derive(Deserialize)]
struct ResizeMsg {
    resize: Resize,
}

#[derive(Deserialize)]
struct Resize {
    cols: u16,
    rows: u16,
}

/// Serve the terminal UI on `addr:port`, spawning `argv` in the PTY.
pub async fn serve(addr: &str, port: u16, argv: Vec<String>, cwd: PathBuf) -> crate::error::Result<()> {
    let state = AppState {
        argv: Arc::new(argv),
        cwd,
    };

    let listener = TcpListener::bind((addr, port)).await?;
    crate::cki_info!("terminal server listening on http://{addr}:{port}");

    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                crate::cki_warn!("TCP accept failed: {e}");
                continue;
            }
        };

        let state = state.clone();
        crate::async_rt::task::spawn(async move {
            if let Err(err) = handle_connection(stream, state).await {
                crate::cki_debug!("connection from {peer_addr} closed: {err}");
            }
        });
    }
}

async fn handle_connection(mut stream: TcpStream, state: AppState) -> std::io::Result<()> {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }

    let req_str = String::from_utf8_lossy(&buf[..n]);
    let mut lines = req_str.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    if method != "GET" {
        let resp = "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n";
        stream.write_all(resp.as_bytes()).await?;
        return Ok(());
    }

    if path == "/" || path == "/index.html" {
        let body = INDEX_HTML.as_bytes();
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(headers.as_bytes()).await?;
        stream.write_all(body).await?;
        stream.flush().await?;
        return Ok(());
    }

    if path == "/ws" {
        let mut ws_key = None;
        for line in lines {
            let lower = line.to_lowercase();
            if lower.starts_with("sec-websocket-key:") {
                let key_part = line["sec-websocket-key:".len()..].trim();
                ws_key = Some(key_part.to_string());
                break;
            }
        }

        if let Some(key) = ws_key {
            let accept = compute_accept(&key);
            let handshake_response = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
                accept
            );
            stream.write_all(handshake_response.as_bytes()).await?;
            stream.flush().await?;

            let ws_stream = WsStream::new(stream);
            session(ws_stream, state).await;
            return Ok(());
        } else {
            let resp = "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n";
            stream.write_all(resp.as_bytes()).await?;
            return Ok(());
        }
    }

    let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
    stream.write_all(resp.as_bytes()).await?;
    Ok(())
}

/// Bridge one browser WebSocket connection to a PTY session.
async fn session(mut socket: WsStream<TcpStream>, state: AppState) {
    let (cols, rows) = match socket.recv().await {
        Ok(Some(WsMessage::Text(text))) => match serde_json::from_str::<ResizeMsg>(&text) {
            Ok(msg) => (msg.resize.cols.max(20), msg.resize.rows.max(5)),
            Err(_) => (120, 30),
        },
        _ => (120, 30),
    };

    let session = match TerminalSession::spawn(&state.argv, cols, rows, Some(&state.cwd)) {
        Ok(s) => s,
        Err(e) => {
            let shell = super::shell::default_shell_command();
            crate::cki_warn!(
                "failed to spawn {:?}: {e}, falling back to default shell {}",
                state.argv,
                shell
            );
            match TerminalSession::spawn(&[shell], cols, rows, Some(&state.cwd)) {
                Ok(s) => s,
                Err(err) => {
                    let _ = socket
                        .send_text(&format!("failed to spawn terminal: {err}"))
                        .await;
                    return;
                }
            }
        }
    };

    crate::cki_info!("terminal session started: {:?}", state.argv);

    let (out_tx, mut out_rx) = crate::async_rt::sync::mpsc::channel::<Vec<u8>>(64);
    let session_out = session.clone_read_handle();
    let output_task = crate::async_rt::task::spawn(async move {
        loop {
            let data = session_out.read_available();
            if data.is_empty() {
                if out_tx.is_closed() {
                    break;
                }
                crate::async_rt::time::sleep(std::time::Duration::from_millis(10)).await;
                continue;
            }
            if out_tx.send(data).await.is_err() {
                break;
            }
        }
    });

    loop {
        crate::select! {
            data_opt = out_rx.recv() => {
                match data_opt {
                    Some(data) => {
                        if socket.send_binary(&data).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            msg = socket.recv() => match msg {
                Ok(Some(WsMessage::Binary(data))) => {
                    let _ = session.write_input(&data);
                }
                Ok(Some(WsMessage::Text(text))) => {
                    if let Ok(msg) = serde_json::from_str::<ResizeMsg>(&text) {
                        let _ = session.resize(msg.resize.cols.max(20), msg.resize.rows.max(5));
                    } else {
                        let _ = session.write_input(text.as_bytes());
                    }
                }
                Ok(Some(WsMessage::Close)) | Ok(None) | Err(_) => break,
                _ => {}
            }
        }
    }

    output_task.abort();
    crate::cki_info!("terminal session closed");
}
