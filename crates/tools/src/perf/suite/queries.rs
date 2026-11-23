use super::fixture::{
    Fixture, GFX, GFX_PATH, LOC_PATH, MISSION, SCRIPT_PATH, TERRAIN, TERRAIN_PATH,
};
use super::metrics::{Metrics, record};
use crate::lsp::Client;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

pub struct Session<'a> {
    pub client: Client,
    pub measurements: &'a mut Metrics,
    pub timeout: Duration,
    pub verified_methods: BTreeSet<String>,
}

pub enum Expect<'a> {
    Populated(&'a str),
    Equals(&'a str, Value),
    WorkspaceRename { external_index: bool },
    PositiveCount(&'a str),
}

impl Expect<'_> {
    pub(super) fn check(&self, value: &Value) -> Result<usize, String> {
        let (path, expected) = match self {
            Self::Populated(path) => (*path, None),
            Self::Equals(path, expected) => (*path, Some(expected)),
            Self::WorkspaceRename { .. } => ("/changes", None),
            Self::PositiveCount(path) => {
                return value
                    .pointer(path)
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .ok_or_else(|| format!("expected positive work count at {path}"));
            }
        };
        let actual = value
            .pointer(path)
            .ok_or_else(|| format!("missing response field {path}"))?;
        let count = match actual {
            Value::Array(items) => items.len(),
            Value::Object(fields) => fields.len(),
            Value::String(s) => usize::from(!s.is_empty()),
            _ => usize::from(!actual.is_null()),
        };
        if expected.map_or(count > 0, |expected| expected == actual) {
            Ok(count)
        } else {
            Err(format!("unexpected result at {path} (cardinality {count})"))
        }
    }
}

impl Session<'_> {
    pub fn query(
        &mut self,
        name: &str,
        method: &str,
        params: Value,
        check: Expect<'_>,
    ) -> Result<Value, String> {
        let start = Instant::now();
        let id = self.client.send_request(method, params)?;
        let (mut response, at) = self
            .client
            .wait_response(id, self.timeout)
            .map_err(|e| format!("{name}: {e}"))?;
        if let Some(error) = response.get("error") {
            if matches!(
                check,
                Expect::WorkspaceRename {
                    external_index: true
                }
            ) && error["code"] == -32602
                && error["message"]
                    == "rename unavailable: Template reference closure or source mapping is incomplete"
            {
                record(
                    self.measurements,
                    &format!("{name}-incomplete"),
                    at.duration_since(start),
                    None,
                );
                self.verified_methods.insert(method.to_owned());
                return Ok(Value::Null);
            }
            return Err(format!("{name}: {error}"));
        }
        let value = response
            .get_mut("result")
            .ok_or_else(|| format!("{name}: missing result"))?
            .take();
        let count = check.check(&value).map_err(|e| format!("{name}: {e}"))?;
        self.verified_methods.insert(method.to_owned());
        record(
            self.measurements,
            name,
            at.duration_since(start),
            Some(count),
        );
        Ok(value)
    }

    /// Each round runs the same editor request sequence on an unchanged document.
    pub fn queries(&mut self, f: &Fixture, phase: &str) -> Result<(), String> {
        use Expect::{Equals, Populated};
        let doc = f.document(SCRIPT_PATH)?;
        let terrain = f.document(TERRAIN_PATH)?;
        let pos = |needle| json!({"textDocument":doc,"position":f.position(needle)});
        let document = json!({"textDocument":doc});
        let range = json!({"start":{"line":0,"character":0},"end":{"line":6,"character":0}});
        let mut call = |label: &str, method: &str, params: Value, expected: Expect| {
            self.query(&format!("{phase}/{label}"), method, params, expected)
        };

        let list = call(
            "completion",
            "textDocument/completion",
            pos("add_pres"),
            Populated("/items"),
        )?;
        let item = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["label"] == "add_prestige")
            .ok_or("completion omitted add_prestige")?;
        let quoted = call(
            "quoted-completion",
            "textDocument/completion",
            pos("BODY = \"add_pre"),
            Populated("/items"),
        )?;
        if !quoted["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["label"] == "add_prestige")
        {
            return Err("quoted completion omitted add_prestige".into());
        }
        call(
            "localisation-hover",
            "textDocument/hover",
            json!({"textDocument":f.document(LOC_PATH)?,"position":{"line":2,"character":22}}),
            Populated("/contents"),
        )?;
        call(
            "localisation-definition",
            "textDocument/definition",
            pos("title = perf_ti"),
            Populated(""),
        )?;
        call(
            "completion-resolve",
            "completionItem/resolve",
            item.clone(),
            Equals("/label", json!("add_prestige")),
        )?;
        call(
            "hover",
            "textDocument/hover",
            pos("add_pres"),
            Populated("/contents"),
        )?;
        call(
            "definition",
            "textDocument/definition",
            pos("perf_eff"),
            Populated(""),
        )?;
        let mut references = pos("perf_eff");
        references["context"] = json!({"includeDeclaration":true});
        call(
            "references",
            "textDocument/references",
            references,
            Populated(""),
        )?;
        call(
            "prepare-rename",
            "textDocument/prepareRename",
            pos("perf_eff"),
            Populated("/range"),
        )?;
        let mut rename = pos("perf_eff");
        rename["newName"] = json!("perf_renamed_effect");
        call(
            "rename",
            "textDocument/rename",
            rename,
            Expect::WorkspaceRename {
                external_index: f.options.get("vanillaIndexCache").is_some(),
            },
        )?;
        let effects = f.document(super::fixture::EFFECTS_PATH)?;
        let col = super::fixture::EFFECTS
            .lines()
            .nth(1)
            .unwrap()
            .find("$BODY$")
            .unwrap()
            + 2;
        call(
            "rename-parameter",
            "textDocument/rename",
            json!({"textDocument":effects,"position":{"line":1,"character":col},"newName":"PAYLOAD"}),
            Populated("/changes"),
        )?;
        call(
            "quick-fix",
            "textDocument/codeAction",
            json!({"textDocument":terrain,"range":{"start":{"line":0,"character":0},"end":{"line":1,"character":0}},"context":{"diagnostics":[]}}),
            Equals("/0/kind", json!("quickfix")),
        )?;
        call(
            "document-symbols",
            "textDocument/documentSymbol",
            document.clone(),
            Populated(""),
        )?;
        call(
            "format",
            "textDocument/formatting",
            json!({"textDocument":doc,"options":{"tabSize":4,"insertSpaces":true}}),
            Populated(""),
        )?;
        let tokens = call(
            "tokens-full",
            "textDocument/semanticTokens/full",
            document.clone(),
            Populated("/data"),
        )?;
        call(
            "tokens-delta-unchanged",
            "textDocument/semanticTokens/full/delta",
            json!({"textDocument":doc,"previousResultId":tokens["resultId"]}),
            Equals("/edits", json!([])),
        )?;
        call(
            "tokens-range",
            "textDocument/semanticTokens/range",
            json!({"textDocument":doc,"range":range}),
            Populated("/data"),
        )?;
        call(
            "workspace-symbols",
            "workspace/symbol",
            json!({"query":"perf_effect"}),
            Populated(""),
        )?;
        call(
            "workspace-symbols-miss",
            "workspace/symbol",
            json!({"query":"zzzzzz_performance_missing_zzz"}),
            Equals("", json!([])),
        )?;
        call(
            "workspace-files",
            "pdc/workspaceFiles",
            Value::Null,
            Populated("/files"),
        )?;
        call(
            "workspace-summary",
            "pdc/workspaceSummary",
            Value::Null,
            Populated("/roots"),
        )?;
        call(
            "analyzer-info",
            "pdc/analyzerInfo",
            Value::Null,
            Equals("/gameId", json!("eu4")),
        )?;
        call(
            "classify",
            "pdc/classifyPaths",
            json!({"paths":[SCRIPT_PATH,TERRAIN_PATH,"outside.unknown"]}),
            Equals("", json!([SCRIPT_PATH, TERRAIN_PATH])),
        )?;
        call(
            "workspace-diagnostics",
            "pdc/workspaceDiagnostics",
            json!({"limit":8}),
            Populated("/items"),
        )?;
        call(
            "buffer-diagnostics",
            "pdc/textDiagnostics",
            json!({"files":[{"path":TERRAIN_PATH,"text":TERRAIN}]}),
            Populated("/0/diagnostics"),
        )?;
        call(
            "rules",
            "pdc/ruleSearch",
            json!({"key":"add_prestige","context":"effect","limit":4}),
            Populated("/rules"),
        )?;
        call(
            "localisation",
            "pdc/localisationSearch",
            json!({"key":"perf_title"}),
            Populated("/hits"),
        )?;
        call(
            "symbols",
            "pdc/symbolSearch",
            json!({"query":"perf_effect"}),
            Populated("/symbols"),
        )?;
        call(
            "symbol-uses",
            "pdc/symbolReferences",
            json!({"name":"perf_effect"}),
            Populated("/references"),
        )?;
        call(
            "mission-preview",
            "pdc/missionPreview",
            json!({"path":"missions/perf.txt","text":MISSION}),
            Equals("/nodes/0/id", json!("perf_mission")),
        )?;
        call(
            "event-preview",
            "pdc/eventPreview",
            json!({"textDocument":doc,"id":"perf.1"}),
            Equals("/card/event/id", json!("perf.1")),
        )?;
        call(
            "hover-card",
            "pdc/hoverCard",
            json!({"textDocument":f.document(GFX_PATH)?,"position":{"line":0,"character":GFX.find("event_pictures").unwrap()},"textureOnly":true}),
            Equals("/card/kind", json!("texture")),
        )?;
        call(
            "search-context",
            "pdc/editorSearchContext",
            Value::Null,
            Populated("/roots"),
        )?;
        for (tab, query) in [
            ("definitions", "perf_effect"),
            ("localisation", "perf_title"),
            ("text", "perf_effect"),
            ("rules", "add_prestige"),
        ] {
            let found = call(
                &format!("search-{tab}"),
                "pdc/editorSearch",
                json!({"tab":tab,"query":query,"searchKeys":true}),
                Populated("/groups"),
            )?;
            let first = &found["groups"][0]["versions"][0];
            if tab == "definitions" {
                call(
                    "search-references",
                    "pdc/editorSearchReferences",
                    json!({"id":first["id"],"name":first["name"],"kind":first["kind"],"revision":found["revision"]}),
                    Populated("/groups"),
                )?;
            } else if tab == "localisation" {
                call(
                    "search-detail",
                    "pdc/editorSearchDetail",
                    json!({"id":first["id"],"revision":found["revision"]}),
                    Equals("/text", json!("Performance 中文")),
                )?;
            }
        }
        let path = f.workspace.join("localisation/perf_l_english.yml");
        let encoded = call(
            "transcode-encode",
            "pdc/transcodeEncode",
            json!({"path":path,"bytes":"e4b8ad"}),
            Populated("/bytes"),
        )?;
        call(
            "transcode-decode",
            "pdc/transcodeDecode",
            json!({"path":path,"bytes":encoded["bytes"]}),
            Equals("/bytes", json!("e4b8ad")),
        )?;
        Ok(())
    }
}
