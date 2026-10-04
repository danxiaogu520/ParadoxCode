//! Bounded release-binary LSP startup check without linking analyzer libraries.
use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn send(input: &mut impl Write, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    write!(input, "Content-Length: {}\r\n\r\n", bytes.len()).map_err(|e| e.to_string())?;
    input.write_all(&bytes).map_err(|e| e.to_string())?;
    input.flush().map_err(|e| e.to_string())
}
fn frame(reader: &mut impl BufRead) -> Result<Value, String> {
    let mut length = None;
    let mut header_bytes = 0;
    loop {
        let mut line = String::new();
        require(
            (&mut *reader)
                .take((8193 - header_bytes) as u64)
                .read_line(&mut line)
                .map_err(|e| e.to_string())?
                > 0,
            "release server closed its transport",
        )?;
        header_bytes += line.len();
        require(header_bytes <= 8192, "oversized LSP header")?;
        if line.trim().is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            require(length.is_none(), "duplicate LSP length")?;
            length = Some(value.trim().parse::<usize>().map_err(|e| e.to_string())?);
        }
    }
    let length = length.ok_or("missing LSP length")?;
    require(
        length > 0 && length <= 8 * 1024 * 1024,
        "release LSP frame exceeds bound",
    )?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}
fn file_uri(path: &Path) -> Result<String, String> {
    let path = path
        .to_str()
        .ok_or("non-UTF8 fixture path")?
        .replace('\\', "/");
    let path = path.strip_prefix("//?/").unwrap_or(&path);
    let mut uri = if path.starts_with('/') {
        "file://".to_owned()
    } else {
        "file:///".to_owned()
    };
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~:/".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    Ok(uri)
}
pub(super) fn run(root: &Path, args: &Args) -> Result<String, String> {
    let version = workspace_version(root)?;
    let binary = args
        .path("--binary")
        .ok_or("missing --binary")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    require(
        capture(process::command(&binary).arg("--version"))? == format!("paradoxcode {version}"),
        "candidate binary version differs",
    )?;
    let fixture = tempfile::tempdir().map_err(|e| e.to_string())?;
    let mut server = Server(
        process::command(&binary)
            .current_dir(fixture.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| e.to_string())?,
    );
    let mut input = server.0.stdin.take().ok_or("missing server stdin")?;
    let stdout = server.0.stdout.take().ok_or("missing server stdout")?;
    let (sender, receiver) = mpsc::sync_channel(16);
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let value = frame(&mut reader);
            let failed = value.is_err();
            if sender.send(value).is_err() || failed {
                break;
            }
        }
    });
    let uri = file_uri(fixture.path())?;
    let request = |input: &mut std::process::ChildStdin,
                   id: u64,
                   method: &str,
                   params: Option<Value>|
     -> Result<Value, String> {
        let mut message = serde_json::json!({"jsonrpc":"2.0","id":id,"method":method});
        if let Some(params) = params {
            message["params"] = params;
        }
        send(input, &message)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let response = receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|e| format!("candidate LSP response timeout: {e}"))??;
            if response["id"] == id {
                require(
                    response.get("error").is_none(),
                    &format!("candidate request {method} failed: {response}"),
                )?;
                return Ok(response["result"].clone());
            }
        }
    };
    let init = request(
        &mut input,
        1,
        "initialize",
        Some(
            serde_json::json!({"workspaceFolders":[{"uri":uri,"name":"owned-release-fixture"}],"capabilities":{},"initializationOptions":{"vanillaMode":"disabled"}}),
        ),
    )?;
    require(
        init["capabilities"].is_object(),
        "candidate initialization lacks capabilities",
    )?;
    send(
        &mut input,
        &serde_json::json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
    )?;
    let info = request(&mut input, 2, "pdc/analyzerInfo", None)?;
    require(
        info["version"] == version && info["schemaCount"].as_u64().is_some_and(|n| n > 0),
        "candidate analyzer did not initialize embedded rules",
    )?;
    let diagnostics = request(
        &mut input,
        3,
        "pdc/textDiagnostics",
        Some(
            serde_json::json!({"files":[{"path":"events/owned.txt","text":"country_event = { id = owned.1 is_triggered_only = maybe mystery = yes option = { name = owned.option } }"}]}),
        ),
    )?;
    let values = diagnostics[0]["diagnostics"]
        .as_array()
        .ok_or("missing candidate diagnostics")?;
    require(
        values.iter().any(|d| d["code"] == "InvalidValue")
            && values.iter().any(|d| d["code"] == "UnknownKey"),
        "candidate did not validate owned input",
    )?;
    request(&mut input, 4, "shutdown", Some(serde_json::json!({})))?;
    send(
        &mut input,
        &serde_json::json!({"jsonrpc":"2.0","method":"exit"}),
    )?;
    Ok(
        "candidate release binary startup, version, embedded rules and owned diagnostics verified"
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_uris_encode_path_bytes_on_unix_and_windows() {
        assert_eq!(
            file_uri(Path::new("/tmp/a b/中")).unwrap(),
            "file:///tmp/a%20b/%E4%B8%AD"
        );
        assert_eq!(
            file_uri(Path::new(r"C:\Temp\a#b")).unwrap(),
            "file:///C:/Temp/a%23b"
        );
        assert_eq!(
            file_uri(Path::new(r"\\?\C:\Temp\a")).unwrap(),
            "file:///C:/Temp/a"
        );
    }

    #[test]
    fn malformed_or_oversized_transport_is_rejected() {
        for data in [
            b"Content-Length: 0\r\n\r\n".to_vec(),
            b"Content-Length: 8388609\r\n\r\n".to_vec(),
            b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}".to_vec(),
            vec![b'x'; 20_000],
            b"Content-Length: 9\r\n\r\n{}".to_vec(),
        ] {
            assert!(frame(&mut std::io::Cursor::new(data)).is_err());
        }
    }
}
