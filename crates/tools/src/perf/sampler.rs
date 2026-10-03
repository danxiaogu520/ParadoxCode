//! Periodic child CPU/RSS observations; missing samples remain explicit.
use crate::process;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub fn sample(pid: u32) -> Result<Value, String> {
    if cfg!(windows) {
        let script = format!(
            "$p = Get-Process -Id {pid} -ErrorAction Stop; Write-Output ($p.CPU.ToString([Globalization.CultureInfo]::InvariantCulture) + ' ' + $p.WorkingSet64)"
        );
        let output = process::capture(process::command("powershell").args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ]))?;
        let text = String::from_utf8_lossy(&output.stdout);
        let parts = text.split_whitespace().collect::<Vec<_>>();
        if parts.len() != 2 {
            return Err("invalid Windows process sample".into());
        }
        return Ok(
            json!({"cpu_seconds":parts[0].parse::<f64>().map_err(|e|e.to_string())?,"working_set_bytes":parts[1].parse::<u64>().map_err(|e|e.to_string())?}),
        );
    }
    let output = process::capture(process::command("ps").args([
        "-o",
        "time=,rss=",
        "-p",
        &pid.to_string(),
    ]))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let parts = text.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 2 {
        return Err("invalid Unix process sample".into());
    }
    Ok(
        json!({"cpu_seconds":cpu_time(parts[0])?,"working_set_bytes":parts[1].parse::<u64>().map_err(|e|e.to_string())?*1024}),
    )
}
fn cpu_time(text: &str) -> Result<f64, String> {
    let (days, time) = text
        .split_once('-')
        .map(|(day, time)| day.parse::<f64>().map(|day| (day, time)))
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or((0.0, text));
    let mut value = days * 86400.0;
    let mut subtotal = 0.0;
    for part in time.split(':') {
        subtotal = subtotal * 60.0 + part.parse::<f64>().map_err(|e| e.to_string())?;
    }
    value += subtotal;
    Ok(value)
}
pub struct Sampler {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    data: Arc<Mutex<Value>>,
}
impl Sampler {
    pub fn start(pid: u32, interval: Duration) -> Self {
        let data = Arc::new(Mutex::new(
            json!({"pid":pid,"samples":0,"sample_errors":[],"peak_working_set_bytes":null,"last":null,"method":"periodic process RSS and cumulative CPU; peak is sampled, not wait4"}),
        ));
        let captured = data.clone();
        let (stop, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let started = Instant::now();
            loop {
                match sample(pid) {
                    Ok(mut sample) => {
                        sample["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
                        let mut data = captured.lock().unwrap();
                        data["samples"] = json!(data["samples"].as_u64().unwrap() + 1);
                        let peak = data["peak_working_set_bytes"]
                            .as_u64()
                            .unwrap_or(0)
                            .max(sample["working_set_bytes"].as_u64().unwrap());
                        data["peak_working_set_bytes"] = json!(peak);
                        data["last"] = sample;
                    }
                    Err(error) => {
                        let mut data = captured.lock().unwrap();
                        if data["sample_errors"].as_array().unwrap().len() < 32 {
                            data["sample_errors"]
                                .as_array_mut()
                                .unwrap()
                                .push(json!(error));
                        }
                    }
                }
                if rx.recv_timeout(interval).is_ok() {
                    break;
                }
            }
        });
        Self {
            stop: Some(stop),
            thread: Some(thread),
            data,
        }
    }
    pub fn finish(&mut self) -> Value {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.data.lock().unwrap().clone()
    }
    pub fn snapshot(&self) -> Value {
        self.data.lock().unwrap().clone()
    }
}
impl Drop for Sampler {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_platform_cpu_time() {
        assert_eq!(cpu_time("1-02:03:04.50").unwrap(), 93784.5);
        assert_eq!(cpu_time("00:01.25").unwrap(), 1.25);
    }
}
