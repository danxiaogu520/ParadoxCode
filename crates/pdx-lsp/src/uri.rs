use std::fmt;
use std::path::{Path, PathBuf};

/// Converts a `file://` URI to a filesystem path.
pub fn uri_to_path(uri: &str) -> Result<PathBuf, UriError> {
    let rest = uri
        .strip_prefix("file://")
        .or_else(|| uri.strip_prefix("FILE://"))
        .ok_or(UriError::UnsupportedScheme)?;
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (authority, encoded_path) = if rest.starts_with('/') {
        (None, rest.to_owned())
    } else if let Some((authority, path)) = rest.split_once('/') {
        (Some(authority), format!("/{path}"))
    } else {
        (Some(rest), "/".to_owned())
    };
    if authority.is_some_and(|value| !value.is_empty() && !value.eq_ignore_ascii_case("localhost"))
    {
        return Err(UriError::UnsupportedAuthority);
    }
    let decoded = percent_decode(&encoded_path)?;
    #[cfg(windows)]
    let decoded = decoded.strip_prefix('/').unwrap_or(&decoded).to_owned();
    Ok(PathBuf::from(decoded))
}

/// Converts an absolute filesystem path to a percent-encoded `file://` URI.
#[must_use]
pub fn path_to_uri(path: &Path) -> String {
    // `fs::canonicalize` returns an extended-length path on Windows (for example
    // `\\\\?\\C:\\mods\\common\\events.txt`).  That spelling is valid for Win32
    // file APIs, but it is not a portable file URI path: encoding the backslashes
    // and the `\\\\?\\` prefix produces a URI that VS Code cannot open.  Normalize
    // only the URI representation; the engine keeps its canonical path unchanged.
    #[cfg(windows)]
    let raw = {
        let original = path.to_string_lossy();
        let drive_path = original
            .strip_prefix("\\\\?\\")
            .filter(|value| value.as_bytes().get(1) == Some(&b':'))
            .unwrap_or(&original);
        if drive_path.as_bytes().get(1) == Some(&b':') {
            drive_path.replace('\\', "/")
        } else {
            // Keep UNC paths on the existing local-authority path until URI
            // authority support is added to uri_to_path.
            drive_path.to_owned()
        }
    };
    #[cfg(not(windows))]
    let raw = path.to_string_lossy().into_owned();
    let mut uri = String::from("file://");
    if !raw.starts_with('/') {
        uri.push('/');
    }
    for byte in raw.as_bytes() {
        if *byte == b'/' || *byte == b':' || is_uri_unreserved(*byte) {
            uri.push(char::from(*byte));
        } else {
            uri.push('%');
            uri.push(hex_digit(byte >> 4));
            uri.push(hex_digit(byte & 0x0f));
        }
    }
    uri
}

/// URI conversion failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UriError {
    /// The URI is not a supported `file://` URI.
    UnsupportedScheme,
    /// A non-local authority was supplied.
    UnsupportedAuthority,
    /// A percent escape or UTF-8 sequence is invalid.
    InvalidEncoding,
}

impl fmt::Display for UriError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedScheme => "unsupported URI scheme",
            Self::UnsupportedAuthority => "unsupported URI authority",
            Self::InvalidEncoding => "invalid URI percent encoding",
        })
    }
}

impl std::error::Error for UriError {}

fn percent_decode(value: &str) -> Result<String, UriError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(UriError::InvalidEncoding);
            }
            let high = hex_value(bytes[index + 1]).ok_or(UriError::InvalidEncoding)?;
            let low = hex_value(bytes[index + 2]).ok_or(UriError::InvalidEncoding)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| UriError::InvalidEncoding)
}

fn is_uri_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        _ => char::from(b'A' + value - 10),
    }
}
