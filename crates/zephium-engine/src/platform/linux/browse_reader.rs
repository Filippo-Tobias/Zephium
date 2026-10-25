//! On-demand Ask reads in a named WebKitGTK isolated world.
use crate::page_reader::{PageActFailure, PageReadFailure, PageReadout};
use crate::platform::page_recipe::PageRecipe;
use gtk::{gio, glib, prelude::*};
use javascriptcore::ValueExt;
use std::{
    cell::Cell,
    rc::Rc,
    sync::OnceLock,
    time::{Duration, Instant},
};
use webkit2gtk::{UserContentManagerExt, WebViewExt};
use zephium_core::ids::ItemId;

type ReadDone = Box<dyn FnOnce(Result<PageReadout, PageReadFailure>) + Send>;
type RecipeDone = Box<dyn FnOnce(Option<String>) + Send>;
type ActDone = Box<dyn FnOnce(Result<(), PageActFailure>) + Send>;

fn program() -> &'static str {
    static PROGRAM: OnceLock<String> = OnceLock::new();
    PROGRAM.get_or_init(|| {
        format!(
            "{}\n{}\n{}\n",
            zephium_agentic::SEMANTIC_RUNTIME_PROGRAM.installation_source(),
            include_str!("../../host/picker_kit.js"),
            include_str!("../page_reader_recipes.js")
        )
    })
}
fn monotonic_ms() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min(u64::MAX as u128) as u64
        + 1
}

pub(crate) struct BrowseReader {
    view: glib::WeakRef<webkit2gtk::WebView>,
    tab: Rc<Cell<ItemId>>,
    enabled: Rc<Cell<bool>>,
    committed: Rc<Cell<Option<u64>>>,
    committed_now: Rc<dyn Fn() -> Option<u64>>,
    settled: Rc<dyn Fn() -> bool>,
    context: zephium_agentic::ContextJoin,
    world: String,
    sequence: Rc<Cell<u64>>,
    owner: bool,
    manager: webkit2gtk::UserContentManager,
    world_pin: webkit2gtk::UserScript,
}
impl BrowseReader {
    pub(crate) fn new(
        view: &webkit2gtk::WebView,
        tab: Rc<Cell<ItemId>>,
        enabled: bool,
        committed_now: Rc<dyn Fn() -> Option<u64>>,
        settled: Rc<dyn Fn() -> bool>,
    ) -> Option<Self> {
        let manager = view.user_content_manager()?;
        let world = format!("zephium.ask.{}", ItemId::generate());
        // A retained no-op script pins the named world across separate async
        // invocations. The reader runtime itself remains lazily installed.
        let world_pin = webkit2gtk::UserScript::for_world(
            "void 0;",
            webkit2gtk::UserContentInjectedFrames::TopFrame,
            webkit2gtk::UserScriptInjectionTime::Start,
            &world,
            &[],
            &[],
        );
        manager.add_script(&world_pin);
        let enabled = Rc::new(Cell::new(enabled));
        let input_tab = tab.clone();
        let input_enabled = enabled.clone();
        view.connect_button_press_event(move |_, _| {
            if input_enabled.get() {
                crate::host::page_reader::report_human_input(input_tab.get());
            }
            glib::Propagation::Proceed
        });
        let input_tab = tab.clone();
        let input_enabled = enabled.clone();
        view.connect_key_press_event(move |_, _| {
            if input_enabled.get() {
                crate::host::page_reader::report_human_input(input_tab.get());
            }
            glib::Propagation::Proceed
        });
        Some(Self {
            view: view.downgrade(),
            tab,
            enabled,
            committed: Rc::new(Cell::new(None)),
            committed_now,
            settled,
            context: crate::page_reader::read::reader_context()?,
            world,
            sequence: Rc::new(Cell::new(0)),
            owner: true,
            manager,
            world_pin,
        })
    }
    pub(crate) fn attempt(&self, _: bool, _: &str) {
        self.begin_load();
    }
    pub(crate) fn begin_load(&self) {
        self.committed.set(None);
    }
    pub(crate) fn back_forward(&self, _: bool) {}
    pub(crate) fn commit(&self, epoch: u64, _: &str) {
        self.committed.set(Some(epoch));
    }
    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.enabled.set(enabled);
        if !enabled {
            self.clear_world();
        }
    }
    fn clear_world(&self) {
        if let Some(view) = self.view.upgrade() {
            view.call_async_javascript_function(
                "const api = globalThis.__zephiumPageAssistV1; if (api) { api.recipe('Q1:pickcancel'); api.recipe('Q1:clear'); }",
                None, Some(&self.world), None, None::<&gio::Cancellable>, |_| {});
        }
    }

    fn invoke(
        &self,
        epoch: u64,
        request: &str,
        recipe: bool,
        done: impl FnOnce(Result<String, PageReadFailure>) + 'static,
    ) {
        if !self.enabled.get() {
            done(Err(PageReadFailure::Unavailable));
            return;
        }
        if self.committed.get() != Some(epoch) || (self.committed_now)() != Some(epoch) {
            done(Err(PageReadFailure::PageChanged));
            return;
        }
        if !(self.settled)() {
            done(Err(PageReadFailure::NotReady));
            return;
        }
        let Some(view) = self.view.upgrade() else {
            done(Err(PageReadFailure::Unavailable));
            return;
        };
        let request = match serde_json::to_string(request) {
            Ok(request) => request,
            Err(_) => {
                done(Err(PageReadFailure::Unavailable));
                return;
            }
        };
        // Only the fixed program and Rust's closed serialized invocation enter
        // the private world. Page strings are never executable source.
        let api = if recipe {
            "__zephiumPageAssistV1.recipe"
        } else {
            "__zephiumSemanticRuntimeV1.invoke"
        };
        let body = format!("{}\nreturn await globalThis.{api}({request});", program());
        let done = Rc::new(std::cell::RefCell::new(Some(done)));
        let cancelled = gio::Cancellable::new();
        let timeout_done = done.clone();
        let cancellation = cancelled.clone();
        let timer = glib::timeout_add_local_once(Duration::from_secs(12), move || {
            cancellation.cancel();
            if let Some(done) = timeout_done.borrow_mut().take() {
                done(Err(PageReadFailure::NotReady));
            }
        });
        let enabled = self.enabled.clone();
        let committed = self.committed_now.clone();
        view.call_async_javascript_function(
            &body,
            None,
            Some(&self.world),
            None,
            Some(&cancelled),
            move |result| {
                // A fired timeout already settled the request and removed its
                // own source. Otherwise this completion owns timer removal.
                let callback = done.borrow_mut().take();
                if let Some(done) = callback {
                    timer.remove();
                    if !enabled.get() {
                        done(Err(PageReadFailure::Unavailable));
                    } else if committed() != Some(epoch) {
                        done(Err(PageReadFailure::PageChanged));
                    } else {
                        done(
                            result
                                .ok()
                                .filter(|v| v.is_string())
                                .map(|v| v.to_str().to_string())
                                .filter(|s| s.len() <= 2 * 1024 * 1024)
                                .ok_or(PageReadFailure::Unavailable),
                        );
                    }
                }
            },
        );
    }
    pub(crate) fn read(&self, epoch: u64, url: String, done: ReadDone) {
        let sequence = self.sequence.get().saturating_add(1);
        self.sequence.set(sequence);
        let Some((invocation, pending)) =
            crate::page_reader::read::invocation(self.context, &url, sequence)
        else {
            done(Err(PageReadFailure::Unavailable));
            return;
        };
        let request = invocation.as_str().to_owned();
        let tab = self.tab.get();
        let view = self.view.clone();
        self.invoke(epoch, &request, false, move |result| {
            let result = result
                .and_then(|text| {
                    invocation
                        .decode_result(text.as_bytes())
                        .map_err(|_| PageReadFailure::Unavailable)
                })
                .and_then(|snapshot| {
                    crate::page_reader::read::passages(pending, snapshot, monotonic_ms(), epoch)
                })
                .map(|(passages, kept)| PageReadout {
                    epoch,
                    read: crate::page_reader::publish_read(tab, kept),
                    passages,
                    title: view
                        .upgrade()
                        .and_then(|view| view.title())
                        .map(|s| s.to_string()),
                });
            done(result);
        });
    }
    pub(crate) fn recipe(&self, epoch: u64, recipe: PageRecipe, done: RecipeDone) {
        let request = recipe.as_str().to_owned();
        self.invoke(epoch, &request, true, move |result| {
            done(result.ok().and_then(|text| recipe.decode(text.as_bytes())));
        });
    }
    fn handle(&self) -> Self {
        Self {
            view: self.view.clone(),
            tab: self.tab.clone(),
            enabled: self.enabled.clone(),
            committed: self.committed.clone(),
            committed_now: self.committed_now.clone(),
            settled: self.settled.clone(),
            context: self.context,
            world: self.world.clone(),
            sequence: self.sequence.clone(),
            owner: false,
            manager: self.manager.clone(),
            world_pin: self.world_pin.clone(),
        }
    }
    pub(crate) fn act(
        &self,
        epoch: u64,
        read: u64,
        url: String,
        proposal: zephium_agentic::SemanticActionProposal,
        done: ActDone,
    ) {
        let sequence = self.sequence.get().saturating_add(1);
        self.sequence.set(sequence);
        let Some((observe, _)) = crate::page_reader::read::invocation(self.context, &url, sequence)
        else {
            done(Err(PageActFailure::Failed));
            return;
        };
        let request = observe.as_str().to_owned();
        let reader = self.handle();
        self.invoke(epoch, &request, false, move |result| {
            let prepared = result
                .map_err(|_| PageActFailure::PageChanged)
                .and_then(|text| {
                    observe
                        .decode_result(text.as_bytes())
                        .map_err(|_| PageActFailure::Failed)
                })
                .and_then(|snapshot| {
                    let stored = crate::page_reader::planned_read(reader.tab.get(), read)
                        .filter(|stored| stored.epoch() == epoch)
                        .ok_or(PageActFailure::PageChanged)?;
                    crate::page_reader::read::person_action(
                        &stored,
                        &snapshot,
                        proposal,
                        sequence,
                        monotonic_ms(),
                    )
                });
            let (pending, native) = match prepared {
                Ok(value) => value,
                Err(error) => {
                    done(Err(error));
                    return;
                }
            };
            #[cfg(test)]
            assert_eq!(native.checkpoint_invocation(), observe.invocation());
            let invocation =
                match zephium_agentic::encode_semantic_action_runtime_invocation(&native) {
                    Ok(value) => value,
                    Err(_) => {
                        done(Err(PageActFailure::Failed));
                        return;
                    }
                };
            let request = invocation.as_str().to_owned();
            let started = Instant::now();
            reader.invoke(epoch, &request, false, move |result| {
                let result = result.ok().and_then(|text| {
                    invocation
                        .decode_result(text.as_bytes())
                        .map_err(|error| {
                            crate::page_reader::note(format_args!(
                                "linux page action decode: {error:?}"
                            ));
                        })
                        .ok()
                });
                let settlement = match result {
                    Some(evidence) => {
                        let completed =
                            zephium_agentic::SemanticActionExecutionInstant::from_millis(
                                native.requested_at().millis().saturating_add(
                                    started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                                ),
                            );
                        native.complete(
                            evidence.backend(),
                            evidence.readiness(),
                            evidence.viewport(),
                            evidence.geometry(),
                            completed,
                            completed,
                        )
                    }
                    None => {
                        let deadline = native.deadline();
                        native.fail(
                            zephium_agentic::SemanticActionNativeFailure::Transport,
                            deadline,
                        )
                    }
                };
                done(
                    pending
                        .settle(settlement)
                        .map_err(crate::page_reader::read::act_failure),
                );
            });
        });
    }
    pub(crate) fn poll(&self) {}
    pub(crate) fn pump(&self) {}
    pub(crate) fn timeout(&self, _: u64) {}
}
impl Drop for BrowseReader {
    fn drop(&mut self) {
        if self.owner {
            self.clear_world();
            self.manager.remove_script(&self.world_pin);
            self.enabled.set(false);
            crate::page_reader::forget(self.tab.get());
        }
    }
}
