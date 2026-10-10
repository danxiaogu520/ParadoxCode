//! Protocol boundary for the editor search panel; external-client requests retain their contracts.
use super::*;
use ide::search::{editor_definitions, editor_localisations_with_epoch, editor_name_score};

const PAGE_SIZE: usize = 50;
const MAX_QUERY_CHARS: usize = 1024;
const MAX_SEARCH_BUFFERS: usize = 128;
const MAX_SEARCH_BUFFER_BYTES: usize = 2 * 1024 * 1024;
const MAX_SEARCH_BUFFERS_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
enum SearchTab {
    Rules,
    Definitions,
    Localisation,
    Text,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EditorSearchParams {
    tab: SearchTab,
    query: String,
    roots: Option<Vec<u32>>,
    language: Option<String>,
    #[serde(default)]
    search_keys: bool,
    kind: Option<String>,
    context: Option<String>,
    scope: Option<String>,
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default)]
    corpus_epoch: u64,
    #[serde(default)]
    buffers: Vec<EditorBufferParams>,
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    status: SearchStatus,
    #[serde(default)]
    offset: usize,
    revision: Option<u64>,
    snapshot: Option<String>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EditorBufferParams {
    uri: String,
    version: i64,
    text: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
enum SearchStatus {
    #[default]
    All,
    Active,
    Overridden,
}

impl SearchStatus {
    fn accepts(self, active: bool) -> bool {
        match self {
            Self::All => true,
            Self::Active => active,
            Self::Overridden => !active,
        }
    }
}

impl SnapshotRequestContext {
    pub(super) fn editor_search_context(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        if params.is_some_and(|value| !value.is_null()) {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "search context does not accept parameters",
            ));
        }
        self.ensure_active()?;
        let roots = self.snapshot.source_roots().iter().map(|root| json!({
            "id":root.id.get(), "kind": match root.kind { SourceRootKind::Project => "project", SourceRootKind::Dependency => "dependency", SourceRootKind::Vanilla => "vanilla" },
            "path":root.path, "order":root.order, "writable":root.writable,
        })).collect::<Vec<_>>();
        let kinds = ide::search::editor_definition_kinds(&self.snapshot, &self.cancellation)
            .map_err(cancelled_error)?;
        let ir = self.snapshot.ir();
        let contexts = ir
            .schemas
            .iter()
            .map(|schema| ir.strings.resolve(schema.name))
            .collect::<std::collections::BTreeSet<_>>();
        let scopes = ir
            .fields
            .iter()
            .filter_map(|field| field.scope.as_ref())
            .flat_map(|scope| scope.scopes_in.iter())
            .map(|name| ir.strings.resolve(*name))
            .collect::<std::collections::BTreeSet<_>>();
        Ok(
            json!({ "revision":self.snapshot.revision(), "roots":roots, "languages":ide::search::editor_known_localisation_languages(&self.snapshot), "preferredLanguage":self.snapshot.localisation_preview_language(), "kinds":kinds, "contexts":contexts, "scopes":scopes, "ruleSet":self.snapshot.ir_fingerprint(), "gameId":self.snapshot.rules().game_id(), "ruleVersion":engine::LSP_VERSION }),
        )
    }

    pub(super) fn editor_search(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<EditorSearchParams>(params, "editor search")?;
        self.ensure_active()?;
        if params.query.chars().count() > MAX_QUERY_CHARS {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "search query exceeds 1024 characters",
            ));
        }
        if params.offset > 0 && params.revision != Some(self.snapshot.revision()) {
            return Err(RpcError::new(
                -32801,
                "workspace changed; restart the search",
            ));
        }
        validate_roots(&self.snapshot, params.roots.as_deref())?;
        engine::WorkspaceScanFilters::new(params.include.clone(), Vec::new())
            .map_err(|error| RpcError::new(INVALID_PARAMS, error.to_string()))?;
        engine::WorkspaceScanFilters::new(params.exclude.clone(), Vec::new())
            .map_err(|error| RpcError::new(INVALID_PARAMS, error.to_string()))?;
        if params.buffers.len() > MAX_SEARCH_BUFFERS
            || params.buffers.iter().any(|buffer| {
                buffer.text.len() > MAX_SEARCH_BUFFER_BYTES
                    || buffer.uri.len() > 8192
                    || buffer.version < 0
            })
            || params
                .buffers
                .iter()
                .map(|buffer| buffer.text.len())
                .sum::<usize>()
                > MAX_SEARCH_BUFFERS_BYTES
        {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "editor buffer payload exceeds the search budget",
            ));
        }
        if params.tab != SearchTab::Text && !params.buffers.is_empty() {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "inline buffers are only supported by full-text search",
            ));
        }
        let query = params.query.trim();
        if query.is_empty() {
            return Ok(
                json!({"revision":self.snapshot.revision(),"groups":[],"loaded":0,"totalGroups":0,"totalMatches":0,"nextOffset":null,"limitations":[]}),
            );
        }
        let accepts_root = |root: Option<engine::SourceRootId>| {
            root.is_some_and(|root| {
                params
                    .roots
                    .as_ref()
                    .is_none_or(|roots| roots.contains(&root.get()))
            })
        };
        let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut limitations = Vec::new();
        let mut corpus_generation = None;
        let languages = ide::search::editor_known_localisation_languages(&self.snapshot);
        match params.tab {
            SearchTab::Localisation => {
                return self.editor_localisation_page(&params, query);
            }
            SearchTab::Definitions => {
                if self.snapshot.scan_report().skipped_entries > 0
                    || !self.snapshot.scan_report().issues.is_empty()
                {
                    limitations.push("Workspace discovery reported missing or recovered source data; semantic search coverage may be incomplete.".to_owned());
                }
                for entry in editor_definitions(&self.snapshot, &self.cancellation)
                    .map_err(cancelled_error)?
                    .iter()
                {
                    self.ensure_active()?;
                    let Some(score) = editor_name_score(&entry.name, query) else {
                        continue;
                    };
                    if !accepts_root(entry.root)
                        || !params.status.accepts(entry.active)
                        || params.kind.as_ref().is_some_and(|kind| kind != &entry.kind)
                    {
                        continue;
                    }
                    let id = definition_id(entry);
                    groups.entry(format!("{}\u{0}{}",entry.kind,entry.name.to_ascii_lowercase())).or_default().push(json!({"id":id,"name":entry.name,"kind":entry.kind,"rootId":entry.root.map(|root|root.get()),"active":entry.active,"version":entry.version,"location":self.definition_search_location(entry)?,"score":score}));
                }
            }
            SearchTab::Rules => {
                for rule in ide::search::editor_rule_search(
                    &self.snapshot,
                    query,
                    params.context.as_deref(),
                    params.scope.as_deref(),
                    &self.cancellation,
                )
                .map_err(cancelled_error)?
                {
                    groups.insert(rule.id.clone(), vec![json!({"id":rule.id,"name":rule.name,"kind":rule.context,"text":search_excerpt(&rule.documentation,query),"hasFullText":rule.documentation.chars().count()>1000,"allowedScopes":rule.allowed_scopes,"pushScope":rule.push_scope,"valueRequirement":rule.value_requirement,"cardinality":{"min":rule.minimum,"max":rule.maximum},"deprecated":rule.deprecated})]);
                }
            }
            SearchTab::Text => {
                let mut buffer_ids = HashSet::new();
                let buffers = params
                    .buffers
                    .iter()
                    .map(|buffer| {
                        if !buffer_ids.insert(buffer.uri.as_str()) {
                            return Err(RpcError::new(INVALID_PARAMS, "duplicate text buffer URI"));
                        }
                        let id = DocumentId::new(&buffer.uri);
                        if let Some(document) = self.snapshot.document(&id)
                            && document.source() == engine::DocumentSource::Overlay
                        {
                            if document
                                .version()
                                .is_some_and(|version| version > buffer.version)
                            {
                                return Err(RpcError::new(
                                    -32801,
                                    "editor buffer changed; restart the search",
                                ));
                            }
                            if document.version() == Some(buffer.version)
                                && document.text() != buffer.text
                            {
                                return Err(RpcError::new(
                                    INVALID_PARAMS,
                                    "text differs for the same document version",
                                ));
                            }
                        }
                        let scheme = buffer
                            .uri
                            .split_once(':')
                            .map(|(scheme, _)| scheme.to_ascii_lowercase());
                        if !matches!(scheme.as_deref(), Some("file" | "pdcloc")) {
                            return Err(RpcError::new(
                                INVALID_PARAMS,
                                "text buffers require a physical file URI",
                            ));
                        }
                        let path = text::AbsPath::normalize(&transcode_target_path(&buffer.uri)?);
                        Ok(ide::search::EditorTextBuffer {
                            document: DocumentId::new(&buffer.uri),
                            path,
                            version: Some(buffer.version),
                            text: Arc::from(buffer.text.as_str()),
                        })
                    })
                    .collect::<Result<Vec<_>, RpcError>>()?;
                let data = ide::search::editor_text_search_query(
                    &self.snapshot,
                    &ide::search::EditorTextQuery {
                        query,
                        roots: params.roots.as_deref(),
                        case_sensitive: params.case_sensitive,
                        include: &params.include,
                        exclude: &params.exclude,
                        epoch: params.corpus_epoch,
                        buffers: &buffers,
                    },
                    &self.cancellation,
                )
                .map_err(cancelled_error)?;
                limitations = data.limitations;
                corpus_generation = Some(data.generation);
                let mut line_indices = HashMap::new();
                let mut line_anchors = HashMap::new();
                for hit in data.hits {
                    self.ensure_active()?;
                    let uri = hit
                        .document
                        .as_ref()
                        .map(|document| document.as_str().to_owned())
                        .or_else(|| {
                            FileUri::from_path(&hit.physical_path)
                                .ok()
                                .map(|uri| uri.as_str().to_owned())
                        });
                    let Some(uri) = uri else {
                        continue;
                    };
                    let index = line_indices
                        .entry((hit.root, hit.path.clone()))
                        .or_insert_with(|| LineIndex::new(&hit.text));
                    let range = range_to_lsp(index, &hit.text, hit.range);
                    let anchor = *line_anchors
                        .entry((hit.root, hit.path.clone(), range.start.line))
                        .or_insert_with(|| line_anchor(&hit.text, index, range.start.line));
                    let id = format!(
                        "text:{}:{}:{}",
                        hit.root.get(),
                        hit.path.as_str(),
                        hit.range.start()
                    );
                    groups.entry(format!("{}:{}",hit.root.get(),hit.path.as_str())).or_default().push(json!({"id":id,"name":hit.path.as_str(),"kind":"text","text":hit.preview,"context":hit.context,"rootId":hit.root.get(),"version":hit.version,"location":{"uri":uri,"path":hit.path.as_str(),"range":range,"expectedText":source_fragment(&hit.text,hit.range),"lineAnchor":anchor,"sourceLines":index.line_count()}}));
                }
            }
        }
        let mut signature = params.clone();
        signature.offset = 0;
        signature.revision = None;
        signature.snapshot = None;
        let signature = serde_json::to_string(&signature)
            .map_err(|error| RpcError::new(crate::INTERNAL_ERROR, error.to_string()))?;
        let query_snapshot = search_snapshot(
            &groups,
            &self.snapshot,
            &format!("{signature}:{corpus_generation:?}"),
        );
        if params.offset > 0 && params.snapshot.as_deref() != Some(&query_snapshot) {
            return Err(RpcError::new(
                -32801,
                "search snapshot changed; restart the search",
            ));
        }
        let total_matches: usize = groups.values().map(Vec::len).sum();
        let mut groups = groups
            .into_iter()
            .map(|(id, mut versions)| {
                versions.sort_by_key(|entry| {
                    (
                        !entry["active"].as_bool().unwrap_or(true),
                        entry["rootId"].as_u64(),
                        entry["id"].as_str().unwrap_or("").to_owned(),
                    )
                });
                (id, versions)
            })
            .collect::<Vec<_>>();
        groups.sort_by_key(|(id, versions)| {
            (
                versions[0]["score"].as_u64().unwrap_or(0),
                versions[0]["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_ascii_lowercase(),
                id.clone(),
            )
        });
        let total_groups = groups.len();
        let mut page = search_page(groups, params.offset, &mut limitations)?;
        if params.tab == SearchTab::Definitions {
            for group in &mut page {
                if let Some(versions) = group["versions"].as_array_mut() {
                    for entry in versions {
                        if let (Some(kind), Some(name)) =
                            (entry["kind"].as_str(), entry["name"].as_str())
                        {
                            entry["title"] = json!(
                                ide::search::editor_definition_title(
                                    &self.snapshot,
                                    kind,
                                    name,
                                    &self.cancellation
                                )
                                .map_err(cancelled_error)?
                            );
                        }
                    }
                }
            }
        }
        let loaded = params.offset.saturating_add(page.len());
        self.ensure_active()?;
        cap_limitations(&mut limitations);
        Ok(
            json!({"revision":self.snapshot.revision(),"snapshot":query_snapshot,"groups":page,"loaded":loaded,"totalGroups":total_groups,"totalMatches":total_matches,"languages":languages,"nextOffset":(loaded < total_groups).then_some(loaded),"limitations":limitations}),
        )
    }

    /// Count and order immutable matches first; only the requested page needs positions and JSON.
    fn editor_localisation_page(
        &self,
        params: &EditorSearchParams,
        query: &str,
    ) -> Result<Value, RpcError> {
        let data = editor_localisations_with_epoch(
            &self.snapshot,
            params.corpus_epoch,
            &self.cancellation,
        )
        .map_err(cancelled_error)?;
        let matcher = ide::search::LocalisationQuery::new(query);
        let mut matching =
            BTreeMap::<(&str, &str), Vec<&ide::search::EditorLocalisationEntry>>::new();
        for entry in &data.entries {
            self.ensure_active()?;
            if entry.root().is_none_or(|root| {
                params
                    .roots
                    .as_ref()
                    .is_some_and(|roots| !roots.contains(&root.get()))
            }) || !params.status.accepts(entry.active)
                || params
                    .language
                    .as_ref()
                    .is_some_and(|language| language != entry.language())
                || !matcher.matches(if params.search_keys {
                    entry.key()
                } else {
                    entry.value()
                })
            {
                continue;
            }
            matching
                .entry((entry.normalised_key(), entry.language()))
                .or_default()
                .push(entry);
        }
        let total_groups = matching.len();
        let total_matches: usize = matching.values().map(Vec::len).sum();
        if params.offset > total_groups {
            return Err(RpcError::new(INVALID_PARAMS, "invalid search offset"));
        }
        let mut signature = params.clone();
        signature.offset = 0;
        signature.revision = None;
        signature.snapshot = None;
        let signature = serde_json::to_string(&signature)
            .map_err(|error| RpcError::new(crate::INTERNAL_ERROR, error.to_string()))?;
        // The corpus generation fixes every entry, source range and activity flag in this snapshot.
        let query_snapshot = search_snapshot(
            &BTreeMap::new(),
            &self.snapshot,
            &format!("{signature}:localisation:{}", data.generation),
        );
        if params.offset > 0 && params.snapshot.as_deref() != Some(&query_snapshot) {
            return Err(RpcError::new(
                -32801,
                "search snapshot changed; restart the search",
            ));
        }
        let mut limitations = data.limitations.clone();
        let mut line_indices = HashMap::new();
        let mut line_anchors = HashMap::new();
        let mut page = Vec::new();
        let mut rendered_count = 0;
        for ((key, language), mut versions) in
            matching.into_iter().skip(params.offset).take(PAGE_SIZE)
        {
            self.ensure_active()?;
            versions
                .sort_by_cached_key(|entry| (!entry.active, entry.root(), localisation_id(entry)));
            let match_count = versions.len();
            if match_count > 100 {
                limitations.push(format!("A result group has {match_count} confirmed matches; showing 100. Narrow the query or source selection to inspect the remaining matches."));
            }
            let version_count = match_count.min(100);
            if rendered_count + version_count > 200 && !page.is_empty() {
                break;
            }
            rendered_count += version_count;
            let mut rendered = Vec::new();
            for entry in versions.into_iter().take(100) {
                self.ensure_active()?;
                let source_location = entry.location();
                let precise_location = if params.search_keys {
                    Some(source_location.clone())
                } else {
                    ide::search::editor_localisation_match_location(entry, query)
                };
                let chosen = precise_location.as_ref().unwrap_or(&source_location);
                let mut location = self.search_location(chosen)?;
                let line_index = line_indices
                    .entry((entry.document().cloned(), entry.file()))
                    .or_insert_with(|| LineIndex::new(entry.source()));
                let range = range_to_lsp(line_index, entry.source(), chosen.range);
                let anchor = line_anchors
                    .entry((entry.document().cloned(), entry.file(), range.start.line))
                    .or_insert_with(|| line_anchor(entry.source(), line_index, range.start.line));
                location["lineAnchor"] = json!(*anchor);
                location["sourceLines"] = json!(line_index.line_count());
                location["range"] = json!(range);
                location["expectedText"] = json!(source_fragment(entry.source(), chosen.range));
                rendered.push(json!({"id":localisation_id(entry),"name":entry.key(),"kind":"localisation","text":search_excerpt(entry.value(),query),
                    "hasFullText":entry.full_value_available() && entry.value().chars().count()>1000,"fullValueAvailable":entry.full_value_available(),
                    "corpusGeneration":data.generation,"language":entry.language(),"rootId":entry.root().map(|root|root.get()),"active":entry.active,
                    "version":entry.version(),"location":location,"precision":if precise_location.is_some() {"match"}else{"entry"}}));
            }
            page.push(json!({"id":format!("{key}\u{0}{language}"),"versions":rendered,"matchCount":match_count}));
        }
        let loaded = params.offset.saturating_add(page.len());
        self.ensure_active()?;
        cap_limitations(&mut limitations);
        Ok(
            json!({"revision":self.snapshot.revision(),"snapshot":query_snapshot,"groups":page,"loaded":loaded,
            "totalGroups":total_groups,"totalMatches":total_matches,"languages":ide::search::editor_localisation_languages(&data),
            "nextOffset":(loaded<total_groups).then_some(loaded),"limitations":limitations}),
        )
    }

    fn search_location(&self, location: &ide::Location) -> Result<Value, RpcError> {
        let lsp = location_to_lsp(&self.snapshot, location).ok_or_else(|| {
            RpcError::new(
                crate::INTERNAL_ERROR,
                "search result has no opening location",
            )
        })?;
        Ok(
            json!({"uri":lsp.uri,"range":lsp.range,"path":location.path.as_ref().map(LogicalPath::as_str)}),
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EditorReferencesParams {
    id: String,
    name: String,
    kind: String,
    revision: u64,
    #[serde(default)]
    corpus_epoch: u64,
    snapshot: Option<String>,
    roots: Option<Vec<u32>>,
    #[serde(default)]
    offset: usize,
}

impl SnapshotRequestContext {
    pub(super) fn editor_search_references(
        &self,
        params: Option<&Value>,
    ) -> Result<Value, RpcError> {
        let params = typed_params::<EditorReferencesParams>(params, "editor references")?;
        self.ensure_active()?;
        if params.revision != self.snapshot.revision() {
            return Err(RpcError::new(
                -32801,
                "workspace changed; restart the search",
            ));
        }
        validate_roots(&self.snapshot, params.roots.as_deref())?;
        let active = if params.kind == "localisation" {
            editor_localisations_with_epoch(&self.snapshot, params.corpus_epoch, &self.cancellation)
                .map_err(cancelled_error)?
                .entries
                .iter()
                .find(|entry| localisation_id(entry) == params.id && entry.key() == params.name)
                .map(|entry| entry.active)
        } else {
            editor_definitions(&self.snapshot, &self.cancellation)
                .map_err(cancelled_error)?
                .iter()
                .find(|entry| {
                    definition_id(entry) == params.id
                        && entry.name == params.name
                        && entry.kind == params.kind
                })
                .map(|entry| entry.active)
        }
        .ok_or_else(|| {
            RpcError::new(
                INVALID_PARAMS,
                "reference target is not a search definition",
            )
        })?;
        let cache_issues = self.snapshot.reference_source_issues();
        let invalid_cache_roots = cache_issues
            .iter()
            .map(|(root, _)| *root)
            .collect::<HashSet<_>>();
        let mut limitations = Vec::new();
        limitations.extend(cache_issues.into_iter().map(|(_, issue)| issue));
        let mut files: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        if !active && params.kind != "localisation" {
            limitations.push("This definition version is overridden. References resolving to the active version are not attributed to it; use full-text search for name occurrences.".to_owned());
        } else if let Some((_, references)) = symbol_references_with_cancellation(
            &self.snapshot,
            &params.kind,
            &params.name,
            false,
            &self.cancellation,
        )
        .map_err(cancelled_error)?
        {
            for location in references {
                self.ensure_active()?;
                let root = location
                    .file
                    .and_then(|id| self.snapshot.source_files().get(&id))
                    .map(|file| file.root_id)
                    .or_else(|| {
                        location
                            .document
                            .as_ref()
                            .and_then(|id| self.snapshot.document(id))
                            .and_then(|document| document.path())
                            .and_then(|path| {
                                self.snapshot
                                    .source_roots()
                                    .iter()
                                    .filter(|root| path.starts_with(&root.path))
                                    .max_by_key(|root| root.path.as_os_str().len())
                                    .map(|root| root.id)
                            })
                    });
                if root.is_some_and(|root| invalid_cache_roots.contains(&root)) {
                    continue;
                }
                if params
                    .roots
                    .as_ref()
                    .is_some_and(|roots| root.is_none_or(|root| !roots.contains(&root.get())))
                {
                    continue;
                }
                let lsp = self.search_location(&location)?;
                let id = format!(
                    "ref:{}:{}:{}",
                    root.map_or(0, |root| root.get()),
                    location.path.as_ref().map_or("", LogicalPath::as_str),
                    location.range.start()
                );
                let group = format!(
                    "{}:{}",
                    root.map_or(0, |root| root.get()),
                    location.path.as_ref().map_or("", LogicalPath::as_str)
                );
                let version = location
                    .document
                    .as_ref()
                    .and_then(|id| self.snapshot.document(id))
                    .and_then(|document| document.version());
                files.entry(group).or_default().push(json!({"id":id,"name":params.name,"kind":params.kind,"rootId":root.map(|root|root.get()),"version":version,"location":lsp}));
            }
        } else {
            limitations.push("The semantic target cannot currently be resolved; no text occurrences have been counted as confirmed references.".to_owned());
        }
        let notes = vec![
            "Only references known to the analyser are counted. Runtime-computed names may require full-text search.",
        ];
        let query_snapshot = search_snapshot(
            &files,
            &self.snapshot,
            &format!(
                "{}:{}:{}:{:?}:{}",
                params.id, params.kind, params.name, params.roots, params.corpus_epoch
            ),
        );
        if params.offset > 0 && params.snapshot.as_deref() != Some(&query_snapshot) {
            return Err(RpcError::new(
                -32801,
                "reference snapshot changed; restart the search",
            ));
        }
        let total_groups = files.len();
        let total_matches: usize = files.values().map(Vec::len).sum();
        let page = search_page(files.into_iter().collect(), params.offset, &mut limitations)?;
        cap_limitations(&mut limitations);
        let loaded = params.offset + page.len();
        Ok(
            json!({"revision":self.snapshot.revision(),"snapshot":query_snapshot,"groups":page,"loaded":loaded,"totalGroups":total_groups,"totalMatches":total_matches,"languages":ide::search::editor_known_localisation_languages(&self.snapshot),"nextOffset":(loaded<total_groups).then_some(loaded),"limitations":limitations,"notes":notes}),
        )
    }
}

fn localisation_id(entry: &ide::search::EditorLocalisationEntry) -> String {
    format!(
        "loc:{}:{}:{}:{}",
        entry.root().map_or(0, |root| root.get()),
        entry.path().map_or("", LogicalPath::as_str),
        entry.key_range().start(),
        entry.language()
    )
}

fn definition_id(entry: &ide::search::EditorDefinition) -> String {
    format!(
        "def:{}:{}:{}:{}",
        entry.root.map_or(0, |root| root.get()),
        entry.location.path.as_ref().map_or("", LogicalPath::as_str),
        entry.location.range.start(),
        entry.kind
    )
}

fn search_excerpt(value: &str, query: &str) -> String {
    if value.chars().count() <= 1000 {
        return value.to_owned();
    }
    let needle = query.split_whitespace().next().unwrap_or("").to_lowercase();
    let folded = value.to_lowercase();
    let offset = folded.find(&needle).unwrap_or(0).min(value.len());
    let mut boundary = offset;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let start = value[..boundary].chars().count().saturating_sub(120);
    let excerpt = value.chars().skip(start).take(1000).collect::<String>();
    format!("{}{}…", if start > 0 { "…" } else { "" }, excerpt)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EditorDetailParams {
    id: String,
    revision: u64,
    #[serde(default)]
    corpus_epoch: u64,
    corpus_generation: Option<u64>,
}

impl SnapshotRequestContext {
    pub(super) fn editor_search_detail(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = typed_params::<EditorDetailParams>(params, "editor search detail")?;
        self.ensure_active()?;
        if params.revision != self.snapshot.revision() {
            return Err(RpcError::new(
                -32801,
                "workspace changed; restart the search",
            ));
        }
        if params.id.starts_with("ir:") {
            if let Some(rule) =
                ide::search::editor_rule_search(&self.snapshot, "", None, None, &self.cancellation)
                    .map_err(cancelled_error)?
                    .into_iter()
                    .find(|rule| rule.id == params.id)
            {
                return Ok(json!({"id":params.id,"text":rule.documentation,"truncated":false}));
            }
            return Err(RpcError::new(INVALID_PARAMS, "rule detail is unavailable"));
        }
        let data = editor_localisations_with_epoch(
            &self.snapshot,
            params.corpus_epoch,
            &self.cancellation,
        )
        .map_err(cancelled_error)?;
        if params
            .corpus_generation
            .is_some_and(|generation| generation != data.generation)
        {
            return Err(RpcError::new(
                -32801,
                "value snapshot changed; restart the search",
            ));
        }
        if let Some(entry) = data
            .entries
            .iter()
            .find(|entry| localisation_id(entry) == params.id)
        {
            if !entry.full_value_available() {
                return Err(RpcError::new(
                    -32803,
                    "the complete readable value is unavailable in this source view",
                ));
            }
            let truncated = entry.value().chars().count() > 128_000;
            return Ok(
                json!({"id":params.id,"text":entry.value().chars().take(128_000).collect::<String>(),"truncated":truncated}),
            );
        }
        Err(RpcError::new(
            INVALID_PARAMS,
            "detail target is unavailable",
        ))
    }
}

fn search_snapshot(
    groups: &BTreeMap<String, Vec<Value>>,
    snapshot: &AnalysisSnapshot,
    signature: &str,
) -> String {
    use std::hash::{Hash, Hasher};
    let mut digest = std::collections::hash_map::DefaultHasher::new();
    static SESSION: std::sync::OnceLock<u128> = std::sync::OnceLock::new();
    SESSION
        .get_or_init(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        })
        .hash(&mut digest);
    let revision = snapshot.revision();
    revision.hash(&mut digest);
    snapshot.host_identity().hash(&mut digest);
    signature.hash(&mut digest);
    for (id, versions) in groups {
        id.hash(&mut digest);
        for version in versions {
            version.to_string().hash(&mut digest);
        }
    }
    format!("{revision}:{:016x}", digest.finish())
}

fn cap_limitations(limitations: &mut Vec<String>) {
    if limitations.len() > 100 {
        let omitted = limitations.len() - 100;
        limitations.truncate(100);
        limitations.push(format!("{omitted} additional coverage limitations omitted"));
    }
}

fn validate_roots(snapshot: &AnalysisSnapshot, roots: Option<&[u32]>) -> Result<(), RpcError> {
    if let Some(roots) = roots
        && (roots.len() > snapshot.source_roots().len()
            || roots.iter().any(|id| {
                !snapshot
                    .source_roots()
                    .iter()
                    .any(|root| root.id.get() == *id)
            }))
    {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "selected sources are no longer available; refresh the source selection",
        ));
    }
    Ok(())
}

fn source_fragment(source: &str, range: TextRange) -> Option<&str> {
    source
        .get(range.start() as usize..range.end() as usize)
        .filter(|text| text.len() <= 8192)
}

fn search_page(
    groups: Vec<(String, Vec<Value>)>,
    offset: usize,
    limitations: &mut Vec<String>,
) -> Result<Vec<Value>, RpcError> {
    if offset > groups.len() {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "search offset exceeds the result count",
        ));
    }
    let mut page = Vec::new();
    let mut entries = 0;
    for (id, mut versions) in groups.into_iter().skip(offset).take(PAGE_SIZE) {
        let match_count = versions.len();
        if match_count > 100 {
            limitations.push(format!("A result group has {match_count} confirmed matches; showing 100. Narrow the query or source selection to inspect the remaining matches."));
            versions.truncate(100);
        }
        if entries + versions.len() > 200 && !page.is_empty() {
            break;
        }
        entries += versions.len();
        page.push(json!({"id":id,"versions":versions,"matchCount":match_count}));
    }
    Ok(page)
}

impl SnapshotRequestContext {
    fn definition_search_location(
        &self,
        entry: &ide::search::EditorDefinition,
    ) -> Result<Value, RpcError> {
        let mut location = self.search_location(&entry.location)?;
        let source = entry
            .location
            .document
            .as_ref()
            .and_then(|id| self.snapshot.document(id))
            .map(|document| document.text())
            .or_else(|| {
                entry
                    .location
                    .file
                    .and_then(|id| self.snapshot.source_text(id))
            });
        if let Some(fragment) =
            source.and_then(|source| source_fragment(source, entry.location.range))
        {
            location["expectedText"] = json!(fragment);
        } else {
            location["expectedName"] = json!(entry.name);
        }
        Ok(location)
    }
}

fn line_anchor(source: &str, index: &LineIndex, line: u32) -> u32 {
    let start = index.offset(source, Position::new(line, 0)).unwrap_or(0) as usize;
    let end = index
        .offset(source, Position::new(line.saturating_add(1), 0))
        .map_or(source.len(), |offset| offset as usize);
    source
        .get(start..end)
        .unwrap_or("")
        .bytes()
        .filter(|byte| matches!(byte, 0x20..=0x7e))
        .fold(0x811c9dc5_u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(16777619)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{first_party_host, temp_workspace_dir};
    use engine::{SourceRoot, SourceRootId, WorkspaceChange};
    use text::AbsPath;

    fn context(host: &engine::AnalysisHost) -> SnapshotRequestContext {
        SnapshotRequestContext::new(
            host.snapshot(),
            CancellationToken::new(),
            false,
            Arc::new(HashSet::new()),
            Arc::new(BTreeMap::new()),
            Arc::new(GlobIncludePatterns::default()),
            Arc::new(SemanticTokensCache::default()),
        )
    }

    #[test]
    fn editor_search_pages_keep_snapshot_and_source_filter_does_not_promote_old_versions() {
        let (root, _) = temp_workspace_dir();
        let mut roots = Vec::new();
        for (id, name, kind) in [
            (1, "dependency", SourceRootKind::Dependency),
            (2, "mod", SourceRootKind::Project),
        ] {
            let path = root.join(name);
            std::fs::create_dir_all(path.join("localisation")).unwrap();
            let mut source = format!("l_english:\nshared:0 \"{name} needle\"\n");
            for index in 0..63 {
                source.push_str(&format!("key_{index:03}:0 \"needle\"\n"));
            }
            std::fs::write(
                path.join(format!("localisation/{name}_l_english.yml")),
                source,
            )
            .unwrap();
            roots.push(SourceRoot::new(
                SourceRootId::new(id),
                kind,
                AbsPath::normalize(&path),
            ));
        }
        let mut host = first_party_host();
        host.apply_change(WorkspaceChange::SetSourceRoots(roots));
        host.refresh_source_roots().unwrap();
        let adapter = context(&host);
        let first = adapter
            .editor_search(Some(&json!({"tab":"localisation","query":"needle"})))
            .unwrap();
        let next=adapter.editor_search(Some(&json!({"tab":"localisation","query":"needle","offset":first["nextOffset"],"revision":first["revision"],"snapshot":first["snapshot"]}))).unwrap();
        assert_eq!(next["loaded"], 64);
        assert!(next["nextOffset"].is_null());
        let first_ids = first["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|group| group["id"].as_str().unwrap())
            .collect::<HashSet<_>>();
        assert!(
            next["groups"]
                .as_array()
                .unwrap()
                .iter()
                .all(|group| !first_ids.contains(group["id"].as_str().unwrap()))
        );
        let old = adapter
            .editor_search(Some(
                &json!({"tab":"localisation","query":"dependency","roots":[1]}),
            ))
            .unwrap();
        assert_eq!(old["totalMatches"], 1);
        assert_eq!(old["groups"][0]["versions"][0]["active"], false);
        let active = adapter
            .editor_search(Some(
                &json!({"tab":"localisation","query":"needle","roots":[1],"status":"active"}),
            ))
            .unwrap();
        assert_eq!(
            active["totalMatches"], 0,
            "hiding the Mod cannot promote dependency translations"
        );
        let overridden = adapter
            .editor_search(Some(
                &json!({"tab":"localisation","query":"needle","roots":[1],"status":"overridden"}),
            ))
            .unwrap();
        assert_eq!(overridden["totalMatches"], 64);
        let absent_language = adapter
            .editor_search(Some(
                &json!({"tab":"localisation","query":"needle","language":"french"}),
            ))
            .unwrap();
        assert_eq!(
            absent_language["totalMatches"], 0,
            "a missing selected language cannot match English fallback text"
        );
        let wrong=adapter.editor_search(Some(&json!({"tab":"localisation","query":"needle","offset":50,"revision":first["revision"],"snapshot":"unrelated query"})));
        assert!(wrong.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn editor_search_reference_view_does_not_attribute_active_uses_to_old_definition() {
        let (root, _) = temp_workspace_dir();
        let mut roots = Vec::new();
        for (id, name, kind) in [
            (1, "dependency", SourceRootKind::Dependency),
            (2, "mod", SourceRootKind::Project),
        ] {
            let path = root.join(name);
            std::fs::create_dir_all(path.join("common/scripted_effects")).unwrap();
            std::fs::write(
                path.join(format!("common/scripted_effects/{name}.txt")),
                "search_effect = { add_prestige = 1 }\n",
            )
            .unwrap();
            roots.push(SourceRoot::new(
                SourceRootId::new(id),
                kind,
                AbsPath::normalize(&path),
            ));
        }
        std::fs::create_dir_all(root.join("mod/events")).unwrap();
        std::fs::write(root.join("mod/events/uses.txt"),"country_event = { id = search.1 immediate = { search_effect = yes } }\n# search_effect\n").unwrap();
        let mut host = first_party_host();
        host.apply_change(WorkspaceChange::SetSourceRoots(roots));
        host.refresh_source_roots().unwrap();
        let adapter = context(&host);
        let result = adapter
            .editor_search(Some(&json!({"tab":"definitions","query":"search_effect"})))
            .unwrap();
        let versions = result["groups"][0]["versions"].as_array().unwrap();
        assert_eq!(versions.len(), 2);
        for entry in versions {
            let references=adapter.editor_search_references(Some(&json!({"id":entry["id"],"name":entry["name"],"kind":entry["kind"],"revision":result["revision"]}))).unwrap();
            assert_eq!(
                references["totalMatches"],
                if entry["active"] == true { 1 } else { 0 }
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn editor_search_cache_values_keep_exact_ranges_and_offline_coverage_is_explicit() {
        let (root, _) = temp_workspace_dir();
        let source = root.join("game");
        std::fs::create_dir_all(source.join("localisation")).unwrap();
        let path = source.join("localisation/cached_l_english.yml");
        std::fs::write(
            &path,
            format!("l_english:\nkey:0 \"{} needle\"\n", "a".repeat(1500)),
        )
        .unwrap();
        let mut builder = first_party_host();
        builder.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(0),
            SourceRootKind::Vanilla,
            AbsPath::normalize(&source),
        )]));
        builder.refresh_source_roots().unwrap();
        let cache_path = root.join("cache.pdcindex");
        engine::IndexCache::from_snapshot(&builder.snapshot())
            .unwrap()
            .save(&cache_path)
            .unwrap();
        let mut host = first_party_host();
        host.install_index_cache(engine::IndexCache::load(&cache_path).unwrap())
            .unwrap();
        assert!(
            host.snapshot()
                .source_files()
                .keys()
                .all(|id| host.snapshot().source_text(*id).is_none())
        );
        let result = context(&host)
            .editor_search(Some(&json!({"tab":"localisation","query":"needle"})))
            .unwrap();
        let entry = &result["groups"][0]["versions"][0];
        assert_eq!(entry["location"]["range"]["start"]["line"], 1);
        assert!(
            entry["location"]["range"]["start"]["character"]
                .as_u64()
                .unwrap()
                > 1500
        );
        assert_eq!(entry["location"]["expectedText"], "needle");
        let revision = host.snapshot().revision();
        std::fs::write(&path, "l_english:\nkey:0 \"updated needle\"\n").unwrap();
        let changed = context(&host)
            .editor_search(Some(
                &json!({"tab":"localisation","query":"updated","corpusEpoch":1}),
            ))
            .unwrap();
        assert_eq!(changed["totalMatches"], 1);
        assert_eq!(host.snapshot().revision(), revision);
        std::fs::remove_dir_all(&source).unwrap();
        let mut offline = first_party_host();
        offline
            .install_index_cache(engine::IndexCache::load(&cache_path).unwrap())
            .unwrap();
        let result = context(&offline)
            .editor_search(Some(&json!({"tab":"localisation","query":"needle"})))
            .unwrap();
        assert_eq!(result["totalMatches"], 0);
        assert!(!result["limitations"].as_array().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn editor_search_output_budgets_keep_confirmed_counts_and_reject_invalid_offsets() {
        let groups = vec![(
            "large-file".to_owned(),
            (0..350).map(|id| json!({"id":id})).collect(),
        )];
        let mut limitations = Vec::new();
        let page = search_page(groups, 0, &mut limitations).unwrap();
        assert_eq!(page[0]["matchCount"], 350);
        assert_eq!(page[0]["versions"].as_array().unwrap().len(), 100);
        assert!(!limitations.is_empty());
        assert!(search_page(Vec::new(), usize::MAX, &mut Vec::new()).is_err());
    }

    #[test]
    fn localisation_pages_preserve_the_total_entry_budget_with_many_versions() {
        let (root, _) = temp_workspace_dir();
        std::fs::create_dir_all(root.join("localisation")).unwrap();
        let mut source = String::from("l_english:\n");
        for group in 0..9 {
            for version in 0..30 {
                source.push_str(&format!("key_{group}:0 \"needle {version}\"\n"));
            }
        }
        std::fs::write(root.join("localisation/owned.yml"), source).unwrap();
        let mut host = first_party_host();
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::Project,
            AbsPath::normalize(&root),
        )]));
        host.refresh_source_roots().unwrap();
        let adapter = context(&host);
        let first = adapter
            .editor_search(Some(&json!({"tab":"localisation","query":"needle"})))
            .unwrap();
        assert_eq!(first["totalGroups"], 9);
        assert_eq!(first["totalMatches"], 270);
        assert_eq!(first["groups"].as_array().unwrap().len(), 6);
        assert_eq!(first["nextOffset"], 6);
        let second = adapter
            .editor_search(Some(&json!({"tab":"localisation","query":"needle",
            "offset":6,"revision":first["revision"],"snapshot":first["snapshot"]})))
            .unwrap();
        assert_eq!(second["loaded"], 9);
        assert!(second["nextOffset"].is_null());
        assert_eq!(second["groups"].as_array().unwrap().len(), 3);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn editor_search_inline_buffer_transport_preserves_snapshot_and_document_identity() {
        let (root, _) = temp_workspace_dir();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        let path = root.join("notes/readme.md");
        std::fs::write(&path, "disk text").unwrap();
        let uri = crate::tests::file_uri_string(&path);
        let mut host = first_party_host();
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::Project,
            AbsPath::normalize(&root),
        )]));
        host.refresh_source_roots().unwrap();
        let adapter = context(&host);
        let query = json!({"tab":"text","query":"needle","buffers":[{"uri":uri,"version":4,"text":"unsaved needle"}]});
        let result = adapter.editor_search(Some(&query)).unwrap();
        assert_eq!(result["totalMatches"], 1);
        assert_eq!(result["groups"][0]["versions"][0]["version"], 4);
        assert_eq!(result["groups"][0]["versions"][0]["location"]["uri"], uri);
        let mut wrong = query.clone();
        wrong["offset"] = json!(1);
        wrong["revision"] = result["revision"].clone();
        wrong["snapshot"] = result["snapshot"].clone();
        wrong["corpusEpoch"] = json!(1);
        assert!(adapter.editor_search(Some(&wrong)).is_err());
        let mut invalid = query.clone();
        invalid["buffers"][0]["uri"] = json!("https://example.invalid/buffer");
        assert!(adapter.editor_search(Some(&invalid)).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "disk text");
        std::fs::remove_dir_all(root).unwrap();
    }
}
