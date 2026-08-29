use crate::dependency::run_dependency_cache_loads;

use super::*;

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
            let mut initialize_progress_token = None::<String>;
            let mut ready_logged = false;
            let mut in_flight_index = None::<IndexSetupCancellation>;
            let mut index_cache_in_flight = false;
            let mut index_progress_token = None::<String>;
            let mut in_flight_dependency = None::<WorkspaceScanToken>;
            let mut dependency_cache_in_flight = false;
            let mut dependency_progress_token = None::<String>;
            let mut in_flight_disk_changes = None::<InFlightDiskChanges>;
            let mut in_flight_background_reindex = None::<InFlightBackgroundReindex>;
            let mut in_flight_workspace_diagnostics = None::<InFlightWorkspaceDiagnostics>;
            let mut in_flight_reindex_command = None::<InFlightReindexCommand>;
            let mut deferred_messages = VecDeque::<Value>::new();

            loop {
                let pending_workspace_clears =
                    std::mem::take(&mut self.workspace_diagnostic_clear_queue);
                for uri in pending_workspace_clears {
                    write_message(&mut output, &diagnostics_notification(&uri, json!([])))?;
                }
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
                let mut background_busy = !self.pending_parses.is_empty()
                    || !in_flight_parses.is_empty()
                    || !self.pending_diagnostics.is_empty()
                    || !in_flight.is_empty()
                    || !in_flight_requests.is_empty()
                    || in_flight_initialize.is_some()
                    || in_flight_index.is_some()
                    || in_flight_dependency.is_some()
                    || self.has_pending_disk_changes()
                    || in_flight_disk_changes.is_some()
                    || in_flight_background_reindex.is_some()
                    || in_flight_workspace_diagnostics.is_some()
                    || in_flight_reindex_command.is_some();
                self.spawn_due_background_reindex(
                    scope,
                    &event_sender,
                    &mut in_flight_background_reindex,
                    ready_logged,
                    background_busy,
                );
                // A due quiet pass may have been launched by the call above. Recompute the
                // guard before accepting an explicit command so the two full scans never overlap.
                background_busy = background_busy
                    || in_flight_background_reindex.is_some()
                    || in_flight_reindex_command.is_some();
                self.spawn_pending_workspace_diagnostics(
                    scope,
                    &event_sender,
                    &mut in_flight_workspace_diagnostics,
                    background_busy,
                );
                background_busy = background_busy || in_flight_workspace_diagnostics.is_some();
                if let Some(task) = in_flight_reindex_command.as_ref()
                    && self.cancelled.contains(&task.request_id)
                {
                    task.cancellation.cancel();
                }
                let parse_busy = !self.pending_parses.is_empty() || !in_flight_parses.is_empty();
                let initialize_busy = in_flight_initialize.is_some();
                let disk_changes_busy =
                    self.has_pending_disk_changes() || in_flight_disk_changes.is_some();
                let vanilla_cache_busy = index_cache_in_flight && in_flight_index.is_some();
                let dependency_cache_busy =
                    dependency_cache_in_flight && in_flight_dependency.is_some();
                let deferred_ready = !parse_busy
                    && !initialize_busy
                    && !disk_changes_busy
                    && !vanilla_cache_busy
                    && !dependency_cache_busy
                    && !(background_busy
                        && deferred_messages
                            .front()
                            .is_some_and(is_execute_command_message))
                    && !deferred_messages.is_empty();
                let (event, from_reader) = if deferred_ready {
                    let message = deferred_messages.pop_front().expect("checked non-empty");
                    (TransportEvent::Input(Ok(Some(message))), false)
                } else {
                    let timeout = self
                        .next_diagnostic_wait(&in_flight)
                        .into_iter()
                        .chain(self.background_reindex_wait(
                            ready_logged,
                            in_flight_background_reindex.as_ref(),
                            background_busy,
                        ))
                        .chain(self.pending_disk_change_wait(in_flight_disk_changes.as_ref()))
                        .min();
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
                        let disk_changes_busy =
                            self.has_pending_disk_changes() || in_flight_disk_changes.is_some();
                        let vanilla_cache_busy = index_cache_in_flight && in_flight_index.is_some();
                        let dependency_cache_busy =
                            dependency_cache_in_flight && in_flight_dependency.is_some();
                        let execute_command_busy =
                            background_busy && is_execute_command_message(&message);
                        if from_reader
                            && (((parse_busy || disk_changes_busy)
                                && is_snapshot_request_message(&message))
                                || (vanilla_cache_busy && is_snapshot_request_message(&message))
                                || (dependency_cache_busy && is_snapshot_request_message(&message))
                                || execute_command_busy
                                || (initialize_busy && !is_initialize_control_message(&message)))
                        {
                            deferred_messages.push_back(message);
                        } else {
                            let spawned = self.spawn_initialize_request(
                                scope,
                                &event_sender,
                                &mut in_flight_initialize,
                                &message,
                                &mut output,
                                &mut initialize_progress_token,
                            )? || self.spawn_reindex_command(
                                scope,
                                &event_sender,
                                &mut in_flight_reindex_command,
                                background_busy,
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
                            if let Some(task) = in_flight_index.as_ref() {
                                task.cancel();
                            }
                            if let Some(task) = in_flight_dependency.as_ref() {
                                task.cancel();
                            }
                            if let Some(task) = in_flight_disk_changes.as_ref() {
                                task.cancellation.cancel();
                            }
                            if let Some(task) = in_flight_background_reindex.as_ref() {
                                task.cancellation.cancel();
                            }
                            if let Some(task) = in_flight_workspace_diagnostics.as_ref() {
                                task.cancellation.cancel();
                            }
                            if let Some(task) = in_flight_reindex_command.as_ref() {
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
                        let (response, warnings, auto_vanilla, index_cache, dependency_caches) =
                            match result.result {
                                Ok(prepared) if !task.cancellation.is_cancelled() => {
                                    self.host = prepared.host;
                                    self.invalidate_all_semantic_tokens();
                                    self.state = ServerState::Initialized;
                                    self.textures = prepared.textures;
                                    self.watcher_registration = prepared.watcher_registration;
                                    self.client_work_done_progress =
                                        prepared.client_work_done_progress;
                                    self.client_snippet_support = prepared.client_snippet_support;
                                    self.background_reindex_interval_minutes =
                                        prepared.background_reindex_interval_minutes;
                                    self.background_reindex_idle_seconds =
                                        prepared.background_reindex_idle_seconds;
                                    self.ignored_diagnostic_codes = Arc::new(
                                        prepared.ignored_diagnostic_codes.iter().cloned().collect(),
                                    );
                                    self.diagnostic_severity_overrides =
                                        Arc::new(prepared.diagnostic_severity_overrides.clone());
                                    self.workspace_wide_diagnostics =
                                        prepared.workspace_wide_diagnostics;
                                    self.last_activity = Instant::now();
                                    (
                                        json!({
                                            "jsonrpc": JSON_RPC_VERSION,
                                            "id": result.id,
                                            "result": prepared.result,
                                        }),
                                        prepared.warnings,
                                        prepared.auto_vanilla,
                                        prepared.index_cache,
                                        prepared.dependency_caches,
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
                                        Vec::new(),
                                    )
                                }
                                Err(error) => {
                                    self.state = ServerState::Uninitialized;
                                    (
                                        error.response(result.id),
                                        Vec::new(),
                                        None,
                                        None,
                                        Vec::new(),
                                    )
                                }
                            };
                        write_message(&mut output, &response)?;
                        if let Some(token) = initialize_progress_token.take() {
                            let message = if response.get("result").is_some() {
                                format!(
                                    "Ready — {} source file(s) indexed",
                                    self.host.snapshot().source_files().len()
                                )
                            } else if response.get("error").and_then(|error| error.get("code"))
                                == Some(&json!(REQUEST_CANCELLED))
                            {
                                "Initialization cancelled".to_owned()
                            } else {
                                "Initialization failed".to_owned()
                            };
                            write_message(&mut output, &work_done_progress_end(&token, &message))?;
                        }
                        for warning in warnings {
                            write_message(&mut output, &show_warning_notification(warning))?;
                        }
                        if let Some(path) = index_cache {
                            write_message(
                                &mut output,
                                &log_message_notification(
                                    MessageType::INFO,
                                    format!("Vanilla index: loading cache from {}", path.display()),
                                ),
                            )?;
                            let cancellation = IndexSetupCancellation::new();
                            let sender = event_sender.clone();
                            let worker_cancellation = cancellation.clone();
                            let rules = self.host.snapshot().rules().clone();
                            let profile = self.host.snapshot().game_profile().clone();
                            let scan_limits = self.host.snapshot().scan_limits();
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
                                    Some(Box::new(progress_sender(
                                        sender.clone(),
                                        progress_token.clone(),
                                        "Loading Vanilla index",
                                        "Loading Vanilla index",
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
                            index_progress_token = if self.client_work_done_progress {
                                Some(progress_token.clone())
                            } else {
                                None
                            };
                            in_flight_index = Some(cancellation);
                            index_cache_in_flight = true;
                            // The user-level auto configuration survives `apply_user_vanilla_configuration`,
                            // so an unavailable configured cache can still fall back to discovery.
                            let auto_vanilla = self.auto_vanilla.clone();
                            let log = {
                                let sender = event_sender.clone();
                                move |message: &str| {
                                    let _ =
                                        sender.send(TransportEvent::Log(log_message_notification(
                                            MessageType::INFO,
                                            message.to_owned(),
                                        )));
                                }
                            };
                            scope.spawn(move || {
                                let log_ref: &(dyn Fn(&str) + Sync) = &log;
                                let result = run_index_cache_load(IndexCacheLoadRequest {
                                    path: &path,
                                    rules,
                                    profile,
                                    current_rule_hash,
                                    auto_vanilla: auto_vanilla.as_ref(),
                                    log: Some(log_ref),
                                    progress: progress
                                        .as_deref()
                                        .map(|callback| callback as &(dyn Fn(usize, usize) + Sync)),
                                    scan_limits,
                                    cancellation: &worker_cancellation,
                                });
                                let _ =
                                    sender.send(TransportEvent::VanillaSetup(IndexSetupResult {
                                        result,
                                    }));
                            });
                        } else if let Some(configuration) = auto_vanilla {
                            write_message(
                                &mut output,
                                &log_message_notification(
                                    MessageType::INFO,
                                    "Vanilla index: discovering installation and building cache…"
                                        .to_owned(),
                                ),
                            )?;
                            let cancellation = IndexSetupCancellation::new();
                            let sender = event_sender.clone();
                            let rules = self.host.snapshot().rules().clone();
                            let profile = self.host.snapshot().game_profile().clone();
                            let scan_limits = self.host.snapshot().scan_limits();
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
                                    Some(Box::new(progress_sender(
                                        sender.clone(),
                                        progress_token.clone(),
                                        "Discovering Vanilla files",
                                        "Indexing Vanilla files",
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
                            index_progress_token = if self.client_work_done_progress {
                                Some(progress_token.clone())
                            } else {
                                None
                            };
                            in_flight_index = Some(cancellation);
                            index_cache_in_flight = false;
                            let log = {
                                let sender = event_sender.clone();
                                move |message: &str| {
                                    let _ =
                                        sender.send(TransportEvent::Log(log_message_notification(
                                            MessageType::INFO,
                                            message.to_owned(),
                                        )));
                                }
                            };
                            scope.spawn(move || {
                                let log_ref: &(dyn Fn(&str) + Sync) = &log;
                                let result =
                                    crate::vanilla::run_auto_vanilla_setup_with_options_and_limits(
                                        &configuration,
                                        rules,
                                        profile,
                                        Some(log_ref),
                                        progress.as_deref().map(|callback| {
                                            callback as &(dyn Fn(usize, usize) + Sync)
                                        }),
                                        &worker_cancellation,
                                        &pdx_game::DiscoveryOptions::default(),
                                        scan_limits,
                                    );
                                let _ =
                                    sender.send(TransportEvent::VanillaSetup(IndexSetupResult {
                                        result,
                                    }));
                            });
                        } else {
                            write_message(
                                &mut output,
                                &log_message_notification(
                                    MessageType::INFO,
                                    "Vanilla index: no cache or automatic discovery worker scheduled; continuing without Vanilla symbols"
                                        .to_owned(),
                                ),
                            )?;
                        }
                        if !dependency_caches.is_empty() {
                            write_message(
                                &mut output,
                                &log_message_notification(
                                    MessageType::INFO,
                                    format!(
                                        "Dependency indexes: loading {} cache(s)…",
                                        dependency_caches.len()
                                    ),
                                ),
                            )?;
                            let cancellation = WorkspaceScanToken::new();
                            let sender = event_sender.clone();
                            let worker_cancellation = cancellation.clone();
                            let rules = self.host.snapshot().rules().clone();
                            let profile = self.host.snapshot().game_profile().clone();
                            let scan_limits = self.host.snapshot().scan_limits();
                            let current_rule_hash = rules.rule_hash().to_hex();
                            let progress_token = format!("pdx-dependency-{}", progress_nonce());
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
                                            "Loading dependency index…",
                                        ),
                                    )?;
                                    Some(Box::new(progress_sender(
                                        sender.clone(),
                                        progress_token.clone(),
                                        "Loading dependency index",
                                        "Indexing dependency files",
                                    )))
                                } else {
                                    write_message(
                                        &mut output,
                                        &show_info_notification(
                                            "Dependency indexes are being loaded in the background…"
                                                .to_owned(),
                                        ),
                                    )?;
                                    None
                                };
                            dependency_progress_token = if self.client_work_done_progress {
                                Some(progress_token.clone())
                            } else {
                                None
                            };
                            in_flight_dependency = Some(cancellation);
                            dependency_cache_in_flight = true;
                            let log = {
                                let sender = event_sender.clone();
                                move |message: &str| {
                                    let _ =
                                        sender.send(TransportEvent::Log(log_message_notification(
                                            MessageType::INFO,
                                            message.to_owned(),
                                        )));
                                }
                            };
                            scope.spawn(move || {
                                let results = run_dependency_cache_loads(
                                    dependency_caches,
                                    rules,
                                    profile,
                                    current_rule_hash,
                                    scan_limits,
                                    Some(&log),
                                    progress
                                        .as_deref()
                                        .map(|callback| callback as &(dyn Fn(usize, usize) + Sync)),
                                    &worker_cancellation,
                                );
                                let _ = sender.send(TransportEvent::DependencySetup(
                                    DependencySetupResult { results },
                                ));
                            });
                        } else {
                            write_message(
                                &mut output,
                                &log_message_notification(
                                    MessageType::INFO,
                                    "Dependency indexes: none configured".to_owned(),
                                ),
                            )?;
                        }
                        if in_flight_index.is_none()
                            && in_flight_dependency.is_none()
                            && !ready_logged
                        {
                            write_message(
                                &mut output,
                                &log_message_notification(
                                    MessageType::INFO,
                                    "pdx-ls ready — workspace indexed".to_owned(),
                                ),
                            )?;
                            let snapshot = self.host.snapshot();
                            write_message(
                                &mut output,
                                &ready_notification(
                                    snapshot.revision(),
                                    snapshot.source_files().len(),
                                ),
                            )?;
                            ready_logged = true;
                            self.arm_background_reindex();
                            self.request_workspace_diagnostics();
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
                    TransportEvent::Progress(result) => {
                        write_message(
                            &mut output,
                            &json!({
                                "jsonrpc": JSON_RPC_VERSION,
                                "method": "$/progress",
                                "params": result.params,
                            }),
                        )?;
                    }
                    TransportEvent::Log(value) => {
                        write_message(&mut output, &value)?;
                    }
                    TransportEvent::DependencySetup(result) => {
                        in_flight_dependency = None;
                        dependency_cache_in_flight = false;
                        let outcome_message = result
                            .results
                            .iter()
                            .map(|(_, result)| match result {
                                Ok((_, message)) => message.clone(),
                                Err(message) => message.clone(),
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        write_message(
                            &mut output,
                            &log_message_notification(
                                MessageType::INFO,
                                format!("Dependency indexes: {outcome_message}"),
                            ),
                        )?;
                        let mut diagnostics_dirty = false;
                        let current_rule_hash = self.host.snapshot().rules().rule_hash().to_hex();
                        let mut install_metadata = Vec::new();
                        let mut install_caches = Vec::new();
                        for (config, result) in result.results {
                            match result {
                                Ok((cache, message)) => {
                                    install_metadata.push((
                                        config,
                                        cache.metadata().rule_hash.clone(),
                                        message,
                                    ));
                                    install_caches.push(cache);
                                }
                                Err(message) => {
                                    write_message(&mut output, &show_warning_notification(message))?
                                }
                            }
                        }
                        if !install_caches.is_empty() {
                            let cached_files = install_caches
                                .iter()
                                .map(|cache| cache.source_files().len())
                                .sum::<usize>();
                            let cached_positions = install_caches
                                .iter()
                                .map(|cache| cache.index().position_ranges().len())
                                .sum::<usize>();
                            write_message(
                                &mut output,
                                &log_message_notification(
                                    MessageType::INFO,
                                    format!(
                                        "Dependency index phase: merging {} cache(s) ({} file(s), {} position(s)) into the workspace",
                                        install_caches.len(),
                                        cached_files,
                                        cached_positions
                                    ),
                                ),
                            )?;
                            let installed = std::time::Instant::now();
                            match self.host.install_index_caches(install_caches) {
                                Ok(()) => {
                                    self.invalidate_all_semantic_tokens();
                                    diagnostics_dirty = true;
                                    write_message(
                                        &mut output,
                                        &log_message_notification(
                                            MessageType::INFO,
                                            format!(
                                                "Dependency indexes installed in {:.1} ms",
                                                installed.elapsed().as_secs_f64() * 1000.0
                                            ),
                                        ),
                                    )?;
                                    for (_, cache_rule_hash, message) in install_metadata {
                                        if cache_rule_hash != current_rule_hash {
                                            write_message(
                                                &mut output,
                                                &show_warning_notification(format!(
                                                    "{message}; the installed dependency cache was built with rules hash {cache_rule_hash}, but the active rules hash is {current_rule_hash}"
                                                )),
                                            )?;
                                        } else {
                                            write_message(
                                                &mut output,
                                                &show_info_notification(message),
                                            )?;
                                        }
                                    }
                                }
                                Err(error) => {
                                    let elapsed = installed.elapsed().as_secs_f64() * 1000.0;
                                    for (config, _, _) in install_metadata {
                                        write_message(
                                            &mut output,
                                            &show_warning_notification(format!(
                                                "dependency cache for {} could not be enabled in this workspace after {elapsed:.1} ms: {error}",
                                                config.root.path.display()
                                            )),
                                        )?;
                                    }
                                }
                            }
                        }
                        if let Some(token) = dependency_progress_token.take() {
                            write_message(
                                &mut output,
                                &work_done_progress_end(&token, &outcome_message),
                            )?;
                        }
                        if diagnostics_dirty {
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
                        if in_flight_index.is_none()
                            && in_flight_dependency.is_none()
                            && !ready_logged
                        {
                            write_message(
                                &mut output,
                                &log_message_notification(
                                    MessageType::INFO,
                                    "pdx-ls ready — workspace, Vanilla and dependency indexes loaded"
                                        .to_owned(),
                                ),
                            )?;
                            let snapshot = self.host.snapshot();
                            write_message(
                                &mut output,
                                &ready_notification(
                                    snapshot.revision(),
                                    snapshot.source_files().len(),
                                ),
                            )?;
                            ready_logged = true;
                            self.arm_background_reindex();
                            self.request_workspace_diagnostics();
                        }
                    }
                    TransportEvent::VanillaSetup(result) => {
                        in_flight_index = None;
                        index_cache_in_flight = false;
                        let outcome_message = match &result.result {
                            Ok((_, message)) => message.clone(),
                            Err(message) => message.clone(),
                        };
                        write_message(
                            &mut output,
                            &log_message_notification(
                                MessageType::INFO,
                                format!("Vanilla index: {outcome_message}"),
                            ),
                        )?;
                        match result.result {
                            Ok((cache, message)) => {
                                let cache_rule_hash = cache.metadata().rule_hash.clone();
                                let cached_files = cache.source_files().len();
                                let cached_positions = cache.index().position_ranges().len();
                                let current_rule_hash =
                                    self.host.snapshot().rules().rule_hash().to_hex();
                                write_message(
                                    &mut output,
                                    &log_message_notification(
                                        MessageType::INFO,
                                        format!(
                                            "Vanilla index phase: merging {} file(s) and {} position(s) into the workspace",
                                            cached_files, cached_positions
                                        ),
                                    ),
                                )?;
                                let installed = std::time::Instant::now();
                                match self.host.install_index_cache(cache) {
                                    Ok(()) => {
                                        self.invalidate_all_semantic_tokens();
                                        write_message(
                                            &mut output,
                                            &log_message_notification(
                                                MessageType::INFO,
                                                format!(
                                                    "Vanilla index installed in {:.1} ms",
                                                    installed.elapsed().as_secs_f64() * 1000.0
                                                ),
                                            ),
                                        )?;
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
                                            "Vanilla cache was built but could not be enabled in this workspace after {:.1} ms: {error}",
                                            installed.elapsed().as_secs_f64() * 1000.0
                                        )),
                                    )?,
                                }
                            }
                            Err(message) => {
                                write_message(&mut output, &show_warning_notification(message))?;
                            }
                        }
                        if let Some(token) = index_progress_token.take() {
                            write_message(
                                &mut output,
                                &work_done_progress_end(&token, &outcome_message),
                            )?;
                        }
                        if in_flight_index.is_none()
                            && in_flight_dependency.is_none()
                            && !ready_logged
                        {
                            write_message(
                                &mut output,
                                &log_message_notification(
                                    MessageType::INFO,
                                    "pdx-ls ready — workspace, Vanilla and dependency indexes loaded"
                                        .to_owned(),
                                ),
                            )?;
                            let snapshot = self.host.snapshot();
                            write_message(
                                &mut output,
                                &ready_notification(
                                    snapshot.revision(),
                                    snapshot.source_files().len(),
                                ),
                            )?;
                            ready_logged = true;
                            self.arm_background_reindex();
                            self.request_workspace_diagnostics();
                        }
                    }
                    TransportEvent::BackgroundReindex(result) => {
                        let current = in_flight_background_reindex
                            .as_ref()
                            .is_some_and(|task| task.base_revision == result.base_revision);
                        if !current {
                            continue;
                        }
                        let task = in_flight_background_reindex
                            .take()
                            .expect("checked background reindex task");
                        if task.cancellation.is_cancelled() {
                            self.arm_background_reindex();
                            continue;
                        }
                        if self.host.snapshot().revision() != result.base_revision {
                            // A foreground edit or disk refresh won while the quiet pass was
                            // running. Its candidate is stale and must never replace newer state.
                            self.arm_background_reindex();
                            continue;
                        }
                        match result.result {
                            Ok((host, workspace)) => {
                                self.host = host;
                                self.invalidate_all_semantic_tokens();
                                let snapshot = self.host.snapshot();
                                write_message(
                                    &mut output,
                                    &log_message_notification(
                                        MessageType::INFO,
                                        format!(
                                            "Background workspace reindex completed (revision {}, {} source file(s))",
                                            snapshot.revision(),
                                            snapshot.source_files().len(),
                                        ),
                                    ),
                                )?;
                                let open = snapshot
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
                                if self.workspace_wide_diagnostics
                                    && let Some(workspace) = workspace.as_ref()
                                {
                                    self.publish_workspace_diagnostics(&mut output, workspace)?;
                                }
                            }
                            Err(WorkspaceError::Cancelled) => {}
                            Err(error) => write_message(
                                &mut output,
                                &show_warning_notification(format!(
                                    "Background workspace reindex failed: {error}"
                                )),
                            )?,
                        }
                        self.arm_background_reindex();
                    }
                    TransportEvent::ReindexCommand(result) => {
                        let current = in_flight_reindex_command.as_ref().is_some_and(|task| {
                            task.request_id == result.request_id
                                && task.base_revision == result.base_revision
                                && task.command == result.command
                        });
                        if !current {
                            continue;
                        }
                        let task = in_flight_reindex_command
                            .take()
                            .expect("checked explicit reindex command task");
                        self.cancelled.remove(&result.request_id);
                        let response = if task.cancellation.is_cancelled() {
                            RpcError::new(REQUEST_CANCELLED, "request was cancelled")
                                .response(result.id)
                        } else if self.host.snapshot().revision() != result.base_revision {
                            RpcError::new(
                                INVALID_REQUEST,
                                match result.command {
                                    WorkspaceCommand::Reindex => {
                                        "workspace changed while reindexing; run pdx/reindexWorkspace again"
                                    }
                                    WorkspaceCommand::Validate => {
                                        "workspace changed while validating; run validateWorkspace again"
                                    }
                                },
                            )
                            .response(result.id)
                        } else {
                            match result.result {
                                Ok((host, summary)) => {
                                    self.host = host;
                                    self.invalidate_all_semantic_tokens();
                                    let snapshot = self.host.snapshot();
                                    let open = snapshot
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
                                    if self.workspace_wide_diagnostics
                                        && let Some(workspace) = summary.as_ref()
                                    {
                                        self.publish_workspace_diagnostics(&mut output, workspace)?;
                                    }
                                    let result_value = match (result.command, summary.as_ref()) {
                                        (WorkspaceCommand::Validate, Some(workspace)) => json!({
                                            "revision": snapshot.revision(),
                                            "sourceFiles": snapshot.source_files().len(),
                                            "totalFiles": workspace.summary.total_files,
                                            "validatedFiles": workspace.summary.validated_files,
                                            "filesWithErrors": workspace.summary.files_with_errors,
                                            "totalErrors": workspace.summary.total_errors,
                                            "totalWarnings": workspace.summary.total_warnings,
                                            "totalInfos": workspace.summary.total_infos,
                                            "totalHints": workspace.summary.total_hints,
                                        }),
                                        (WorkspaceCommand::Reindex, _) => json!({
                                            "revision": snapshot.revision(),
                                            "sourceFiles": snapshot.source_files().len(),
                                        }),
                                        (WorkspaceCommand::Validate, None) => json!({
                                            "revision": snapshot.revision(),
                                            "sourceFiles": snapshot.source_files().len(),
                                            "totalFiles": 0,
                                            "validatedFiles": 0,
                                            "filesWithErrors": 0,
                                            "totalErrors": 0,
                                            "totalWarnings": 0,
                                            "totalInfos": 0,
                                            "totalHints": 0,
                                        }),
                                    };
                                    json!({
                                        "jsonrpc": JSON_RPC_VERSION,
                                        "id": result.id,
                                        "result": result_value,
                                    })
                                }
                                Err(WorkspaceError::Cancelled) => {
                                    RpcError::new(REQUEST_CANCELLED, "request was cancelled")
                                        .response(result.id)
                                }
                                Err(error) => {
                                    let operation = match result.command {
                                        WorkspaceCommand::Reindex => "reindex",
                                        WorkspaceCommand::Validate => "validation",
                                    };
                                    RpcError::new(
                                        INTERNAL_ERROR,
                                        format!("workspace {operation} failed: {error}"),
                                    )
                                    .response(result.id)
                                }
                            }
                        };
                        self.arm_background_reindex();
                        write_message(&mut output, &response)?;
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
                            Ok((host, workspace)) => {
                                self.host = host;
                                self.invalidate_all_semantic_tokens();
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
                                if self.workspace_wide_diagnostics
                                    && let Some(workspace) = workspace.as_ref()
                                {
                                    self.publish_workspace_diagnostics(&mut output, workspace)?;
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
                    TransportEvent::WorkspaceDiagnostics(result) => {
                        let current = in_flight_workspace_diagnostics
                            .as_ref()
                            .is_some_and(|task| task.base_revision == result.base_revision);
                        if current {
                            let _task = in_flight_workspace_diagnostics
                                .take()
                                .expect("checked workspace diagnostics task");
                            if self.workspace_wide_diagnostics {
                                if self.host.snapshot().revision() != result.base_revision {
                                    // Any edit or source refresh while the worker was running
                                    // invalidates its snapshot. Re-run once newer foreground work
                                    // is complete.
                                    self.workspace_diagnostics_pending = true;
                                } else {
                                    match result.result {
                                        Ok(workspace) => {
                                            self.publish_workspace_diagnostics(
                                                &mut output,
                                                &workspace,
                                            )?;
                                        }
                                        Err(WorkspaceError::Cancelled) => {
                                            self.workspace_diagnostics_pending = true;
                                        }
                                        Err(error) => {
                                            write_message(
                                                &mut output,
                                                &show_warning_notification(format!(
                                                    "Workspace diagnostics failed: {error}"
                                                )),
                                            )?;
                                        }
                                    }
                                }
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
                        || in_flight_index.is_some()
                        || self.has_pending_disk_changes()
                        || in_flight_disk_changes.is_some()
                        || in_flight_background_reindex.is_some()
                        || in_flight_workspace_diagnostics.is_some()
                        || in_flight_reindex_command.is_some());
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
