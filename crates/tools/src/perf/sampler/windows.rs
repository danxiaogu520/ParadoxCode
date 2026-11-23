//! Keep one observer process alive for the complete measurement window.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

pub(super) struct Observer {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Observer {
    pub(super) fn new(pid: u32) -> Result<Self, String> {
        let script = format!(
            "$ErrorActionPreference='Stop'; $p=[Diagnostics.Process]::GetProcessById({pid}); \
             while ($null -ne [Console]::ReadLine()) {{ \
               $p.Refresh(); if ($p.HasExited) {{ break }}; \
               [Console]::WriteLine($p.TotalProcessorTime.Ticks.ToString()+' '+$p.WorkingSet64.ToString()) \
             }}"
        );
        let mut child = crate::process::command("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let input = child.stdin.take().ok_or("observer stdin unavailable")?;
        let output = BufReader::new(child.stdout.take().ok_or("observer stdout unavailable")?);
        Ok(Self {
            child,
            input,
            output,
        })
    }

    pub(super) fn sample(&mut self) -> Result<Value, String> {
        self.input
            .write_all(b"\n")
            .and_then(|()| self.input.flush())
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        if self
            .output
            .read_line(&mut line)
            .map_err(|e| e.to_string())?
            == 0
        {
            return Err("process observer closed before a sample was available".into());
        }
        let (ticks, bytes) = line
            .trim()
            .split_once(' ')
            .ok_or("invalid Windows process sample")?;
        Ok(
            json!({"cpu_seconds":ticks.parse::<u64>().map_err(|e|e.to_string())? as f64 / 10_000_000.0,
            "working_set_bytes":bytes.parse::<u64>().map_err(|e|e.to_string())?}),
        )
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
