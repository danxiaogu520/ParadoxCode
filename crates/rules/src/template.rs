//! Source-ranged Template programs shared by lowering and workspace facts.

use std::any::Any;
use std::sync::Arc;
use std::sync::Mutex;
use text::TextRange;

/// One source token retained by a dynamic-definition template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplateToken {
    /// Exact definition-side token range, including quotes when present.
    pub range: TextRange,
    /// Whether the source token was quoted.
    pub quoted: bool,
    /// Literal and parameter fragments in source order, excluding surrounding quotes.
    pub fragments: Vec<TemplateFragment>,
}

/// One literal or parameter fragment within a dynamic template token.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum TemplateFragment {
    /// Definition-side text copied without interpretation.
    Literal(String),
    /// One owner-local parameter slot.
    Parameter {
        /// Parameter spelling without delimiters.
        name: Arc<str>,
        /// Exact definition-side range of the delimited occurrence.
        range: TextRange,
    },
}

/// The value attached to a property in a dynamic template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateValue {
    /// One scalar token.
    Scalar(TemplateToken),
    /// One ordered script block.
    Block {
        /// Exact definition-side block range.
        range: TextRange,
        /// Properties, bare values, and conditional blocks in source order.
        items: Vec<TemplateItem>,
    },
}

/// One property retained in a dynamic template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplateProperty {
    /// Token supplying the property key.
    pub key: TemplateToken,
    /// Full definition-side property range.
    pub range: TextRange,
    /// Operator spelling recovered by the parser.
    pub operator: Option<Arc<str>>,
    /// Scalar or block value.
    pub value: TemplateValue,
}

/// One conditional block retained in a dynamic template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplateConditional {
    /// Parameter spelling without `!`.
    pub name: Arc<str>,
    /// Whether the body is active when the parameter is absent.
    pub negated: bool,
    /// Full definition-side conditional range.
    pub range: TextRange,
    /// Ordered body items.
    pub items: Vec<TemplateItem>,
}

/// One ordered item in a dynamic template container.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateItem {
    /// Lossless syntax whose structure cannot yet be interpreted before binding.
    Recover(TemplateToken),
    /// A key/operator/value property.
    Property(TemplateProperty),
    /// A standalone scalar in a mixed block.
    BareValue(TemplateToken),
    /// A supplied/absent parameter conditional.
    Conditional(TemplateConditional),
}

/// Reusable, source-ranged body of one scripted effect or trigger definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Template {
    /// Lossless body, including trivia and recovery syntax. Empty for historical tree-only caches.
    pub source: Arc<str>,
    /// Dynamic symbol kind, such as `scripted_effect`.
    pub kind: Arc<str>,
    /// Definition name as written in source.
    pub name: String,
    /// Full owning definition range.
    pub definition_range: TextRange,
    /// Exact body block range.
    pub body_range: TextRange,
    /// Ordered body items.
    pub items: Arc<[TemplateItem]>,
    /// Immutable flat program shared by all invocations of this definition.
    pub program: Arc<TemplateProgram>,
}

/// Byte-bounded memo owned by an immutable workspace-facts view.
/// Values belong to the semantic layer; the rules layer stores no editor types.
pub struct TemplateMemo {
    state: Mutex<TemplateMemoState>,
    max_bytes: usize,
}
#[derive(Default)]
struct TemplateMemoState {
    entries: std::collections::HashMap<String, Arc<dyn Any + Send + Sync>>,
    bytes: usize,
}
impl Default for TemplateMemo {
    fn default() -> Self {
        Self {
            state: Mutex::new(Default::default()),
            max_bytes: 16 * 1024 * 1024,
        }
    }
}
impl TemplateMemo {
    /// Reads a typed, proven reusable result from this immutable view.
    pub fn get<T: Any + Send + Sync>(&self, key: &str) -> Option<Arc<T>> {
        self.state
            .lock()
            .ok()?
            .entries
            .get(key)?
            .clone()
            .downcast()
            .ok()
    }
    /// Stores an immutable result, evicting the view memo when its byte budget is exhausted.
    pub fn insert<T: Any + Send + Sync>(&self, key: String, value: Arc<T>, bytes: usize) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let cost = bytes.saturating_add(key.len());
        if cost > self.max_bytes || state.entries.contains_key(&key) {
            return;
        }
        if state.bytes.saturating_add(cost) > self.max_bytes {
            state.entries.clear();
            state.bytes = 0;
        }
        state.bytes = state.bytes.saturating_add(cost);
        state.entries.insert(key, value);
    }
}

/// An index into a definition's immutable block arena.
pub type TemplateBlockId = usize;

/// A source-ranged instruction; child blocks are arena links rather than copied trees.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateInstruction {
    /// Retains an unknown syntax boundary while other instructions remain queryable.
    Recover(TemplateToken),
    /// Select a field using the rendered key, then check its value/container.
    Dispatch(TemplateDispatch),
    /// A scalar item or a script insertion, according to the consuming schema.
    Consume(TemplateToken),
    /// Presence-conditioned block.
    When {
        name: Arc<str>,
        negated: bool,
        block: TemplateBlockId,
    },
}

/// Source-ranged field instruction with its value kept coupled to the key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplateDispatch {
    /// Key expression.
    pub key: TemplateToken,
    /// Full source field range.
    pub range: TextRange,
    /// Source operator.
    pub operator: Option<Arc<str>>,
    /// Value expression or child container.
    pub value: TemplateOperand,
}

/// The operand of a dispatch instruction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateOperand {
    /// A source-ranged scalar expression.
    Scalar(TemplateToken),
    /// A shared child container.
    Block(TemplateBlockId),
}

/// Query-independent syntax/read program. Rules and scopes are selected at consumption time.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TemplateProgram {
    /// Ordered containers. Container zero is the entry body.
    pub blocks: Vec<Arc<[TemplateInstruction]>>,
    /// Parameter spellings read anywhere in the definition, including existence tests.
    pub parameters: std::collections::BTreeSet<Arc<str>>,
}

impl TemplateProgram {
    /// Text bindings required independently of presence guards. Forwarding is still a read.
    pub fn unconditional_reads(&self) -> std::collections::BTreeSet<String> {
        let mut result = std::collections::BTreeSet::new();
        let mut pending = vec![0];
        while let Some(block) = pending.pop() {
            for instruction in self.blocks[block].iter() {
                let mut read = |token: &TemplateToken| {
                    for part in &token.fragments {
                        if let TemplateFragment::Parameter { name, .. } = part {
                            result.insert(name.to_ascii_lowercase());
                        }
                    }
                };
                match instruction {
                    TemplateInstruction::When { .. } => {}
                    TemplateInstruction::Recover(token) => read(token),
                    TemplateInstruction::Consume(token) => read(token),
                    TemplateInstruction::Dispatch(property) => {
                        read(&property.key);
                        match &property.value {
                            TemplateOperand::Scalar(token) => read(token),
                            TemplateOperand::Block(block) => pending.push(*block),
                        }
                    }
                }
            }
        }
        result
    }
    /// Compiles the source tree once without recursion or expanded call copies.
    pub fn compile(items: &[TemplateItem]) -> Self {
        let mut program = Self {
            blocks: vec![Arc::from([])],
            parameters: Default::default(),
        };
        let mut pending = vec![(0, items)];
        while let Some((id, items)) = pending.pop() {
            let mut instructions = Vec::with_capacity(items.len());
            for item in items {
                let mut token_reads = |token: &TemplateToken| {
                    for fragment in &token.fragments {
                        if let TemplateFragment::Parameter { name, .. } = fragment {
                            program.parameters.insert(name.clone());
                        }
                    }
                };
                match item {
                    TemplateItem::Recover(token) => {
                        token_reads(token);
                        instructions.push(TemplateInstruction::Recover(token.clone()));
                    }
                    TemplateItem::BareValue(token) => {
                        token_reads(token);
                        instructions.push(TemplateInstruction::Consume(token.clone()));
                    }
                    TemplateItem::Conditional(condition) => {
                        program.parameters.insert(condition.name.clone());
                        let block = program.blocks.len();
                        program.blocks.push(Arc::from([]));
                        pending.push((block, condition.items.as_slice()));
                        instructions.push(TemplateInstruction::When {
                            name: condition.name.clone(),
                            negated: condition.negated,
                            block,
                        });
                    }
                    TemplateItem::Property(property) => {
                        token_reads(&property.key);
                        let value = match &property.value {
                            TemplateValue::Scalar(token) => {
                                token_reads(token);
                                TemplateOperand::Scalar(token.clone())
                            }
                            TemplateValue::Block { items, .. } => {
                                let block = program.blocks.len();
                                program.blocks.push(Arc::from([]));
                                pending.push((block, items.as_slice()));
                                TemplateOperand::Block(block)
                            }
                        };
                        instructions.push(TemplateInstruction::Dispatch(TemplateDispatch {
                            key: property.key.clone(),
                            range: property.range,
                            operator: property.operator.clone(),
                            value,
                        }));
                    }
                }
            }
            program.blocks[id] = Arc::from(instructions);
        }
        program
    }
}
