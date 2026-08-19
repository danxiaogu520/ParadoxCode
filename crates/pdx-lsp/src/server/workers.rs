use super::*;

impl LspServer {
    pub(super) fn spawn_snapshot_request<'scope, 'environment>(
        &self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut HashMap<RequestId, InFlightRequest>,
        message: &Value,
    ) -> bool {
        if self.state != ServerState::Initialized {
            return false;
        }
        let Some(object) = message.as_object() else {
            return false;
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION) {
            return false;
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return false;
        };
        if !is_snapshot_request(method) {
            return false;
        }
        let Some(id) = object.get("id").filter(|id| !id.is_null()) else {
            return false;
        };
        let Ok(request_id) = RequestId::parse(id) else {
            return false;
        };
        if in_flight.contains_key(&request_id) {
            return false;
        }

        let cancellation = CancellationToken::new();
        if self.cancelled.contains(&request_id) {
            cancellation.cancel();
        }
        let context = SnapshotRequestContext::new(
            self.host.snapshot(),
            cancellation.clone(),
            self.client_snippet_support,
            self.textures.clone(),
        );
        let method = method.to_owned();
        let params = object.get("params").cloned();
        let id = id.clone();
        let sender = event_sender.clone();
        in_flight.insert(
            request_id.clone(),
            InFlightRequest {
                cancellation: cancellation.clone(),
            },
        );
        scope.spawn(move || {
            let result = if cancellation.is_cancelled() {
                Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"))
            } else {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    context.dispatch(&method, params.as_ref())
                }))
                .unwrap_or_else(|_| {
                    Err(RpcError::new(
                        INTERNAL_ERROR,
                        "request worker failed unexpectedly",
                    ))
                });
                if cancellation.is_cancelled() {
                    Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"))
                } else {
                    result
                }
            };
            let _ = sender.send(TransportEvent::Request(SnapshotRequestResult {
                request_id,
                id,
                result,
            }));
        });
        true
    }

    pub(super) fn spawn_initialize_request<'scope, 'environment, W: Write>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut Option<InFlightInitialize>,
        message: &Value,
        output: &mut W,
        initialize_progress_token: &mut Option<String>,
    ) -> Result<bool, LspError> {
        if self.state != ServerState::Uninitialized || in_flight.is_some() {
            return Ok(false);
        }
        let Some(object) = message.as_object() else {
            return Ok(false);
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION)
            || object.get("method").and_then(Value::as_str) != Some("initialize")
        {
            return Ok(false);
        }
        let Some(id) = object.get("id").filter(|id| !id.is_null()) else {
            return Ok(false);
        };
        let Ok(request_id) = RequestId::parse(id) else {
            return Ok(false);
        };
        let Ok(params) = typed_params::<InitializeParams>(object.get("params"), "initialize")
        else {
            return Ok(false);
        };
        let client_work_done_progress = params
            .capabilities
            .window
            .as_ref()
            .and_then(|window| window.work_done_progress)
            .unwrap_or(false);

        let cancellation = WorkspaceScanToken::new();
        if self.cancelled.contains(&request_id) {
            cancellation.cancel();
        }
        let candidate = self.host.clone();
        let scan_workspace = !self.host.snapshot().rules().game_id().is_empty();
        let auto_vanilla = self.auto_vanilla.clone();
        let sender = event_sender.clone();
        let id = id.clone();
        // One progress token for the whole initialize phase: stage reports and
        // the workspace-scan counter share it, and the event loop sends the
        // terminal `end` report when the initialize worker completes.
        let progress_token = format!("pdx-init-{}", progress_nonce());
        if client_work_done_progress {
            write_message(output, &work_done_progress_create(&progress_token))?;
            write_message(
                output,
                &work_done_progress_begin(&progress_token, "Starting pdx-ls…"),
            )?;
            *initialize_progress_token = Some(progress_token.clone());
        }
        let stage = {
            let sender = sender.clone();
            let token = progress_token.clone();
            move |message: &str| {
                if client_work_done_progress {
                    let _ = sender.send(TransportEvent::Progress(Progress {
                        params: json!({
                            "token": token,
                            "value": {"kind": "report", "message": message},
                        }),
                    }));
                }
            }
        };
        let log = {
            let sender = sender.clone();
            move |message: &str| {
                let _ = sender.send(TransportEvent::Log(log_message_notification(
                    MessageType::INFO,
                    message.to_owned(),
                )));
            }
        };
        let progress: Option<Box<dyn Fn(usize, usize) + Send + Sync>> = if client_work_done_progress
        {
            Some(Box::new(progress_sender(
                sender.clone(),
                progress_token.clone(),
                "Scanning workspace",
                "Indexing workspace files",
            )))
        } else {
            None
        };
        // Drop the `Send` auto-trait from the shared reference inside the
        // worker closure; the callback itself is moved across threads, but
        // `prepare_initialize_candidate` only needs a `Sync` view.
        self.state = ServerState::Initializing;
        *in_flight = Some(InFlightInitialize {
            request_id: request_id.clone(),
            cancellation: cancellation.clone(),
        });
        scope.spawn(move || {
            let stage_ref: &(dyn Fn(&str) + Sync) = &stage;
            let log_ref: &(dyn Fn(&str) + Sync) = &log;
            let progress_ref: Option<&(dyn Fn(usize, usize) + Sync)> = progress
                .as_deref()
                .map(|f| f as &(dyn Fn(usize, usize) + Sync));
            let callbacks = InitializeCallbacks {
                stage: Some(stage_ref),
                log: Some(log_ref),
                progress: progress_ref,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                prepare_initialize_candidate(
                    candidate,
                    params,
                    scan_workspace,
                    auto_vanilla.as_ref(),
                    &cancellation,
                    &callbacks,
                )
            }))
            .unwrap_or_else(|_| {
                Err(RpcError::new(
                    INTERNAL_ERROR,
                    "initialize worker failed unexpectedly",
                ))
            });
            let _ = sender.send(TransportEvent::Initialize(Box::new(InitializeTaskResult {
                request_id,
                id,
                result,
            })));
        });
        Ok(true)
    }

    pub(super) fn spawn_pending_disk_changes<'scope, 'environment>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut Option<InFlightDiskChanges>,
    ) {
        if !matches!(
            self.state,
            ServerState::Initialized | ServerState::ShuttingDown
        ) || in_flight.is_some()
            || self.pending_disk_changes.is_empty()
        {
            return;
        }
        let changes = std::mem::take(&mut self.pending_disk_changes)
            .into_iter()
            .map(|(path, kind)| DiskFileChange::new(path, kind))
            .collect::<Vec<_>>();
        let base_revision = self.host.snapshot().revision();
        let cancellation = WorkspaceScanToken::new();
        let worker_cancellation = cancellation.clone();
        let worker_changes = changes.clone();
        let mut candidate = self.host.clone();
        let sender = event_sender.clone();
        *in_flight = Some(InFlightDiskChanges {
            base_revision,
            cancellation,
        });
        scope.spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                candidate
                    .apply_disk_file_changes_cancellable(&worker_changes, &worker_cancellation)
                    .map(|_| candidate)
            }))
            .unwrap_or_else(|_| {
                Err(WorkspaceError::Io(io::Error::other(
                    "workspace file-change worker failed unexpectedly",
                )))
            });
            let _ = sender.send(TransportEvent::DiskChanges(DiskChangesResult {
                base_revision,
                changes: worker_changes,
                result,
            }));
        });
    }

    pub(super) fn requeue_disk_changes(&mut self, changes: Vec<DiskFileChange>) {
        for change in changes {
            self.pending_disk_changes
                .entry(change.path)
                .or_insert(change.kind);
        }
    }

    pub(super) fn schedule_parse(&mut self, uri: &str) {
        let id = DocumentId::new(uri);
        let version = self
            .host
            .snapshot()
            .document(&id)
            .filter(|document| document.source() == DocumentSource::Overlay)
            .and_then(|document| document.version());
        if let Some(version) = version {
            self.pending_parses.insert(id, PendingParse { version });
        }
    }

    pub(super) fn cancel_stale_parses(&self, in_flight: &BTreeMap<DocumentId, InFlightParse>) {
        let snapshot = self.host.snapshot();
        for (id, task) in in_flight {
            let current_version = snapshot
                .document(id)
                .and_then(|document| document.version());
            let superseded = self
                .pending_parses
                .get(id)
                .is_some_and(|pending| pending.version != task.version);
            if current_version != Some(task.version) || superseded {
                task.cancelled.store(true, Ordering::Release);
            }
        }
    }

    pub(super) fn spawn_pending_parses<'scope, 'environment>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut BTreeMap<DocumentId, InFlightParse>,
    ) {
        let ready = self
            .pending_parses
            .keys()
            .filter(|id| !in_flight.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in ready {
            let Some(pending) = self.pending_parses.remove(&id) else {
                continue;
            };
            let snapshot = self.host.snapshot();
            let sender = event_sender.clone();
            let cancelled = Arc::new(AtomicBool::new(false));
            in_flight.insert(
                id.clone(),
                InFlightParse {
                    version: pending.version,
                    cancelled: Arc::clone(&cancelled),
                },
            );
            scope.spawn(move || {
                let prepared = if cancelled.load(Ordering::Acquire) {
                    None
                } else {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        snapshot.prepare_document(&id)
                    }))
                    .ok()
                    .flatten()
                    .filter(|_| !cancelled.load(Ordering::Acquire))
                };
                let _ = sender.send(TransportEvent::Parse(ParseResult {
                    id,
                    version: pending.version,
                    prepared,
                }));
            });
        }
    }

    pub(super) fn schedule_diagnostics(&mut self, uri: &str, delay: Duration) {
        let id = DocumentId::new(uri);
        let version = self
            .host
            .snapshot()
            .document(&id)
            .filter(|document| document.source() == DocumentSource::Overlay)
            .and_then(|document| document.version());
        if let Some(version) = version {
            self.pending_diagnostics.insert(
                id,
                PendingDiagnostics {
                    uri: uri.to_owned(),
                    version,
                    due: Instant::now() + delay,
                },
            );
        }
    }

    pub(super) fn schedule_diagnostics_for_document(
        &mut self,
        id: DocumentId,
        version: i64,
        delay: Duration,
    ) {
        self.pending_diagnostics.insert(
            id.clone(),
            PendingDiagnostics {
                uri: id.as_str().to_owned(),
                version,
                due: Instant::now() + delay,
            },
        );
    }

    pub(super) fn cancel_stale_diagnostics(
        &self,
        in_flight: &BTreeMap<DocumentId, InFlightDiagnostics>,
    ) {
        let snapshot = self.host.snapshot();
        for (id, task) in in_flight {
            let current_version = snapshot
                .document(id)
                .and_then(|document| document.version());
            let superseded = self
                .pending_diagnostics
                .get(id)
                .is_some_and(|pending| pending.version != task.version);
            if current_version != Some(task.version) || superseded {
                task.cancellation.cancel();
            }
        }
    }

    pub(super) fn next_diagnostic_wait(
        &self,
        in_flight: &BTreeMap<DocumentId, InFlightDiagnostics>,
    ) -> Option<Duration> {
        let now = Instant::now();
        self.pending_diagnostics
            .iter()
            .filter(|(id, _)| !in_flight.contains_key(*id))
            .map(|(_, pending)| pending.due.saturating_duration_since(now))
            .min()
    }

    pub(super) fn spawn_due_diagnostics<'scope, 'environment>(
        &mut self,
        scope: &'scope std::thread::Scope<'scope, 'environment>,
        event_sender: &mpsc::Sender<TransportEvent>,
        in_flight: &mut BTreeMap<DocumentId, InFlightDiagnostics>,
        force: bool,
    ) {
        let now = Instant::now();
        let ready = self
            .pending_diagnostics
            .iter()
            .filter(|(id, pending)| !in_flight.contains_key(*id) && (force || pending.due <= now))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ready {
            let Some(pending) = self.pending_diagnostics.remove(&id) else {
                continue;
            };
            let snapshot = self.host.snapshot();
            let sender = event_sender.clone();
            let cancellation = CancellationToken::new();
            in_flight.insert(
                id.clone(),
                InFlightDiagnostics {
                    version: pending.version,
                    cancellation: cancellation.clone(),
                },
            );
            scope.spawn(move || {
                let values = if cancellation.is_cancelled() {
                    None
                } else {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        diagnostic_values(&snapshot, &id, &cancellation)
                    }))
                    .ok()
                    .flatten()
                    .filter(|_| !cancellation.is_cancelled())
                };
                let _ = sender.send(TransportEvent::Diagnostics(DiagnosticsResult {
                    id,
                    uri: pending.uri,
                    version: pending.version,
                    values,
                }));
            });
        }
    }
}
