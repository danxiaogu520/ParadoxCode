//! Coalesces identical immutable body queries from simultaneous editor features.
use super::*;
use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
struct Flight {
    state: Mutex<State>,
    changed: Condvar,
}
#[derive(Default)]
struct State {
    building: bool,
    value: Option<Arc<BodyAnalysis>>,
}
pub(super) enum Claim {
    Ready(Arc<BodyAnalysis>),
    Build(Builder),
}
pub(super) struct Builder {
    flight: Arc<Flight>,
}
impl Builder {
    pub(super) fn publish(&self, body: Arc<BodyAnalysis>) {
        if body.coverage.is_complete() {
            self.flight.state.lock().expect("body query").value = Some(body);
            self.flight.changed.notify_all();
        }
    }
}
impl Drop for Builder {
    fn drop(&mut self) {
        self.flight.state.lock().expect("body query").building = false;
        self.flight.changed.notify_all();
    }
}
pub(super) fn acquire(
    memo: &rules::template::TemplateMemo,
    key: &str,
    cancellation: &CancellationToken,
) -> Result<Claim, Cancelled> {
    let key = format!("{key}:flight");
    let flight = memo.get::<Flight>(&key).unwrap_or_else(|| {
        let flight = Arc::new(Flight::default());
        memo.insert(key.clone(), flight.clone(), 512);
        memo.get(&key).unwrap_or(flight)
    });
    let mut state = flight.state.lock().expect("body query");
    loop {
        cancellation.checkpoint()?;
        if let Some(body) = &state.value {
            return Ok(Claim::Ready(body.clone()));
        }
        if !state.building {
            state.building = true;
            drop(state);
            return Ok(Claim::Build(Builder { flight }));
        }
        state = flight
            .changed
            .wait_timeout(state, std::time::Duration::from_millis(5))
            .expect("body query")
            .0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn simultaneous_features_share_one_body_and_cancelled_waiters_exit() {
        let ir = game::eu4::first_party_ir().unwrap();
        let schema =
            hir::template::template_body(&ir, ir.type_by_name("scripted_effect").unwrap()).unwrap();
        let body = Arc::new(
            hir::template_instance::instantiate_rendered_no_calls(
                &ir,
                &rules::ir::NoSymbolFacts,
                hir::template_text::RenderedTemplate {
                    text: "add_stability = 1".into(),
                    ..Default::default()
                },
                schema,
                ScopeState::initial(hir::ScopeValue::known_single("country")),
                &[],
                &mut |_| false,
                &mut || Ok::<(), ()>(()),
            )
            .unwrap()
            .unwrap(),
        );
        let memo = rules::template::TemplateMemo::default();
        let barrier = std::sync::Barrier::new(8);
        let builders = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let (body, memo, barrier, builders) = (body.clone(), &memo, &barrier, &builders);
                scope.spawn(move || {
                    barrier.wait();
                    match acquire(memo, "same", &CancellationToken::new()).unwrap() {
                        Claim::Ready(actual) => assert!(Arc::ptr_eq(&actual, &body)),
                        Claim::Build(build) => {
                            builders.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            std::thread::sleep(std::time::Duration::from_millis(10));
                            build.publish(body);
                        }
                    }
                });
            }
        });
        assert_eq!(builders.load(std::sync::atomic::Ordering::Relaxed), 1);
        let Claim::Build(build) = acquire(&memo, "waiting", &CancellationToken::new()).unwrap()
        else {
            panic!("new query");
        };
        let cancellation = CancellationToken::new();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(10));
                cancellation.cancel();
            });
            assert!(acquire(&memo, "waiting", &cancellation).is_err());
        });
        drop(build);
        assert!(matches!(
            acquire(&memo, "waiting", &CancellationToken::new()).unwrap(),
            Claim::Build(_)
        ));
    }
}
