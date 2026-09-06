//! Native process session implemented using Rust standard library.
//!
//! Replaces portable-pty with std::process, providing non-blocking
//! background streaming to the WebSocket output reader.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

/// Cloneable read side of a session, owned by the output-forwarding task.
pub struct SessionReader {
    buffer: Arc<Mutex<VecDeque<u8>>>,
}

impl SessionReader {
    /// Read whatever the child process has written so far.
    pub fn read_available(&self) -> Vec<u8> {
        let mut queue = self.buffer.lock().unwrap();
        if queue.is_empty() {
            return Vec::new();
        }
        queue.drain(..).collect()
    }
}

/// A running terminal session bound to a process.
pub struct TerminalSession {
    child: Arc<Mutex<Child>>,
    writer: Arc<Mutex<std::process::ChildStdin>>,
    reader_buf: Arc<Mutex<VecDeque<u8>>>,
    size: Arc<Mutex<(u16, u16)>>,
}

impl TerminalSession {
    /// Spawn `argv[0]` inside a subprocess with piped IO.
    pub fn spawn(
        argv: &[String],
        cols: u16,
        rows: u16,
        cwd: Option<&Path>,
    ) -> crate::error::Result<Self> {
        if argv.is_empty() {
            return Err(crate::error::message("argv must not be empty"));
        }

        let mut cmd = Command::new(&argv[0]);
        for arg in &argv[1..] {
            cmd.arg(arg);
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| crate::error::message("failed to open child stdin"))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let buffer = Arc::new(Mutex::new(VecDeque::<u8>::new()));

        // Spawn reader threads for stdout and stderr to stream into buffer
        if let Some(mut out) = stdout {
            let buf_clone = Arc::clone(&buffer);
            thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                loop {
                    match out.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut q = buf_clone.lock().unwrap();
                            q.extend(&chunk[..n]);
                            if q.len() > 1024 * 1024 {
                                let excess = q.len() - 1024 * 1024;
                                q.drain(0..excess);
                            }
                        }
                    }
                }
            });
        }

        if let Some(mut err) = stderr {
            let buf_clone = Arc::clone(&buffer);
            thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                loop {
                    match err.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut q = buf_clone.lock().unwrap();
                            q.extend(&chunk[..n]);
                            if q.len() > 1024 * 1024 {
                                let excess = q.len() - 1024 * 1024;
                                q.drain(0..excess);
                            }
                        }
                    }
                }
            });
        }

        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            writer: Arc::new(Mutex::new(stdin)),
            reader_buf: buffer,
            size: Arc::new(Mutex::new((cols, rows))),
        })
    }

    /// Cloneable handle used by the output-forwarding task.
    pub fn clone_read_handle(&self) -> SessionReader {
        SessionReader {
            buffer: Arc::clone(&self.reader_buf),
        }
    }

    /// Resize the terminal.
    pub fn resize(&self, cols: u16, rows: u16) -> crate::error::Result<()> {
        let mut size = self.size.lock().unwrap();
        *size = (cols, rows);
        Ok(())
    }

    /// Write bytes to the child's stdin.
    pub fn write_input(&self, data: &[u8]) -> crate::error::Result<()> {
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(data)?;
        writer.flush()?;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}
