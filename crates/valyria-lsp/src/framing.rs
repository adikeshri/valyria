//! The LSP wire format: JSON-RPC 2.0 bodies behind HTTP-style
//! `Content-Length` headers.
//!
//! Kept in its own module and written against `AsyncRead`/`AsyncWrite`
//! rather than against a child process, so the whole client can be driven
//! over an in-memory pipe in tests. That is what makes it possible to test
//! lifecycle, timeouts, crash handling and malformed input without any
//! language server being installed.

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::error::{LspError, Result};

/// Refuse a body larger than this. A corrupt or hostile `Content-Length`
/// would otherwise have the client try to allocate whatever number it was
/// sent.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

pub async fn write_message<W: AsyncWrite + Unpin>(writer: &mut W, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message)
        .map_err(|e| LspError::Protocol(format!("could not serialize message: {e}")))?;
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one message, or `None` at a clean end of stream.
///
/// End of stream is `None` rather than an error because it is how a
/// language server exiting normally looks from here; the caller decides
/// whether that was expected.
pub async fn read_message<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Option<Value>> {
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            return Ok(None);
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // the blank line that ends the header block
        }

        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = Some(value.trim().parse().map_err(|_| {
                    LspError::Protocol(format!("invalid Content-Length: {}", value.trim()))
                })?);
            }
            // Other headers (`Content-Type`) are accepted and ignored:
            // the spec allows them and nothing here depends on one.
        } else {
            return Err(LspError::Protocol(format!("malformed header: {trimmed}")));
        }
    }

    let length = content_length
        .ok_or_else(|| LspError::Protocol("message has no Content-Length header".into()))?;
    if length > MAX_MESSAGE_BYTES {
        return Err(LspError::Protocol(format!(
            "message of {length} bytes exceeds the {MAX_MESSAGE_BYTES}-byte cap"
        )));
    }

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map_err(|e| LspError::Protocol(format!("message body is not valid JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn round_trip(message: Value) -> Value {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &message).await.unwrap();
        let mut reader = BufReader::new(std::io::Cursor::new(buffer));
        read_message(&mut reader).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn a_message_round_trips() {
        let message = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        assert_eq!(round_trip(message.clone()).await, message);
    }

    #[tokio::test]
    async fn a_multi_byte_body_is_length_counted_in_bytes_not_characters() {
        // `Content-Length` is bytes; counting characters would truncate
        // every message containing non-ASCII text.
        let message = json!({"jsonrpc": "2.0", "result": "→ ünïcode ←"});
        assert_eq!(round_trip(message.clone()).await, message);
    }

    #[tokio::test]
    async fn end_of_stream_is_none_not_an_error() {
        let mut reader = BufReader::new(std::io::Cursor::new(Vec::new()));
        assert!(read_message(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn extra_headers_are_ignored() {
        let body = br#"{"jsonrpc":"2.0","id":1}"#;
        let mut raw = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n",
            body.len()
        )
        .into_bytes();
        raw.extend_from_slice(body);

        let mut reader = BufReader::new(std::io::Cursor::new(raw));
        let message = read_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(message["id"], 1);
    }

    #[tokio::test]
    async fn several_messages_read_back_in_order() {
        let mut buffer = Vec::new();
        for i in 0..3 {
            write_message(&mut buffer, &json!({"id": i})).await.unwrap();
        }
        let mut reader = BufReader::new(std::io::Cursor::new(buffer));
        for i in 0..3 {
            let message = read_message(&mut reader).await.unwrap().unwrap();
            assert_eq!(message["id"], i);
        }
        assert!(read_message(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_missing_content_length_is_a_protocol_error() {
        let raw = b"Content-Type: application/json\r\n\r\n{}".to_vec();
        let mut reader = BufReader::new(std::io::Cursor::new(raw));
        assert!(matches!(
            read_message(&mut reader).await.unwrap_err(),
            LspError::Protocol(_)
        ));
    }

    #[tokio::test]
    async fn an_absurd_content_length_is_refused_rather_than_allocated() {
        let raw = b"Content-Length: 999999999999\r\n\r\n".to_vec();
        let mut reader = BufReader::new(std::io::Cursor::new(raw));
        let err = read_message(&mut reader).await.unwrap_err();
        assert!(matches!(err, LspError::Protocol(msg) if msg.contains("cap")));
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_is_a_protocol_error_not_a_panic() {
        let mut raw = b"Content-Length: 7\r\n\r\n".to_vec();
        raw.extend_from_slice(b"not{jso");
        let mut reader = BufReader::new(std::io::Cursor::new(raw));
        assert!(matches!(
            read_message(&mut reader).await.unwrap_err(),
            LspError::Protocol(_)
        ));
    }
}
