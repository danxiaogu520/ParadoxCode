//! Typed `file://`/`pdcloc://` URI boundary between the LSP wire format and filesystem paths.
//!
//! Parsing, percent-encoding, and serialization follow the WHATWG URL Standard via the `url`
//! crate. This module adds only the two product rules the standard does not cover: the
//! `pdcloc://` transparent-localisation scheme, which shares `file://` path semantics, and
//! the mapping between URI authorities and Windows UNC paths.

use std::fmt;
use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;
use url::Url;

/// A validated `file://` or `pdcloc://` URI.
///
/// The serialized form is the canonical WHATWG spelling, so round-trips through
/// [`FileUri::parse`] and [`FileUri::as_str`] are lossless for both schemes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FileUri {
    url: Url,
}

impl FileUri {
    /// Parses a client-supplied URI. Only the `file` and `pdcloc` schemes are accepted; the
    /// scheme comparison itself is case-insensitive per RFC 3986.
    pub fn parse(uri: &str) -> Result<Self, UriError> {
        let url = Url::parse(uri).map_err(|_| UriError::UnsupportedScheme)?;
        match url.scheme() {
            "file" | "pdcloc" => Ok(Self { url }),
            _ => Err(UriError::UnsupportedScheme),
        }
    }

    /// Builds the `file://` URI of a filesystem path.
    ///
    /// Extended-length Windows spellings are normalized first: `\\?\C:\...` loses its
    /// verbatim prefix and `\\?\UNC\server\share` becomes `\\server\share`, which serializes
    /// as `file://server/share/...`.
    pub fn from_path(path: &Path) -> Result<Self, UriError> {
        let url = Url::from_file_path(portable(path)).map_err(|_| UriError::NotAbsolute)?;
        Ok(Self { url })
    }

    /// Returns the canonical serialized URI.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }

    /// Resolves the URI to a filesystem path.
    ///
    /// A `localhost` authority counts as local. Any other authority names a remote host: on
    /// Windows it maps to a UNC path, elsewhere no filesystem spelling exists and the URI is
    /// rejected.
    pub fn to_path(&self) -> Result<PathBuf, UriError> {
        let host = self.url.host_str().unwrap_or_default();
        if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
            #[cfg(windows)]
            {
                let host = decode(host)?;
                let path = decode(self.url.path())?;
                return Ok(PathBuf::from(format!(
                    r"\\{host}{}",
                    path.replace('/', "\\")
                )));
            }
            #[cfg(not(windows))]
            return Err(UriError::UnsupportedAuthority);
        }
        let path = decode(self.url.path())?;
        #[cfg(windows)]
        let path = path.strip_prefix('/').unwrap_or(&path).to_owned();
        Ok(PathBuf::from(path))
    }
}

impl fmt::Display for FileUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.url.as_str())
    }
}

/// URI conversion failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UriError {
    /// The URI is not a supported `file://` or `pdcloc://` URI.
    UnsupportedScheme,
    /// A remote-host authority has no filesystem spelling on this platform.
    UnsupportedAuthority,
    /// A percent escape or UTF-8 sequence is invalid.
    InvalidEncoding,
    /// The path is not absolute and therefore has no `file://` representation.
    NotAbsolute,
}

impl fmt::Display for UriError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedScheme => "unsupported URI scheme",
            Self::UnsupportedAuthority => "unsupported URI authority",
            Self::InvalidEncoding => "invalid URI percent encoding",
            Self::NotAbsolute => "path is not absolute",
        })
    }
}

impl std::error::Error for UriError {}

/// Percent-decodes one URI component into UTF-8.
fn decode(value: &str) -> Result<String, UriError> {
    percent_decode_str(value)
        .decode_utf8()
        .map(|value| value.into_owned())
        .map_err(|_| UriError::InvalidEncoding)
}

/// Normalizes extended-length Windows spellings to the portable form the `url` crate expects:
/// `\\?\UNC\server\share` becomes `\\server\share` and `\\?\C:\...` loses its verbatim prefix;
/// every other path passes through unchanged.
fn portable(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let original = path.to_string_lossy();
        let stripped = if let Some(rest) = original.strip_prefix(r"\\?\UNC\") {
            Some(format!(r"\\{rest}"))
        } else {
            original
                .strip_prefix(r"\\?\")
                .filter(|value| value.as_bytes().get(1) == Some(&b':'))
                .map(str::to_owned)
        };
        if let Some(portable) = stripped {
            return PathBuf::from(portable);
        }
        path.to_path_buf()
    }
    #[cfg(not(windows))]
    path.to_path_buf()
}
