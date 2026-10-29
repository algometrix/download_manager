//! Loopback IPC between the native host and the app.
//!
//! The app listens on `127.0.0.1` on an ephemeral port and writes the port
//! and a random token to `ipc.json` in its per-user data directory. Clients
//! must present the token, so other local users and web pages that can reach
//! loopback ports cannot inject downloads. Each connection carries one
//! newline-terminated JSON request and one newline-terminated JSON reply.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::messages::{AppReply, AppRequest};

pub const ENDPOINT_FILE: &str = "ipc.json";
pub const MAX_LINE_BYTES: u64 = 8 * 1024 * 1024;

/// Contents of `ipc.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub token: String,
    pub request: AppRequest,
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("the app is not running")]
    NotRunning,
    #[error("ipc: {0}")]
    Io(#[from] io::Error),
    #[error("ipc message is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ipc message exceeds {MAX_LINE_BYTES} bytes")]
    TooLarge,
    #[error("the app closed the connection without replying")]
    NoReply,
}

impl Endpoint {
    pub fn load(data_dir: &Path) -> Result<Endpoint, IpcError> {
        let text = match std::fs::read_to_string(data_dir.join(ENDPOINT_FILE)) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(IpcError::NotRunning)
            }
            Err(error) => return Err(error.into()),
        };
        Ok(serde_json::from_str(&text)?)
    }

    pub fn address(&self) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, self.port))
    }

    /// Sends one request. A refused connection means no app is listening,
    /// usually because `ipc.json` is left over from a crash.
    pub fn send(&self, request: AppRequest, timeout: Duration) -> Result<AppReply, IpcError> {
        let mut stream = match TcpStream::connect_timeout(&self.address(), timeout) {
            Ok(stream) => stream,
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                return Err(IpcError::NotRunning)
            }
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                return Err(IpcError::NotRunning)
            }
            Err(error) => return Err(error.into()),
        };
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        write_line(
            &mut stream,
            &Envelope {
                token: self.token.clone(),
                request,
            },
        )?;
        read_line(&mut BufReader::new(stream))?.ok_or(IpcError::NoReply)
    }
}

pub fn write_line<T: Serialize>(writer: &mut impl Write, message: &T) -> Result<(), IpcError> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()?;
    Ok(())
}

/// Reads one line of JSON; `Ok(None)` when the peer closed without sending.
pub fn read_line<T: DeserializeOwned>(reader: &mut impl BufRead) -> Result<Option<T>, IpcError> {
    let mut line = Vec::new();
    let read = reader
        .take(MAX_LINE_BYTES + 1)
        .read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.len() as u64 > MAX_LINE_BYTES {
        return Err(IpcError::TooLarge);
    }
    Ok(Some(serde_json::from_slice(&line)?))
}

/// Compares tokens without an early exit, so response timing does not reveal
/// how many leading characters were right.
pub fn tokens_match(expected: &str, presented: &str) -> bool {
    let (expected, presented) = (expected.as_bytes(), presented.as_bytes());
    if expected.len() != presented.len() {
        return false;
    }
    expected
        .iter()
        .zip(presented)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_round_trip() {
        let mut wire = Vec::new();
        write_line(&mut wire, &AppReply::Shown).unwrap();
        let reply: Option<AppReply> = read_line(&mut wire.as_slice()).unwrap();
        assert_eq!(reply, Some(AppReply::Shown));
    }

    #[test]
    fn token_comparison() {
        assert!(tokens_match("abc", "abc"));
        assert!(!tokens_match("abc", "abd"));
        assert!(!tokens_match("abc", "ab"));
    }

    #[test]
    fn missing_endpoint_means_not_running() {
        let dir = std::env::temp_dir().join(format!("downpour-ipc-missing-{}", std::process::id()));
        assert!(matches!(Endpoint::load(&dir), Err(IpcError::NotRunning)));
    }
}
