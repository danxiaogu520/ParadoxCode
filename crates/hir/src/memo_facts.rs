//! Exact fact receipts for reuse across immutable workspace views.
use rules::ir::{Symbol, SymbolFacts, TypeId};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Read {
    Complete,
    Member(TypeId, String),
    Subtype(TypeId, Symbol, String),
    Template(TypeId, String),
    NoArguments(TypeId, String),
    Asset(String, String),
}
#[derive(Clone, Debug)]
enum Answer {
    Bool(bool),
    Optional(Option<bool>),
    Template(Option<Arc<rules::template::Template>>),
}

#[derive(Default, Debug)]
pub(crate) struct Receipts(BTreeMap<Read, Answer>);

impl Receipts {
    fn matches<E>(
        &self,
        facts: &dyn SymbolFacts,
        checkpoint: &mut dyn FnMut() -> Result<(), E>,
    ) -> Result<bool, E> {
        for (read, answer) in &self.0 {
            checkpoint()?;
            let equal = match (read, answer) {
                (Read::Complete, Answer::Bool(expected)) => facts.facts_complete() == *expected,
                (Read::Member(ty, name), Answer::Bool(expected)) => {
                    facts.type_member(*ty, name) == *expected
                }
                (Read::Subtype(ty, subtype, name), Answer::Bool(expected)) => {
                    facts.type_subtype_member(*ty, *subtype, name) == *expected
                }
                (Read::Template(ty, name), Answer::Template(expected)) => {
                    facts.template(*ty, name) == *expected
                }
                (Read::Asset(category, name), Answer::Optional(expected)) => {
                    facts.asset_member(category, name) == *expected
                }
                (Read::NoArguments(ty, name), Answer::Optional(expected)) => {
                    let mut interrupted = None;
                    let actual =
                        facts.template_accepts_no_arguments(
                            *ty,
                            name,
                            &mut || match checkpoint() {
                                Ok(()) => false,
                                Err(error) => {
                                    interrupted = Some(error);
                                    true
                                }
                            },
                        );
                    if let Some(error) = interrupted {
                        return Err(error);
                    }
                    actual == *expected
                }
                _ => unreachable!("receipt kinds agree with their answers"),
            };
            if !equal {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn validate_and_replay<E>(
        &self,
        facts: &dyn SymbolFacts,
        checkpoint: &mut dyn FnMut() -> Result<(), E>,
    ) -> Result<bool, E> {
        if let Some(probe) = facts.untracked_view()
            && !self.matches(probe, checkpoint)?
        {
            return Ok(false);
        }
        self.matches(facts, checkpoint)
    }

    pub(crate) fn bytes(&self) -> usize {
        self.0
            .iter()
            .map(|(read, answer)| {
                let names = match read {
                    Read::Complete => 0,
                    Read::Member(_, name)
                    | Read::Subtype(_, _, name)
                    | Read::Template(_, name)
                    | Read::NoArguments(_, name) => name.len(),
                    Read::Asset(category, name) => category.len() + name.len(),
                };
                let retained = match answer {
                    Answer::Template(Some(template)) => {
                        template.source.len().saturating_mul(8).saturating_add(
                            template
                                .program
                                .blocks
                                .iter()
                                .map(|b| b.len())
                                .sum::<usize>()
                                .saturating_mul(512),
                        )
                    }
                    _ => 0,
                };
                192usize.saturating_add(names).saturating_add(retained)
            })
            .sum()
    }
}

pub(crate) struct RecordingFacts<'a> {
    facts: &'a dyn SymbolFacts,
    reads: RefCell<Receipts>,
    local_memo: Arc<rules::template::TemplateMemo>,
}
impl<'a> RecordingFacts<'a> {
    pub(crate) fn new(facts: &'a dyn SymbolFacts) -> Self {
        Self {
            facts,
            reads: RefCell::new(Receipts::default()),
            local_memo: Arc::new(Default::default()),
        }
    }
    pub(crate) fn finish(self) -> Receipts {
        self.reads.into_inner()
    }
    fn record(&self, read: Read, answer: Answer) {
        self.reads.borrow_mut().0.insert(read, answer);
    }
}
impl SymbolFacts for RecordingFacts<'_> {
    fn untracked_view(&self) -> Option<&dyn SymbolFacts> {
        Some(self.facts.untracked_view().unwrap_or(self.facts))
    }
    fn shared_template_memo(&self) -> Option<Arc<rules::template::TemplateMemo>> {
        self.facts.shared_template_memo()
    }
    fn template_memo(&self) -> Option<Arc<rules::template::TemplateMemo>> {
        // This private memo starts empty and never leaves the recording view.
        // Every hit therefore has its cold reads in this recorder's receipt.
        Some(self.local_memo.clone())
    }
    // Validated shared hits replay their receipts through this recorder.
    // These methods record
    // every observable answer, including negative lookups and absent templates.
    fn facts_complete(&self) -> bool {
        let value = self.facts.facts_complete();
        self.record(Read::Complete, Answer::Bool(value));
        value
    }
    fn type_member(&self, ty: TypeId, name: &str) -> bool {
        let value = self.facts.type_member(ty, name);
        self.record(Read::Member(ty, name.into()), Answer::Bool(value));
        value
    }
    fn type_subtype_member(&self, ty: TypeId, subtype: Symbol, name: &str) -> bool {
        let value = self.facts.type_subtype_member(ty, subtype, name);
        self.record(Read::Subtype(ty, subtype, name.into()), Answer::Bool(value));
        value
    }
    fn template(&self, ty: TypeId, name: &str) -> Option<Arc<rules::template::Template>> {
        let value = self.facts.template(ty, name);
        self.record(
            Read::Template(ty, name.into()),
            Answer::Template(value.clone()),
        );
        value
    }
    fn asset_member(&self, category: &str, name: &str) -> Option<bool> {
        let value = self.facts.asset_member(category, name);
        self.record(
            Read::Asset(category.into(), name.into()),
            Answer::Optional(value),
        );
        value
    }
    fn template_accepts_no_arguments(
        &self,
        ty: TypeId,
        name: &str,
        checkpoint: &mut dyn FnMut() -> bool,
    ) -> Option<bool> {
        let value = self
            .facts
            .template_accepts_no_arguments(ty, name, checkpoint);
        self.record(Read::NoArguments(ty, name.into()), Answer::Optional(value));
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Facts {
        member: Cell<bool>,
        complete: Cell<bool>,
    }
    impl SymbolFacts for Facts {
        fn facts_complete(&self) -> bool {
            self.complete.get()
        }
        fn type_member(&self, _: TypeId, _: &str) -> bool {
            self.member.get()
        }
    }
    struct Tracked<'a> {
        facts: &'a Facts,
        reads: RefCell<Vec<String>>,
    }
    impl SymbolFacts for Tracked<'_> {
        fn untracked_view(&self) -> Option<&dyn SymbolFacts> {
            Some(self.facts)
        }
        fn facts_complete(&self) -> bool {
            self.facts.facts_complete()
        }
        fn type_member(&self, ty: TypeId, name: &str) -> bool {
            self.reads.borrow_mut().push(name.into());
            self.facts.type_member(ty, name)
        }
    }

    #[test]
    fn receipts_replay_negative_reads_and_do_not_publish_failed_probes() {
        let ir = game::eu4::first_party_ir().unwrap();
        let ty = ir.type_by_name("country_tag").unwrap();
        let facts = Facts {
            member: Cell::new(false),
            complete: Cell::new(true),
        };
        let tracked = Tracked {
            facts: &facts,
            reads: Default::default(),
        };
        let recording = RecordingFacts::new(&tracked);
        assert!(!recording.type_member(ty, "missing"));
        assert!(!recording.type_member(ty, "missing"));
        assert!(recording.facts_complete());
        let private = recording.template_memo().unwrap();
        private.insert("private".into(), Arc::new(true), 1);
        assert!(
            recording
                .template_memo()
                .unwrap()
                .get::<bool>("private")
                .is_some()
        );
        assert!(
            RecordingFacts::new(&tracked)
                .template_memo()
                .unwrap()
                .get::<bool>("private")
                .is_none()
        );
        assert!(recording.shared_template_memo().is_none());
        let receipts = recording.finish();
        tracked.reads.borrow_mut().clear();
        assert!(
            receipts
                .validate_and_replay::<()>(&tracked, &mut || Ok(()))
                .unwrap()
        );
        assert_eq!(&*tracked.reads.borrow(), &["missing"]);
        facts.member.set(true);
        tracked.reads.borrow_mut().clear();
        assert!(
            !receipts
                .validate_and_replay::<()>(&tracked, &mut || Ok(()))
                .unwrap()
        );
        assert!(tracked.reads.borrow().is_empty());
        let speculative = RecordingFacts::new(&tracked);
        assert!(
            !receipts
                .validate_and_replay(&speculative, &mut || Ok::<(), ()>(()))
                .unwrap()
        );
        assert!(speculative.finish().0.is_empty());
        assert!(tracked.reads.borrow().is_empty());
        facts.member.set(false);
        facts.complete.set(false);
        assert!(
            !receipts
                .validate_and_replay::<()>(&tracked, &mut || Ok(()))
                .unwrap()
        );
        facts.complete.set(true);
        assert!(
            receipts
                .validate_and_replay(&tracked, &mut || Err(()))
                .is_err()
        );
        assert!(tracked.reads.borrow().is_empty());
    }
}
