use super::*;

/// Monotonic nonce for work-done-progress tokens and their create-request ids.
fn progress_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos())
}

/// Server-initiated work-done-progress create request; the client's response is ignored.
fn work_done_progress_create(token: &str) -> Value {
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "id": format!("pdx-progress-{token}"),
        "method": "window/workDoneProgress/create",
        "params": {"token": token},
    })
}

fn work_done_progress_begin(token: &str, message: &str) -> Value {
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "method": "$/progress",
        "params": {
            "token": token,
            "value": {
                "kind": "begin",
                "title": "ParadoxCode",
                "cancellable": false,
                "message": message,
                "percentage": 0,
            },
        },
    })
}

fn work_done_progress_end(token: &str, message: &str) -> Value {
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "method": "$/progress",
        "params": {
            "token": token,
            "value": {"kind": "end", "message": message},
        },
    })
}

/// Builds the worker progress callback that forwards engine scan progress as `$/progress` reports.
fn vanilla_progress_sender(
    sender: mpsc::Sender<TransportEvent>,
    token: String,
) -> impl Fn(usize, usize) {
    move |done, total| {
        let message = if total == 0 {
            "Discovering Vanilla files…".to_owned()
        } else {
            format!("Indexing Vanilla files ({done}/{total})…")
        };
        let mut value = json!({"kind": "report", "message": message});
        if let Some(percent) = done
            .checked_mul(100)
            .and_then(|percent| percent.checked_div(total))
        {
            value["percentage"] = json!(u32::try_from(percent).unwrap_or(100));
        }
        let _ = sender.send(TransportEvent::VanillaProgress(VanillaProgress {
            params: json!({"token": token, "value": value}),
        }));
    }
}

impl LspServer {
    /// Runs the same framed transport over arbitrary streams.
    ///
    /// This is public so integration tests can drive the actual JSON-RPC framing without a
    /// process or socket. A dedicated reader keeps input available while diagnostics run on
    /// immutable snapshots; the event-loop thread remains the sole workspace-state owner.
    pub fn run_transport<R: Read + Send, W: Write>(
        &mut self,
        input: R,
        mut output: W,
    ) -> Result<(), LspError> {
        std::thread::scope(|scope| {
            let (event_sender, event_receiver) = mpsc::channel::<TransportEvent>();
            let (read_sender, read_receiver) = mpsc::channel::<()>();
            let reader_sender = event_sender.clone();
            scope.spawn(move || {
                let mut input = BufReader::new(input);
                while read_receiver.recv().is_ok() {
                    let result = read_message(&mut input);
                    let terminal = !matches!(result, Ok(Some(_)));
                    if reader_sender.send(TransportEvent::Input(result)).is_err() || terminal {
                        break;
                    }
                }
            });

            let mut reader_active = true;
            read_sender.send(()).map_err(|_| {
                LspError::Protocol("LSP transport reader failed to start".to_owned())
            })?;
            let mut in_flight_parses = BTreeMap::<DocumentId, InFlightParse>::new();
            let mut in_flight = BTreeMap::<DocumentId, InFlightDiagnostics>::new();
            let mut in_flight_requests = HashMap::<RequestId, InFlightRequest>::new();
            let mut in_flight_initialize = None::<InFlightInitialize>;
            let mut in_flight_vanilla = None::<VanillaSetupCancellation>;
            let mut vanilla_cache_in_flight = false;
            let mut vanilla_progress_token = None::<String>;
            let mut in_flight_disk_changes = None::<InFlightDiskChanges>;
            let mut deferred_messages = VecDeque::<Value>::new();

            loop {
                self.spawn_pending_disk_changes(scope, &event_sender, &mut in_flight_disk_changes);
                self.cancel_stale_parses(&in_flight_parses);
                self.spawn_pending_parses(scope, &event_sender, &mut in_flight_parses);
                self.cancel_stale_diagnostics(&in_flight);
                self.spawn_due_diagnostics(
                    scope,
                    &event_sender,
                    &mut in_flight,
                    self.state == ServerState::ShuttingDown,
                );
                let parse_busy = !self.pending_parses.is_empty() || !in_flight_parses.is_empty();
                let initialize_busy = in_flight_initialize.is_some();
                let disk_changes_busy =
                    !self.pending_disk_changes.is_empty() || in_flight_disk_changes.is_some();
                let vanilla_cache_busy = vanilla_cache_in_flight && in_flight_vanilla.is_some();
                let deferred_ready = !parse_busy
                    && !initialize_busy
                    && !disk_changes_busy
                    && !vanilla_cache_busy
                    && !deferred_messages.is_empty();
                let (event, from_reader) = if deferred_ready {
                    let message = deferred_messages.pop_front().expect("checked non-empty");
                    (TransportEvent::Input(Ok(Some(message))), false)
                } else {
                    let timeout = self.next_diagnostic_wait(&in_flight);
                    let event = match timeout {
                        Some(timeout) => match event_receiver.recv_timeout(timeout) {
                            Ok(event) => event,
                            Err(RecvTimeoutError::Timeout) => continue,
                            Err(RecvTimeoutError::Disconnected) => {
                                return Err(LspError::Protocol(
                                    "LSP transport workers stopped unexpectedly".to_owned(),
                                ));
                            }
                        },
                        None => event_receiver.recv().map_err(|_| {
                            LspError::Protocol(
                                "LSP transport workers stopped unexpectedly".to_owned(),
                            )
                        })?,
                    };
                    (event, true)
                };

                match event {
                    TransportEvent::Input(result) => {
                        if from_reader {
                            reader_active = false;
                        }
                        let Some(message) = result? else {
                            return if self.state == ServerState::Exited && self.clean_exit {
                                Ok(())
                            } else {
                                Err(LspError::Protocol(
                                    "transport ended before a clean exit".to_owned(),
                                ))
                            };
                        };
                        let parse_busy =
                            !self.pending_parses.is_empty() || !in_flight_parses.is_empty();
                        let initialize_busy = in_flight_initialize.is_some();
                        let disk_changes_busy = !self.pending_disk_changes.is_empty()
                            || in_flight_disk_changes.is_some();
                        let vanilla_cache_busy =
                            vanilla_cache_in_flight && in_flight_vanilla.is_some();
                        if from_reader
                            && (((parse_busy || disk_changes_busy)
                                && is_snapshot_request_message(&message))
                                || (vanilla_cache_busy && is_snapshot_request_message(&message))
                                || (initialize_busy && !is_initialize_control_message(&message)))
                        {
                            deferred_messages.push_back(message);
                        } else {
                            let spawned = self.spawn_initialize_request(
                                scope,
                                &event_sender,
                                &mut in_flight_initialize,
                                &message,
                            ) || self.spawn_snapshot_request(
                                scope,
                                &event_sender,
                                &mut in_flight_requests,
                                &message,
                            );
                            if !spawned {
                                let responses = self.handle_message(message.clone())?;
                                for response in responses {
                                    write_message(&mut output, &response)?;
                                }
                                cancel_request_from_notification(&message, &in_flight_requests);
                                cancel_initialize_from_notification(
                                    &message,
                                    in_flight_initialize.as_ref(),
                                );
                            }
                        }
                        self.cancel_stale_parses(&in_flight_parses);
                        self.cancel_stale_diagnostics(&in_flight);
                        if self.state == ServerState::Exited {
                            for task in in_flight_parses.values() {
                                task.cancelled.store(true, Ordering::Release);
                            }
                            for task in in_flight.values() {
                                task.cancellation.cancel();
                            }
                            for task in in_flight_requests.values() {
                                task.cancellation.cancel();
                            }
                            if let Some(task) = in_flight_initialize.as_ref() {
                                task.cancellation.cancel();
                            }
                            if let Some(task) = in_flight_vanilla.as_ref() {
                                task.cancel();
                            }
                            if let Some(task) = in_flight_disk_changes.as_ref() {
                                task.cancellation.cancel();
                            }
                            return if self.clean_exit {
                                Ok(())
                            } else {
                                Err(LspError::ExitWithoutShutdown)
                            };
                        }
                        if self.state == ServerState::ShuttingDown {
                            self.spawn_due_diagnostics(scope, &event_sender, &mut in_flight, true);
                        }
                    }
                    TransportEvent::Initialize(result) => {
                        let current = in_flight_initialize
                            .as_ref()
                            .is_some_and(|task| task.request_id == result.request_id);
                        if !current {
                            continue;
                        }
                        let task = in_flight_initialize
                            .take()
                            .expect("checked initialize task");
                        self.cancelled.remove(&result.request_id);
                        let (response, warnings, auto_vanilla, vanilla_cache) = match result.result
                        {
                            Ok(prepared) if !task.cancellation.is_cancelled() => {
                                self.host = prepared.host;
                                self.state = ServerState::Initialized;
                                self.watcher_registration = prepared.watcher_registration;
                                self.client_work_done_progress = prepared.client_work_done_progress;
                                self.client_snippet_support = prepared.client_snippet_support;
                                (
                                    json!({
                                        "jsonrpc": JSON_RPC_VERSION,
                                        "id": result.id,
                                        "result": prepared.result,
                                    }),
                                    prepared.warnings,
                                    prepared.auto_vanilla,
                                    prepared.vanilla_cache,
                                )
                            }
                            Ok(_) => {
                                self.state = ServerState::Uninitialized;
                                (
                                    RpcError::new(REQUEST_CANCELLED, "request was cancelled")
                                        .response(result.id),
                                    Vec::new(),
                                    None,
                                    None,
                                )
                            }
                            Err(error) => {
                                self.state = ServerState::Uninitialized;
                                (error.response(result.id), Vec::new(), None, None)
                            }
                        };
                        write_message(&mut output, &response)?;
                        for warning in warnings {
                            write_message(&mut output, &show_warning_notification(warning))?;
                        }
                        if let Some(path) = vanilla_cache {
                            let cancellation = VanillaSetupCancellation::new();
                            let sender = event_sender.clone();
                            let worker_cancellation = cancellation.clone();
                            let rules = self.host.snapshot().rules().clone();
                            let profile = self.host.snapshot().game_profile().clone();
                            let current_rule_hash = rules.rule_hash().to_hex();
                            let progress_token = format!("pdx-vanilla-{}", progress_nonce());
                            let progress: Option<Box<dyn Fn(usize, usize) + Send + Sync>> =
                                if self.client_work_done_progress {
                                    write_message(
                                        &mut output,
                                        &work_done_progress_create(&progress_token),
                                    )?;
                                    write_message(
                                        &mut output,
                                        &work_done_progress_begin(
                                            &progress_token,
                                            "Loading Vanilla index…",
                                        ),
                                    )?;
                                    Some(Box::new(vanilla_progress_sender(
                                        sender.clone(),
                                        progress_token.clone(),
                                    )))
                                } else {
                                    write_message(
                                        &mut output,
                                        &show_info_notification(
                                            "Vanilla index is being loaded in the background…"
                                                .to_owned(),
                                        ),
                                    )?;
                                    None
                                };
                            vanilla_progress_token = if self.client_work_done_progress {
                                Some(progress_token.clone())
                            } else {
                                None
                            };
                            in_flight_vanilla = Some(cancellation);
                            vanilla_cache_in_flight = true;
                            // The user-level auto configuration survives `apply_user_vanilla_configuration`,
                            // so an unavailable configured cache can still fall back to discovery.
                            let auto_vanilla = self.auto_vanilla.clone();
                            scope.spawn(move || {
                                let result = run_vanilla_cache_load(
                                    &path,
                                    rules,
                                    profile,
                                    current_rule_hash,
                                    auto_vanilla.as_ref(),
                                    progress
                                        .as_deref()
                                        .map(|callback| callback as &(dyn Fn(usize, usize) + Sync)),
                                    &worker_cancellation,
                                );
                                let _ =
                                    sender.send(TransportEvent::VanillaSetup(VanillaSetupResult {
                                        result,
                                    }));
                            });
                        } else if let Some(configuration) = auto_vanilla {
                            let cancellation = VanillaSetupCancellation::new();
                            let sender = event_sender.clone();
                            let rules = self.host.snapshot().rules().clone();
                            let profile = self.host.snapshot().game_profile().clone();
                            let worker_cancellation = cancellation.clone();
                            let progress_token = format!("pdx-vanilla-{}", progress_nonce());
                            let progress: Option<Box<dyn Fn(usize, usize) + Send + Sync>> =
                                if self.client_work_done_progress {
                                    write_message(
                                        &mut output,
                                        &work_done_progress_create(&progress_token),
                                    )?;
                                    write_message(
                                        &mut output,
                                        &work_done_progress_begin(
                                            &progress_token,
                                            "Building Vanilla index…",
                                        ),
                                    )?;
                                    Some(Box::new(vanilla_progress_sender(
                                        sender.clone(),
                                        progress_token.clone(),
                                    )))
                                } else {
                                    write_message(
                                        &mut output,
                                        &show_info_notification(
                                            "Vanilla index is being built in the background…"
                                                .to_owned(),
                                        ),
                                    )?;
                                    None
                                };
                            vanilla_progress_token = if self.client_work_done_progress {
                                Some(progress_token.clone())
                            } else {
                                None
                            };
                            in_flight_vanilla = Some(cancellation);
                            vanilla_cache_in_flight = false;
                            scope.spawn(move || {
                                let result = run_auto_vanilla_setup(
                                    &configuration,
                                    rules,
                                    profile,
                                    progress
                                        .as_deref()
                                        .map(|callback| callback as &(dyn Fn(usize, usize) + Sync)),
                                    &worker_cancellation,
                                );
                                let _ =
                                    sender.send(TransportEvent::VanillaSetup(VanillaSetupResult {
                                        result,
                                    }));
                            });
                        }
                    }
                    TransportEvent::Parse(result) => {
                        if in_flight_parses
                            .get(&result.id)
                            .is_some_and(|task| task.version == result.version)
                        {
                            in_flight_parses.remove(&result.id);
                        }
                        if let Some(prepared) = result.prepared
                            && self.host.commit_prepared_document(prepared)
                        {
                            self.schedule_diagnostics_for_document(
                                result.id,
                                result.version,
                                DIAGNOSTIC_DEBOUNCE,
                            );
                        }
                    }
                    TransportEvent::Diagnostics(result) => {
                        if in_flight
                            .get(&result.id)
                            .is_some_and(|task| task.version == result.version)
                        {
                            in_flight.remove(&result.id);
                        }
                        if let Some(values) = result.values
                            && self.commit_diagnostics(&result.uri, result.version, values.clone())
                        {
                            write_message(
                                &mut output,
                                &diagnostics_notification(&result.uri, values),
                            )?;
                        }
                    }
                    TransportEvent::Request(result) => {
                        in_flight_requests.remove(&result.request_id);
                        self.cancelled.remove(&result.request_id);
                        let response = match result.result {
                            Ok(value) => json!({
                                "jsonrpc": JSON_RPC_VERSION,
                                "id": result.id,
                                "result": value,
                            }),
                            Err(error) => error.response(result.id),
                        };
                        write_message(&mut output, &response)?;
                    }
                    TransportEvent::VanillaProgress(result) => {
                        write_message(
                            &mut output,
                            &json!({
                                "jsonrpc": JSON_RPC_VERSION,
                                "method": "$/progress",
                                "params": result.params,
                            }),
                        )?;
                    }
                    TransportEvent::VanillaSetup(result) => {
                        in_flight_vanilla = None;
                        vanilla_cache_in_flight = false;
                        if let Some(token) = vanilla_progress_token.take() {
                            let message = match &result.result {
                                Ok((_, message)) => message.clone(),
                                Err(message) => message.clone(),
                            };
                            write_message(&mut output, &work_done_progress_end(&token, &message))?;
                        }
                        match result.result {
                            Ok((cache, message)) => {
                                let cache_rule_hash = cache.metadata().rule_hash.clone();
                                let current_rule_hash =
                                    self.host.snapshot().rules().rule_hash().to_hex();
                                match self.host.install_vanilla_cache(cache) {
                                    Ok(()) => {
                                        if cache_rule_hash != current_rule_hash {
                                            write_message(
                                                &mut output,
                                                &show_warning_notification(format!(
                                                    "{message}; the installed cache was built with rules hash {cache_rule_hash}, but the active rules hash is {current_rule_hash}"
                                                )),
                                            )?;
                                        } else {
                                            write_message(
                                                &mut output,
                                                &show_info_notification(message),
                                            )?;
                                        }
                                        let open = self
                                            .host
                                            .snapshot()
                                            .documents()
                                            .iter()
                                            .filter_map(|(id, document)| {
                                                document
                                                    .version()
                                                    .map(|version| (id.clone(), version))
                                            })
                                            .collect::<Vec<_>>();
                                        for (id, version) in open {
                                            self.schedule_diagnostics_for_document(
                                                id,
                                                version,
                                                DIAGNOSTIC_DEBOUNCE,
                                            );
                                        }
                                    }
                                    Err(error) => write_message(
                                        &mut output,
                                        &show_warning_notification(format!(
                                            "Vanilla cache was built but could not be enabled in this workspace: {error}"
                                        )),
                                    )?,
                                }
                            }
                            Err(message) => {
                                write_message(&mut output, &show_warning_notification(message))?;
                            }
                        }
                    }
                    TransportEvent::DiskChanges(result) => {
                        let current = in_flight_disk_changes
                            .as_ref()
                            .is_some_and(|task| task.base_revision == result.base_revision);
                        if !current {
                            continue;
                        }
                        let task = in_flight_disk_changes
                            .take()
                            .expect("checked disk change task");
                        if task.cancellation.is_cancelled() {
                            continue;
                        }
                        if self.host.snapshot().revision() != result.base_revision {
                            self.requeue_disk_changes(result.changes);
                            continue;
                        }
                        match result.result {
                            Ok(host) => {
                                self.host = host;
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
                                    self.schedule_diagnostics_for_document(
                                        id,
                                        version,
                                        DIAGNOSTIC_DEBOUNCE,
                                    );
                                }
                            }
                            Err(WorkspaceError::Cancelled) => {}
                            Err(error) => {
                                write_message(
                                    &mut output,
                                    &show_warning_notification(format!(
                                        "Workspace file changes could not be indexed: {error}"
                                    )),
                                )?;
                            }
                        }
                    }
                }

                let draining_shutdown = self.state == ServerState::ShuttingDown
                    && (!self.pending_parses.is_empty()
                        || !in_flight_parses.is_empty()
                        || !self.pending_diagnostics.is_empty()
                        || !in_flight.is_empty()
                        || !in_flight_requests.is_empty()
                        || in_flight_initialize.is_some()
                        || in_flight_vanilla.is_some()
                        || !self.pending_disk_changes.is_empty()
                        || in_flight_disk_changes.is_some());
                if !reader_active && !draining_shutdown && deferred_messages.is_empty() {
                    read_sender.send(()).map_err(|_| {
                        LspError::Protocol("LSP transport reader stopped unexpectedly".to_owned())
                    })?;
                    reader_active = true;
                }
            }
        })
    }
}
