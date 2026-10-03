//! A bounded real-process LSP client shared by audits, measurements and transport tests.
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const MAX_FRAME: usize = 64 * 1024 * 1024;
fn request_value(id: u64, method: &str, params: Value) -> Value {
    let mut value = json!({"jsonrpc":"2.0","id":id,"method":method});
    if !params.is_null() {
        value["params"] = params;
    }
    value
}
pub fn read_frame(reader: &mut impl BufRead) -> Result<Option<Value>, String> {
    let mut length = None;
    let mut total = 0;
    loop {
        let mut line = String::new();
        let size = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if size == 0 {
            return if total == 0 {
                Ok(None)
            } else {
                Err("truncated LSP header".into())
            };
        }
        total += size;
        if total > 8192 {
            return Err("LSP header limit exceeded".into());
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':')
            && key.eq_ignore_ascii_case("Content-Length")
        {
            if length.is_some() {
                return Err("duplicate Content-Length".into());
            }
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| "invalid Content-Length")?,
            );
        }
    }
    let length = length
        .filter(|n| *n <= MAX_FRAME)
        .ok_or("missing or oversized Content-Length")?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| e.to_string())
}
pub fn write_frame(writer: &mut impl Write, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FRAME {
        return Err("LSP frame limit exceeded".into());
    }
    write!(writer, "Content-Length: {}\r\n\r\n", bytes.len()).map_err(|e| e.to_string())?;
    writer
        .write_all(&bytes)
        .and_then(|_| writer.flush())
        .map_err(|e| e.to_string())
}
pub fn uri(path: &Path) -> Result<String, String> {
    url::Url::from_file_path(std::path::absolute(path).map_err(|e| e.to_string())?)
        .map(|u| u.into())
        .map_err(|_| "invalid file URI".into())
}
pub struct Client {
    child: Child,
    input: Option<ChildStdin>,
    incoming: mpsc::Receiver<Result<Value, String>>,
    pending: VecDeque<Value>,
    pub notifications: Vec<Value>,
    stderr: Arc<Mutex<String>>,
    next_id: u64,
    timeout: Duration,
}
impl Client {
    pub fn spawn(binary: &Path, workspace: &Path, timeout: Duration) -> Result<Self, String> {
        let mut child = Command::new(binary)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("server {}: {e}", binary.display()))?;
        let input = child.stdin.take();
        let stdout = child.stdout.take().ok_or("missing server stdout")?;
        let stderr = Arc::new(Mutex::new(String::new()));
        let captured = stderr.clone();
        let errors = child.stderr.take().ok_or("missing server stderr")?;
        std::thread::spawn(move || {
            for line in BufReader::new(errors).lines() {
                match line {
                    Ok(line) => {
                        let mut output = captured.lock().unwrap();
                        if output.len() < 1024 * 1024 {
                            output.push_str(&line);
                            output.push('\n');
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let (sender, incoming) = mpsc::sync_channel(256);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_frame(&mut reader) {
                    Ok(Some(value)) => {
                        if sender.send(Ok(value)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = sender.send(Err("server transport closed".into()));
                        break;
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            input,
            incoming,
            pending: VecDeque::new(),
            notifications: Vec::new(),
            stderr,
            next_id: 1,
            timeout,
        })
    }
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    pub fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }
    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(&json!({"jsonrpc":"2.0","method":method,"params":params}))
    }
    fn send(&mut self, value: &Value) -> Result<(), String> {
        write_frame(self.input.as_mut().ok_or("LSP input closed")?, value)
    }
    fn receive(&mut self, deadline: Instant) -> Result<Value, String> {
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or("LSP timeout")?;
            let value = self
                .incoming
                .recv_timeout(remaining)
                .map_err(|e| format!("LSP receive: {e}\n{}", self.stderr()))??;
            if let Some(method) = value["method"].as_str() {
                if let Some(id) = value.get("id") {
                    let result = match method {
                        "workspace/configuration" => json!(
                            value["params"]["items"]
                                .as_array()
                                .map(|a| vec![json!({}); a.len()])
                                .unwrap_or_default()
                        ),
                        "workspace/workspaceFolders" => json!([]),
                        "client/registerCapability"
                        | "client/unregisterCapability"
                        | "window/workDoneProgress/create" => Value::Null,
                        _ => {
                            self.send(&json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"client method not implemented"}}))?;
                            continue;
                        }
                    };
                    self.send(&json!({"jsonrpc":"2.0","id":id,"result":result}))?;
                } else {
                    if self.notifications.len() >= 100_000 {
                        return Err("LSP notification limit exceeded".into());
                    }
                    self.notifications.push(value.clone());
                    return Ok(value);
                }
            } else {
                return Ok(value);
            }
        }
    }
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.request_timeout(method, params, self.timeout)
    }
    pub fn request_many(
        &mut self,
        requests: &[(&str, Value)],
        timeout: Duration,
    ) -> Result<Vec<Value>, String> {
        let mut ids = Vec::new();
        for (method, params) in requests {
            let id = self.next_id;
            self.next_id += 1;
            self.send(&request_value(id, method, params.clone()))?;
            ids.push(id);
        }
        let deadline = Instant::now() + timeout;
        let mut results = std::collections::BTreeMap::new();
        while results.len() < ids.len() {
            let value = self.receive(deadline)?;
            if value.get("method").is_some() {
                continue;
            }
            let id = value["id"].as_u64().ok_or("invalid batch response ID")?;
            if !ids.contains(&id) {
                self.pending.push_back(value);
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(format!("LSP batch: {error}"));
            }
            if results
                .insert(
                    id,
                    value
                        .get("result")
                        .cloned()
                        .ok_or("batch response has no result")?,
                )
                .is_some()
            {
                return Err("duplicate batch response".into());
            }
        }
        Ok(ids
            .into_iter()
            .map(|id| results.remove(&id).unwrap())
            .collect())
    }
    pub fn request_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&request_value(id, method, params.clone()))?;
        let deadline = Instant::now() + timeout;
        loop {
            let value = if let Some(index) = self.pending.iter().position(|v| v["id"] == id) {
                self.pending.remove(index).unwrap()
            } else {
                self.receive(deadline)?
            };
            if value.get("method").is_some() {
                continue;
            }
            if value["id"] != id {
                self.pending.push_back(value);
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(format!("{method}: {error}"));
            }
            return value
                .get("result")
                .cloned()
                .ok_or_else(|| format!("{method}: response has no result"));
        }
    }
    pub fn wait_notification(
        &mut self,
        method: &str,
        predicate: impl Fn(&Value) -> bool,
    ) -> Result<Value, String> {
        self.wait_notification_since(method, 0, self.timeout, predicate)
    }
    pub fn wait_notification_since(
        &mut self,
        method: &str,
        since: usize,
        timeout: Duration,
        predicate: impl Fn(&Value) -> bool,
    ) -> Result<Value, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(value) = self
                .notifications
                .iter()
                .skip(since)
                .find(|v| v["method"] == method && predicate(&v["params"]))
            {
                return Ok(value["params"].clone());
            }
            let value = self.receive(deadline)?;
            if value.get("method").is_none() {
                self.pending.push_back(value);
            }
        }
    }
    pub fn initialize(&mut self, workspace: &Path, options: Value) -> Result<Value, String> {
        let result = self.handshake(workspace, options)?;
        self.wait_notification("pdc/ready", |_| true)?;
        Ok(result)
    }
    pub fn handshake(&mut self, workspace: &Path, options: Value) -> Result<Value, String> {
        let result=self.request("initialize",json!({"processId":std::process::id(),"clientInfo":{"name":"paradoxcode-tools","version":env!("CARGO_PKG_VERSION")},"workspaceFolders":[{"uri":uri(workspace)?,"name":"tools-workspace"}],"capabilities":{"window":{"workDoneProgress":true},"workspace":{"didChangeWatchedFiles":{"dynamicRegistration":false}},"textDocument":{"completion":{"completionItem":{"snippetSupport":false}}}},"initializationOptions":options}))?;
        self.notify("initialized", json!({}))?;
        Ok(result)
    }
    pub fn open(&mut self, path: &Path, text: &str, version: i64) -> Result<(), String> {
        self.notify("textDocument/didOpen",json!({"textDocument":{"uri":uri(path)?,"languageId":if path.extension().is_some_and(|e|e=="yml"){"localisation"}else{"eu4"},"version":version,"text":text}}))
    }
    pub fn close(&mut self, path: &Path) -> Result<(), String> {
        self.notify(
            "textDocument/didClose",
            json!({"textDocument":{"uri":uri(path)?}}),
        )
    }
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.request_timeout("shutdown", Value::Null, Duration::from_secs(10))?;
        self.notify("exit", Value::Null)?;
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().map_err(|e| e.to_string())? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("server exit: {status}"))
                };
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err("server failed to exit after shutdown".into())
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frames_use_utf8_bytes_and_reject_truncation_duplicate_and_oversize() {
        let value = json!({"text":"中文"});
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &value).unwrap();
        assert_eq!(read_frame(&mut bytes.as_slice()).unwrap(), Some(value));
        for bad in [
            b"Content-Length: 20\r\n\r\n{}".as_slice(),
            b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
            b"Content-Length: 9999999999\r\n\r\n",
        ] {
            assert!(read_frame(&mut bad.to_owned().as_slice()).is_err());
        }
    }
}
