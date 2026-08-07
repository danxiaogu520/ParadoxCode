use std::io::{BufRead, Read, Write};

use serde_json::Value;

use crate::protocol::LspError;
use crate::{MAX_LSP_HEADER_BYTES, MAX_LSP_MESSAGE_BYTES};

pub(crate) fn read_message<R: BufRead>(reader: &mut R) -> Result<Option<Value>, LspError> {
    let mut content_length = None;
    let mut saw_header = false;
    let mut header_bytes = 0_usize;
    loop {
        let remaining = MAX_LSP_HEADER_BYTES.saturating_sub(header_bytes);
        if remaining == 0 {
            return Err(LspError::Protocol(
                "LSP headers exceed the safety limit".to_owned(),
            ));
        }
        let mut line = String::new();
        let bytes = (&mut *reader)
            .take(
                u64::try_from(remaining)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
            )
            .read_line(&mut line)?;
        header_bytes = header_bytes.saturating_add(bytes);
        if header_bytes > MAX_LSP_HEADER_BYTES {
            return Err(LspError::Protocol(
                "LSP headers exceed the safety limit".to_owned(),
            ));
        }
        if bytes == 0 {
            if saw_header {
                return Err(LspError::Protocol(
                    "unexpected EOF in LSP headers".to_owned(),
                ));
            }
            return Ok(None);
        }
        saw_header = true;
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("Content-Length")
        {
            let parsed = value
                .trim()
                .parse::<usize>()
                .map_err(|_| LspError::Protocol("invalid Content-Length".to_owned()))?;
            if content_length.replace(parsed).is_some() {
                return Err(LspError::Protocol("duplicate Content-Length".to_owned()));
            }
        }
    }
    let content_length =
        content_length.ok_or_else(|| LspError::Protocol("missing Content-Length".to_owned()))?;
    if content_length > MAX_LSP_MESSAGE_BYTES {
        return Err(LspError::Protocol(format!(
            "LSP message exceeds the {MAX_LSP_MESSAGE_BYTES}-byte safety limit"
        )));
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|error| {
        if error.is_data() || error.is_syntax() {
            LspError::Json(error)
        } else {
            LspError::Protocol(format!("invalid JSON-RPC body: {error}"))
        }
    })
}

pub(crate) fn write_message<W: Write>(writer: &mut W, message: &Value) -> Result<(), LspError> {
    let body = serde_json::to_vec(message)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}
