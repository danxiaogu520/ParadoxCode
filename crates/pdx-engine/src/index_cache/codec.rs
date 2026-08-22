//! Stable scalar/path codecs shared by cache readers and writers.

use std::path::{Path, PathBuf};

use pdx_rules::FileResolutionPolicy;
use pdx_text::{LogicalPath, TextRange};

use super::IndexCacheError;
use crate::SourceFileId;

pub(super) fn decode_range(start: i64, end: i64) -> Result<TextRange, IndexCacheError> {
    let start = u32::try_from(start)
        .map_err(|_| IndexCacheError::InvalidData("range start exceeds u32".to_owned()))?;
    let end = u32::try_from(end)
        .map_err(|_| IndexCacheError::InvalidData("range end exceeds u32".to_owned()))?;
    TextRange::new(start, end)
        .ok_or_else(|| IndexCacheError::InvalidData("range end precedes start".to_owned()))
}

pub(super) fn encode_file_id(id: SourceFileId) -> Vec<u8> {
    id.get().to_be_bytes().to_vec()
}

pub(super) fn decode_file_id(bytes: &[u8]) -> Result<SourceFileId, IndexCacheError> {
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| IndexCacheError::InvalidData("file id is not eight bytes".to_owned()))?;
    Ok(SourceFileId::new(u64::from_be_bytes(bytes)))
}

pub(super) fn resolution_name(resolution: FileResolutionPolicy) -> &'static str {
    match resolution {
        FileResolutionPolicy::ReplaceByRelativePath => "replace-by-relative-path",
        FileResolutionPolicy::Merge => "merge",
        FileResolutionPolicy::ReplaceDirectory => "replace-directory",
    }
}

pub(super) fn parse_resolution(value: &str) -> Result<FileResolutionPolicy, IndexCacheError> {
    match value {
        "replace-by-relative-path" => Ok(FileResolutionPolicy::ReplaceByRelativePath),
        "merge" => Ok(FileResolutionPolicy::Merge),
        "replace-directory" => Ok(FileResolutionPolicy::ReplaceDirectory),
        value => Err(IndexCacheError::InvalidData(format!(
            "unknown file resolution policy: {value}"
        ))),
    }
}

pub(super) fn join_logical_path(root: &Path, logical: &LogicalPath) -> PathBuf {
    logical
        .as_str()
        .split('/')
        .fold(root.to_owned(), |path, component| path.join(component))
}

#[cfg(unix)]
pub(super) fn encode_path(path: &Path) -> Result<(&'static str, Vec<u8>), IndexCacheError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(("unix-bytes-v1", path.as_os_str().as_bytes().to_vec()))
}

#[cfg(unix)]
pub(super) fn decode_path(bytes: &[u8], encoding: &str) -> Result<PathBuf, IndexCacheError> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    if encoding != "unix-bytes-v1" {
        return Err(IndexCacheError::InvalidData(format!(
            "cache path encoding {encoding} is not usable on this platform"
        )));
    }
    Ok(PathBuf::from(OsStr::from_bytes(bytes)))
}

#[cfg(windows)]
pub(super) fn encode_path(path: &Path) -> Result<(&'static str, Vec<u8>), IndexCacheError> {
    use std::os::windows::ffi::OsStrExt;
    let bytes = path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    Ok(("windows-utf16le-v1", bytes))
}

#[cfg(windows)]
pub(super) fn decode_path(bytes: &[u8], encoding: &str) -> Result<PathBuf, IndexCacheError> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    if encoding != "windows-utf16le-v1" || !bytes.len().is_multiple_of(2) {
        return Err(IndexCacheError::InvalidData(format!(
            "cache path encoding {encoding} is not usable on this platform"
        )));
    }
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|unit| u16::from_le_bytes(*unit))
        .collect::<Vec<_>>();
    Ok(PathBuf::from(OsString::from_wide(&units)))
}

#[cfg(not(any(unix, windows)))]
pub(super) fn encode_path(path: &Path) -> Result<(&'static str, Vec<u8>), IndexCacheError> {
    let value = path
        .to_str()
        .ok_or_else(|| IndexCacheError::InvalidData("source root is not valid UTF-8".to_owned()))?;
    Ok(("utf8-v1", value.as_bytes().to_vec()))
}

#[cfg(not(any(unix, windows)))]
pub(super) fn decode_path(bytes: &[u8], encoding: &str) -> Result<PathBuf, IndexCacheError> {
    if encoding != "utf8-v1" {
        return Err(IndexCacheError::InvalidData(format!(
            "cache path encoding {encoding} is not usable on this platform"
        )));
    }
    let value = std::str::from_utf8(bytes)
        .map_err(|_| IndexCacheError::InvalidData("source root is not UTF-8".to_owned()))?;
    Ok(PathBuf::from(value))
}
