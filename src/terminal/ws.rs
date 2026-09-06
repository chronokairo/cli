//! Pure std WebSocket (RFC 6455) codec and handshake for Tokio streams.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// SHA-1 pure Rust standard implementation for RFC 6455 handshake.
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x67452301;
    let mut h1: u32 = 0xEFCDAB89;
    let mut h2: u32 = 0x98BADCFE;
    let mut h3: u32 = 0x10325476;
    let mut h4: u32 = 0xC3D2E1F0;

    let msg_len_bits = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() * 8) % 512 != 448 {
        msg.push(0);
    }
    msg.extend_from_slice(&msg_len_bits.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;

        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };

            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

/// Compute `Sec-WebSocket-Accept` header response value.
pub fn compute_accept(key: &str) -> String {
    let mut concat = Vec::with_capacity(key.len() + WS_GUID.len());
    concat.extend_from_slice(key.trim().as_bytes());
    concat.extend_from_slice(WS_GUID);
    let digest = sha1(&concat);
    crate::base64::encode(digest)
}

#[derive(Debug, PartialEq, Eq)]
pub enum WsMessage {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close,
}

pub struct WsStream<S> {
    stream: S,
}

impl<S: AsyncReadExt + AsyncWriteExt + Unpin> WsStream<S> {
    pub fn new(stream: S) -> Self {
        Self { stream }
    }

    pub async fn send_text(&mut self, text: &str) -> std::io::Result<()> {
        self.send_frame(0x1, text.as_bytes()).await
    }

    pub async fn send_binary(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.send_frame(0x2, data).await
    }

    pub async fn send_ping(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.send_frame(0x9, data).await
    }

    pub async fn send_pong(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.send_frame(0xA, data).await
    }

    pub async fn send_close(&mut self) -> std::io::Result<()> {
        self.send_frame(0x8, &[]).await
    }

    async fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
        let mut header = Vec::with_capacity(10);
        header.push(0x80 | (opcode & 0x0F)); // FIN + opcode

        let len = payload.len();
        if len <= 125 {
            header.push(len as u8);
        } else if len <= 65535 {
            header.push(126);
            header.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            header.push(127);
            header.extend_from_slice(&(len as u64).to_be_bytes());
        }

        self.stream.write_all(&header).await?;
        if !payload.is_empty() {
            self.stream.write_all(payload).await?;
        }
        self.stream.flush().await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> std::io::Result<Option<WsMessage>> {
        loop {
            let mut b0_b1 = [0u8; 2];
            if self.stream.read_exact(&mut b0_b1).await.is_err() {
                return Ok(None);
            }
            let b0 = b0_b1[0];
            let b1 = b0_b1[1];

            let opcode = b0 & 0x0F;
            let masked = (b1 & 0x80) != 0;
            let len_byte = b1 & 0x7F;

            let payload_len: usize = match len_byte {
                126 => {
                    let mut b = [0u8; 2];
                    self.stream.read_exact(&mut b).await?;
                    u16::from_be_bytes(b) as usize
                }
                127 => {
                    let mut b = [0u8; 8];
                    self.stream.read_exact(&mut b).await?;
                    u64::from_be_bytes(b) as usize
                }
                l => l as usize,
            };

            let mask = if masked {
                let mut m = [0u8; 4];
                self.stream.read_exact(&mut m).await?;
                Some(m)
            } else {
                None
            };

            let mut payload = vec![0u8; payload_len];
            if payload_len > 0 {
                self.stream.read_exact(&mut payload).await?;
                if let Some(m) = mask {
                    for (i, byte) in payload.iter_mut().enumerate() {
                        *byte ^= m[i % 4];
                    }
                }
            }

            match opcode {
                0x1 => {
                    let text = String::from_utf8_lossy(&payload).into_owned();
                    return Ok(Some(WsMessage::Text(text)));
                }
                0x2 => {
                    return Ok(Some(WsMessage::Binary(payload)));
                }
                0x8 => {
                    return Ok(Some(WsMessage::Close));
                }
                0x9 => {
                    self.send_pong(&payload).await?;
                    continue;
                }
                0xA => {
                    continue;
                }
                _ => {
                    continue;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_accept_hash() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let accept = compute_accept(key);
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}
