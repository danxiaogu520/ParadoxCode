use super::fixture::{Fixture, GFX, GFX_PATH, LOC, LOC_PATH, SCRIPT_PATH, TERRAIN, TERRAIN_PATH};
use super::metrics::record;
use super::queries::{Expect, Session};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use std::{fs, thread};

/// Delta edit offsets address the original flat integer array, in units of u32.
fn updated_tokens(previous: &Value, delta: &Value) -> Result<Vec<Value>, String> {
    if let Some(full) = delta["data"].as_array() {
        return Ok(full.clone());
    }
    let mut tokens = previous["data"]
        .as_array()
        .ok_or("missing previous tokens")?
        .clone();
    let mut limit = tokens.len();
    for edit in delta["edits"]
        .as_array()
        .ok_or("missing semantic token edits")?
        .iter()
        .rev()
    {
        let number = |key: &str| {
            edit[key]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or("invalid token edit index")
        };
        let start = number("start")?;
        let end = start
            .checked_add(number("deleteCount")?)
            .filter(|end| *end <= limit)
            .ok_or("overlapping or out-of-range token edit")?;
        let replacement = match edit.get("data") {
            None => &[][..],
            Some(data) => data
                .as_array()
                .ok_or("invalid replacement tokens")?
                .as_slice(),
        };
        tokens.splice(start..end, replacement.iter().cloned());
        limit = start;
    }
    Ok(tokens)
}

impl Session<'_> {
    pub fn open_documents(&mut self, f: &Fixture) -> Result<(), String> {
        for (path, text, name) in [
            (SCRIPT_PATH, f.script.as_str(), "script"),
            (TERRAIN_PATH, TERRAIN, "terrain"),
            (GFX_PATH, GFX, "interface"),
            (LOC_PATH, LOC, "localisation"),
            (
                super::fixture::EFFECTS_PATH,
                super::fixture::EFFECTS,
                "effects",
            ),
        ] {
            let uri = f.document(path)?["uri"].clone();
            let since = self.client.notifications.len();
            let start = Instant::now();
            self.client.open(&f.workspace.join(path), text, 1)?;
            let published = self.client.wait_notification_since(
                "textDocument/publishDiagnostics",
                since,
                self.timeout,
                |p| p["uri"] == uri && p["version"] == 1,
            )?;
            let diagnostics = published["diagnostics"]
                .as_array()
                .ok_or("invalid diagnostic publication")?;
            if path == TERRAIN_PATH && !diagnostics.iter().any(|d| d["code"] == "InvalidValue") {
                return Err("terrain open did not diagnose the invalid enum".into());
            }
            record(
                self.measurements,
                &format!("open/{name}-to-publication"),
                start.elapsed(),
                Some(diagnostics.len()),
            );
        }
        Ok(())
    }

    pub fn edit_diagnostics(&mut self, f: &Fixture) -> Result<(), String> {
        let doc = f.document(TERRAIN_PATH)?;
        let col = TERRAIN.find("foresst").unwrap();
        let edit = json!({"range":{"start":{"line":0,"character":col},"end":{"line":0,"character":col+7}},"text":"forest"});
        for (mode, change, version, has_error) in [
            ("incremental", edit, 2, false),
            ("full", json!({"text":TERRAIN}), 3, true),
        ] {
            let since = self.client.notifications.len();
            let started = Instant::now();
            self.client.notify("textDocument/didChange",json!({"textDocument":{"uri":doc["uri"],"version":version},"contentChanges":[change]}))?;
            let published = self.client.wait_notification_since(
                "textDocument/publishDiagnostics",
                since,
                self.timeout,
                |p| p["uri"] == doc["uri"] && p["version"] == version,
            )?;
            let list = published["diagnostics"]
                .as_array()
                .ok_or("missing diagnostics")?;
            if list.iter().any(|d| d["code"] == "InvalidValue") != has_error {
                return Err(format!("{mode} edit published stale diagnostics"));
            }
            record(
                self.measurements,
                &format!("edit/{mode}-to-publication"),
                started.elapsed(),
                Some(list.len()),
            );
        }
        Ok(())
    }

    pub fn foreground_burst(&mut self, f: &Fixture) -> Result<(), String> {
        let doc = f.document(SCRIPT_PATH)?;
        let requests = [
            (
                "textDocument/hover",
                json!({"textDocument":doc,"position":f.position("add_pres")}),
            ),
            (
                "textDocument/completion",
                json!({"textDocument":doc,"position":f.position("add_pres")}),
            ),
            (
                "textDocument/semanticTokens/full",
                json!({"textDocument":doc}),
            ),
            ("workspace/symbol", json!({"query":"perf_effect"})),
        ];
        let start = Instant::now();
        let mut pending = Vec::new();
        for (method, params) in requests {
            let sent = Instant::now();
            pending.push((method, sent, self.client.send_request(method, params)?));
        }
        let mut last = start;
        for ((method, sent, id), pointer) in
            pending
                .into_iter()
                .zip(["/contents", "/items", "/data", ""])
        {
            let (response, at) = self.client.wait_response(id, self.timeout)?;
            let count = Expect::Populated(pointer)
                .check(&response["result"])
                .map_err(|e| format!("burst {method}: {e}"))?;
            record(
                self.measurements,
                &format!("burst/{method}"),
                at.duration_since(sent),
                Some(count),
            );
            last = last.max(at);
        }
        record(
            self.measurements,
            "burst/all-responses",
            last.duration_since(start),
            Some(4),
        );
        Ok(())
    }

    /// One obsolete completion followed immediately by an edit requesting a different candidate.
    /// The reader timestamps both frames even when the newer response arrives first.
    pub fn typing(
        &mut self,
        f: &Fixture,
        version: &mut i64,
        previous: &mut String,
        reindex: bool,
    ) -> Result<Value, String> {
        let doc = f.document(SCRIPT_PATH)?;
        let anchor = f.position("perf_effect = yes ");
        let token_params = json!({"textDocument":doc});
        let previous_tokens = self
            .client
            .request("textDocument/semanticTokens/full", token_params.clone())?;
        let reindex_id = if reindex {
            Some(self.client.send_request(
                "workspace/executeCommand",
                json!({"command":"pdc/reindexWorkspace","arguments":[]}),
            )?)
        } else {
            None
        };
        let mut end = anchor.clone();
        end["character"] = json!(anchor["character"].as_u64().unwrap() + previous.len() as u64);
        let old = self.client.send_request(
            "textDocument/completion",
            json!({"textDocument":doc,"position":end}),
        )?;
        let cancelled_at = Instant::now();
        self.client.notify("$/cancelRequest", json!({"id":old}))?;
        let (prefix, expected) = if previous == "add_pre" {
            ("add_trea", "add_treasury")
        } else {
            ("add_pre", "add_prestige")
        };
        let mut position = anchor.clone();
        position["character"] = json!(anchor["character"].as_u64().unwrap() + prefix.len() as u64);
        *version += 1;
        let params = json!({"textDocument":doc,"position":position});
        let change = json!({"textDocument":{"uri":doc["uri"],"version":version},"contentChanges":[{"range":{"start":anchor,"end":end},"text":prefix}]});
        let started = Instant::now();
        self.client.notify("textDocument/didChange", change)?;
        let id = self
            .client
            .send_request("textDocument/completion", params.clone())?;
        let (response, at) = self.client.wait_response(id, self.timeout)?;
        let overlap = if let Some(id) = reindex_id {
            let (result, completed) = self.client.wait_response(id, self.timeout)?;
            let error = result.get("error");
            if let Some(error) = error
                && !(error["code"] == -32600
                    && error["message"]
                        .as_str()
                        .is_some_and(|m| m.contains("workspace changed while reindexing")))
            {
                return Err(format!("overlapping reindex: {error}"));
            }
            json!({"edit_sent_before_response":started<=completed,"obsolete":error.is_some()})
        } else {
            Value::Null
        };
        let list = response["result"]["items"]
            .as_array()
            .ok_or("typing: latest request failed or was cancelled")?;
        if !list.iter().any(|item| item["label"] == expected)
            || crate::perf::completion::normalize(
                self.client.request("textDocument/completion", params)?,
            ) != crate::perf::completion::normalize(response["result"].clone())
        {
            return Err(format!(
                "typing: latest completion does not match settled {expected}"
            ));
        }
        record(
            self.measurements,
            &format!(
                "{}/edit-to-{expected}",
                if reindex { "typing-reindex" } else { "typing" }
            ),
            at.duration_since(started),
            Some(list.len()),
        );
        let (stale, received) = self.client.wait_response(old, self.timeout)?;
        if stale.get("error").is_some() && stale["error"]["code"] != -32800 {
            return Err(format!("obsolete completion: {}", stale["error"]));
        }
        let delta = self.query(
            "edit/tokens-delta",
            "textDocument/semanticTokens/full/delta",
            json!({"textDocument":doc,"previousResultId":previous_tokens["resultId"]}),
            Expect::Populated("/resultId"),
        )?;
        let full = self
            .client
            .request("textDocument/semanticTokens/full", token_params)?;
        if Value::Array(updated_tokens(&previous_tokens, &delta)?) != full["data"] {
            return Err("token changes do not reconstruct the latest full result".into());
        }
        *previous = prefix.to_owned();
        Ok(
            json!({"cancelled":stale["error"]["code"]==-32800,"response_after_cancel_ms":received.checked_duration_since(cancelled_at).map(|d|d.as_secs_f64()*1000.0),"reindex":overlap}),
        )
    }

    pub fn configuration_and_close(
        &mut self,
        f: &Fixture,
        saved_version: i64,
    ) -> Result<(), String> {
        let doc = f.document(TERRAIN_PATH)?;
        let since = self.client.notifications.len();
        let start = Instant::now();
        self.client.notify(
            "workspace/didChangeConfiguration",
            json!({"settings":{"ignoredErrorCodes":["InvalidValue"]}}),
        )?;
        self.client.wait_notification_since(
            "textDocument/publishDiagnostics",
            since,
            self.timeout,
            |p| {
                p["uri"] == doc["uri"]
                    && p["version"] == 3
                    && p["diagnostics"]
                        .as_array()
                        .is_some_and(|ds| ds.iter().all(|d| d["code"] != "InvalidValue"))
            },
        )?;
        record(
            self.measurements,
            "configuration/diagnostics-republished",
            start.elapsed(),
            None,
        );

        // A saved overlay and its closed-file representation should expose the same symbol.
        let path = f.workspace.join(SCRIPT_PATH);
        let saved = format!("{}country_event = {{ id = perf_saved.1 }}\n", f.script);
        fs::write(&path, &saved).map_err(|e| e.to_string())?;
        let script = f.document(SCRIPT_PATH)?;
        self.client.notify("textDocument/didChange",json!({"textDocument":{"uri":script["uri"],"version":saved_version},"contentChanges":[{"text":saved}]}))?;
        let start = Instant::now();
        self.client
            .notify("textDocument/didSave", json!({"textDocument":script}))?;
        let since = self.client.notifications.len();
        self.client.close(&path)?;
        self.client.wait_notification_since(
            "textDocument/publishDiagnostics",
            since,
            self.timeout,
            |p| {
                p["uri"] == script["uri"]
                    && p.get("version").is_none()
                    && p["diagnostics"] == json!([])
            },
        )?;
        self.wait_symbol("perf_saved.1", 1, start)?;
        record(
            self.measurements,
            "save/close-to-disk-symbol",
            start.elapsed(),
            Some(1),
        );
        Ok(())
    }

    fn wait_symbol(
        &mut self,
        needle: &str,
        expected: usize,
        started: Instant,
    ) -> Result<(), String> {
        loop {
            let remaining = self
                .timeout
                .checked_sub(started.elapsed())
                .ok_or_else(|| format!("timed out waiting for {needle}"))?;
            let result = self.client.request_timeout(
                "workspace/symbol",
                json!({"query":needle}),
                remaining,
            )?;
            let items = result.as_array().ok_or("invalid symbol response")?;
            if items.iter().filter(|v| v["name"] == needle).count() == expected {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn watched_file(&mut self, f: &Fixture) -> Result<(), String> {
        let path = f.workspace.join("common/scripted_effects/watched.txt");
        let uri = crate::lsp::uri(&path)?;
        for (kind, needle, expected) in [
            (1, "perf_watched_a", 1),
            (2, "perf_watched_b", 1),
            (3, "perf_watched_b", 0),
        ] {
            if kind == 3 {
                fs::remove_file(&path)
            } else {
                fs::write(&path, format!("{needle} = {{ add_prestige = 2 }}\n"))
            }
            .map_err(|e| e.to_string())?;
            let started = Instant::now();
            self.client.notify(
                "workspace/didChangeWatchedFiles",
                json!({"changes":[{"uri":uri,"type":kind}]}),
            )?;
            self.wait_symbol(needle, expected, started)?;
            if kind == 2 {
                self.wait_symbol("perf_watched_a", 0, started)?;
            }
            let action = ["create", "change", "delete"][kind - 1];
            record(
                self.measurements,
                &format!("watch/{action}-to-visible"),
                started.elapsed(),
                Some(expected),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_edits_use_original_offsets_and_reject_overlap() {
        let before = json!({"data":[0,1,2,3,4,5]});
        let edits = json!({"edits":[{"start":1,"deleteCount":2,"data":[8]},
            {"start":4,"deleteCount":1,"data":[9,10]}]});
        assert_eq!(
            Value::Array(updated_tokens(&before, &edits).unwrap()),
            json!([0, 8, 3, 9, 10, 5])
        );
        let overlapping =
            json!({"edits":[{"start":1,"deleteCount":2},{"start":2,"deleteCount":2}]});
        assert!(updated_tokens(&before, &overlapping).is_err());
    }
}
