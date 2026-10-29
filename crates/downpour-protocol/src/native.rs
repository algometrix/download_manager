//! Browser native messaging framing: a 32-bit length in native byte order
//! followed by that many bytes of UTF-8 JSON.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Firefox caps host-to-browser messages at 1 MiB.
pub const MAX_OUTGOING_BYTES: usize = 1024 * 1024;
/// Browsers allow larger messages to the host; batches of links stay well
/// under this, and anything bigger is not a request we produce.
pub const MAX_INCOMING_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum NativeMessageError {
    #[error("stdio: {0}")]
    Io(#[from] io::Error),
    #[error("message of {0} bytes exceeds the limit")]
    TooLarge(usize),
    #[error("message is not valid JSON for this protocol: {0}")]
    Json(#[from] serde_json::Error),
}

/// Reads one message; `Ok(None)` means the browser closed the port.
pub fn read_message<T: DeserializeOwned>(
    reader: &mut impl Read,
) -> Result<Option<T>, NativeMessageError> {
    let mut length = [0u8; 4];
    match reader.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let length = u32::from_ne_bytes(length) as usize;
    if length > MAX_INCOMING_BYTES {
        return Err(NativeMessageError::TooLarge(length));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

pub fn write_message<T: Serialize>(
    writer: &mut impl Write,
    message: &T,
) -> Result<(), NativeMessageError> {
    let body = serde_json::to_vec(message)?;
    if body.len() > MAX_OUTGOING_BYTES {
        return Err(NativeMessageError::TooLarge(body.len()));
    }
    writer.write_all(&(body.len() as u32).to_ne_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::HostReply;

    #[test]
    fn round_trips_a_message() {
        let mut wire = Vec::new();
        write_message(&mut wire, &HostReply::Shown).unwrap();
        let reply: Option<HostReply> = read_message(&mut wire.as_slice()).unwrap();
        assert_eq!(reply, Some(HostReply::Shown));
    }

    #[test]
    fn clean_eof_ends_the_session() {
        let reply: Option<HostReply> = read_message(&mut [].as_slice()).unwrap();
        assert_eq!(reply, None);
    }

    #[test]
    fn rejects_oversized_length_before_allocating() {
        let wire = (u32::MAX).to_ne_bytes();
        let error = read_message::<HostReply>(&mut wire.as_slice()).unwrap_err();
        assert!(matches!(error, NativeMessageError::TooLarge(_)));
    }
}
