use super::*;

impl LspServer {
    pub(super) fn handle_message(&mut self, message: Value) -> Result<Vec<Value>, LspError> {
        let Some(object) = message.as_object() else {
            return Ok(vec![
                RpcError::new(INVALID_REQUEST, "request must be a JSON object")
                    .response(Value::Null),
            ]);
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION) {
            return Ok(vec![
                RpcError::new(INVALID_REQUEST, "jsonrpc must be \"2.0\"")
                    .response(object.get("id").cloned().unwrap_or(Value::Null)),
            ]);
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            // Responses from a client are not part of Phase 2's server input stream.
            if object.contains_key("result") || object.contains_key("error") {
                return Ok(Vec::new());
            }
            return Ok(vec![
                RpcError::new(INVALID_REQUEST, "request method is missing")
                    .response(object.get("id").cloned().unwrap_or(Value::Null)),
            ]);
        };
        let id_value = object.get("id").cloned();
        let request_id = match id_value.as_ref() {
            Some(Value::Null) => None,
            Some(value) => match RequestId::parse(value) {
                Ok(request_id) => Some(request_id),
                Err(error) => return Ok(vec![error.response(Value::Null)]),
            },
            None => None,
        };

        if method == "$/cancelRequest" {
            self.handle_cancel(object.get("params"));
            return Ok(Vec::new());
        }

        let result = self.dispatch_method(method, object.get("params"), request_id.as_ref());
        match (id_value, result) {
            (Some(id), Ok(value)) => Ok(vec![
                json!({"jsonrpc": JSON_RPC_VERSION, "id": id, "result": value}),
            ]),
            (Some(id), Err(error)) => Ok(vec![error.response(id)]),
            (None, Ok(value)) if value != Value::Null => Ok(vec![value]),
            (None, _) => Ok(Vec::new()),
        }
    }

    fn dispatch_method(
        &mut self,
        method: &str,
        params: Option<&Value>,
        request_id: Option<&RequestId>,
    ) -> Result<Value, RpcError> {
        if method == "exit" {
            self.clean_exit = self.state == ServerState::ShuttingDown;
            self.state = ServerState::Exited;
            return Ok(Value::Null);
        }
        if method == "initialize" {
            if self.state != ServerState::Uninitialized {
                return Err(RpcError::new(
                    INVALID_REQUEST,
                    "server is already initialized",
                ));
            }
            return self.handle_initialize(params);
        }
        if let Some(request_id) = request_id
            && self.cancelled.remove(request_id)
        {
            return Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"));
        }
        if matches!(
            self.state,
            ServerState::Uninitialized | ServerState::Initializing
        ) {
            return Err(RpcError::new(
                SERVER_NOT_INITIALIZED,
                "server is not initialized",
            ));
        }
        if self.state == ServerState::ShuttingDown {
            return Err(RpcError::new(
                SERVER_NOT_INITIALIZED,
                "server is shutting down",
            ));
        }

        // Every client interaction resets the quiet-pass idle clock. This is intentionally broad
        // (including navigation and configuration notifications): a request arriving while the
        // user is working should never race a background disk walk.
        self.mark_activity();

        match method {
            "initialized" => Ok(self.watcher_registration.take().unwrap_or(Value::Null)),
            "shutdown" => {
                self.state = ServerState::ShuttingDown;
                Ok(Value::Null)
            }
            "textDocument/didOpen" => {
                let uri = self.handle_did_open(params)?;
                self.schedule_parse(&uri);
                Ok(Value::Null)
            }
            "textDocument/didChange" => {
                let uri = self.handle_did_change(params)?;
                self.schedule_parse(&uri);
                Ok(Value::Null)
            }
            "textDocument/didClose" => self.handle_did_close(params),
            "textDocument/didSave" => {
                let params = typed_params::<DidSaveTextDocumentParams>(params, "didSave")?;
                let uri = params.text_document.uri.as_str();
                if let Ok(path) = parse_file_uri_str(uri) {
                    self.pending_disk_changes
                        .insert(normalize_workspace_path(path), DiskFileChangeKind::Changed);
                }
                self.schedule_parse(uri);
                self.schedule_diagnostics(uri, Duration::ZERO);
                Ok(Value::Null)
            }
            "workspace/didChangeWatchedFiles" => {
                self.handle_did_change_watched_files(params)?;
                Ok(Value::Null)
            }
            "workspace/didChangeConfiguration" => {
                self.handle_did_change_configuration(params)?;
                Ok(Value::Null)
            }
            "workspace/executeCommand" => Err(RpcError::new(
                INVALID_REQUEST,
                "only the pdx/reindexWorkspace and validateWorkspace commands are supported",
            )),
            method if is_snapshot_request(method) => SnapshotRequestContext::new(
                self.host.snapshot(),
                CancellationToken::new(),
                self.client_snippet_support,
                self.textures.clone(),
                Arc::clone(&self.ignored_diagnostic_codes),
            )
            .dispatch(method, params),
            _ => Err(RpcError::new(METHOD_NOT_FOUND, "method is not implemented")),
        }
    }

    fn handle_cancel(&mut self, params: Option<&Value>) {
        if let Ok(params) = typed_params::<CancelParams>(params, "cancel request") {
            self.cancelled.insert(request_id_from_lsp(params.id));
        }
    }

    fn handle_initialize(&mut self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<InitializeParams>(params, "initialize")?;
        self.client_work_done_progress = params
            .capabilities
            .window
            .as_ref()
            .and_then(|window| window.work_done_progress)
            .unwrap_or(false);
        self.client_snippet_support = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|text_document| text_document.completion.as_ref())
            .and_then(|completion| completion.completion_item.as_ref())
            .and_then(|item| item.snippet_support)
            .unwrap_or(false);
        let prepared = prepare_initialize_candidate(
            self.host.clone(),
            params,
            !self.host.snapshot().rules().game_id().is_empty(),
            None,
            &WorkspaceScanToken::new(),
            &InitializeCallbacks {
                stage: None,
                log: None,
                progress: None,
            },
        )?;
        self.host = prepared.host;
        self.textures = prepared.textures;
        self.watcher_registration = prepared.watcher_registration;
        self.background_reindex_interval_minutes = prepared.background_reindex_interval_minutes;
        self.background_reindex_idle_seconds = prepared.background_reindex_idle_seconds;
        self.ignored_diagnostic_codes =
            Arc::new(prepared.ignored_diagnostic_codes.iter().cloned().collect());
        self.workspace_wide_diagnostics = prepared.workspace_wide_diagnostics;
        self.last_activity = Instant::now();
        self.state = ServerState::Initialized;
        Ok(prepared.result)
    }

    fn handle_did_open(&mut self, params: Option<&Value>) -> Result<String, RpcError> {
        let params = typed_params::<DidOpenTextDocumentParams>(params, "didOpen")?;
        let uri = params.text_document.uri.as_str().to_owned();
        let version = i64::from(params.text_document.version);
        let text = params.text_document.text;
        changed_document_len(0, None, text.len())?;
        let path = uri_to_path(&uri).ok().map(normalize_workspace_path);
        self.host
            .stage_open_document(DocumentId::new(uri.clone()), version, text, path)
            .map_err(document_error)?;
        Ok(uri)
    }

    fn handle_did_change(&mut self, params: Option<&Value>) -> Result<String, RpcError> {
        let params = typed_params::<DidChangeTextDocumentParams>(params, "didChange")?;
        let uri = params.text_document.uri.as_str().to_owned();
        let id = DocumentId::new(uri.clone());
        let version = i64::from(params.text_document.version);
        let snapshot = self.host.snapshot();
        let current = snapshot
            .document(&id)
            .filter(|document| document.source() == DocumentSource::Overlay)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "didChange document is not open"))?;
        let mut text = current.text().to_owned();
        let mut line_index = current.line_index().clone();
        for change in params.content_changes {
            let range = change
                .range
                .as_ref()
                .map(|range| lsp_range_to_text_range(range, &line_index, &text))
                .transpose()?;
            changed_document_len(text.len(), range, change.text.len())?;
            apply_text_change(&mut text, range, &change.text)?;
            line_index = LineIndex::new(&text);
        }
        self.host
            .stage_document_text(&id, version, text)
            .map_err(document_error)?;
        Ok(uri)
    }

    fn handle_did_close(&mut self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<DidCloseTextDocumentParams>(params, "didClose")?;
        let uri = params.text_document.uri.as_str().to_owned();
        let id = DocumentId::new(uri.clone());
        self.host.close_document(&id).map_err(document_error)?;
        self.pending_parses.remove(&id);
        self.pending_diagnostics.remove(&id);
        self.diagnostics.remove(&id);
        Ok(json!({
            "jsonrpc": JSON_RPC_VERSION,
            "method": "textDocument/publishDiagnostics",
            "params": {"uri": uri, "diagnostics": []}
        }))
    }

    fn handle_did_change_watched_files(&mut self, params: Option<&Value>) -> Result<(), RpcError> {
        let params = typed_params::<DidChangeWatchedFilesParams>(params, "didChangeWatchedFiles")?;
        for event in params.changes {
            let path = normalize_workspace_path(parse_file_uri_str(event.uri.as_str())?);
            let kind = if event.typ == FileChangeType::CREATED {
                DiskFileChangeKind::Created
            } else if event.typ == FileChangeType::CHANGED {
                DiskFileChangeKind::Changed
            } else if event.typ == FileChangeType::DELETED {
                DiskFileChangeKind::Deleted
            } else {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    "didChangeWatchedFiles contains an unsupported change type",
                ));
            };
            self.queue_watched_disk_change(path, kind);
        }
        Ok(())
    }

    fn handle_did_change_configuration(&mut self, params: Option<&Value>) -> Result<(), RpcError> {
        #[derive(Default, serde::Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct DidChangeConfigurationParams {
            settings: Value,
        }

        let params =
            typed_params::<DidChangeConfigurationParams>(params, "didChangeConfiguration")?;
        let Some(settings) = params.settings.as_object() else {
            return Ok(());
        };
        let interval = settings
            .get("backgroundReindexIntervalMinutes")
            .and_then(Value::as_u64);
        let idle = settings
            .get("backgroundReindexIdleSeconds")
            .and_then(Value::as_u64);
        let interval_changed = interval.is_some();
        if let Some(interval) = interval.filter(|value| *value <= 7 * 24 * 60) {
            self.background_reindex_interval_minutes = interval;
        }
        if let Some(idle) = idle.filter(|value| *value <= 24 * 60 * 60) {
            self.background_reindex_idle_seconds = idle;
        }
        let file_patterns = settings
            .get("ignoreFilePatterns")
            .or_else(|| settings.get("ignoreFiles"))
            .map(parse_ignore_patterns)
            .transpose()?;
        let directory_patterns = settings
            .get("ignoreDirectories")
            .or_else(|| settings.get("ignoreDirs"))
            .map(parse_ignore_patterns)
            .transpose()?;
        let ignored_diagnostic_codes = settings
            .get("ignoredErrorCodes")
            .or_else(|| settings.get("ignoreDiagnosticCodes"))
            .map(parse_ignored_diagnostic_codes)
            .transpose()?;
        let workspace_wide_diagnostics = settings
            .get("workspaceWideDiagnostics")
            .or_else(|| settings.get("workspace_wide_diagnostics"))
            .and_then(Value::as_bool);
        if file_patterns.is_some() || directory_patterns.is_some() {
            let current = self.host.scan_filters();
            let filters = WorkspaceScanFilters::new(
                file_patterns.unwrap_or_else(|| current.ignore_file_patterns().to_vec()),
                directory_patterns.unwrap_or_else(|| current.ignore_directory_patterns().to_vec()),
            )
            .map_err(|error| {
                RpcError::new(
                    INVALID_PARAMS,
                    format!("invalid workspace ignore filters: {error}"),
                )
            })?;
            self.host.set_scan_filters(filters);
        }
        if let Some(codes) = ignored_diagnostic_codes {
            self.ignored_diagnostic_codes = Arc::new(codes.into_iter().collect());
            let open = self
                .host
                .snapshot()
                .documents()
                .iter()
                .filter_map(|(id, document)| {
                    document.version().map(|version| (id.clone(), version))
                })
                .collect::<Vec<_>>();
            for (id, version) in open {
                self.schedule_diagnostics_for_document(id, version, Duration::ZERO);
            }
        }
        if let Some(enabled) = workspace_wide_diagnostics
            && enabled != self.workspace_wide_diagnostics
        {
            self.workspace_wide_diagnostics = enabled;
            if !enabled {
                self.workspace_diagnostic_clear_queue
                    .extend(self.workspace_diagnostic_uris.iter().cloned());
                self.workspace_diagnostic_uris.clear();
            }
        }
        if interval_changed {
            self.arm_background_reindex();
        }
        Ok(())
    }
}

fn parse_ignore_patterns(value: &Value) -> Result<Vec<String>, RpcError> {
    let Some(values) = value.as_array() else {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "workspace ignore filters must be arrays of strings",
        ));
    };
    values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                RpcError::new(
                    INVALID_PARAMS,
                    "workspace ignore filters must be arrays of strings",
                )
            })
        })
        .collect()
}

fn parse_ignored_diagnostic_codes(value: &Value) -> Result<Vec<String>, RpcError> {
    let Some(values) = value.as_array() else {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "ignoredErrorCodes must be an array of strings",
        ));
    };
    let values = values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                RpcError::new(
                    INVALID_PARAMS,
                    "ignoredErrorCodes must be an array of strings",
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    crate::workspace::normalize_ignored_diagnostic_codes(values)
}
