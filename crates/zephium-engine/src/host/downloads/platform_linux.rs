//! WebKitGTK transport and portal-capable GTK file dialogs.
use super::*;
use gtk::{gio, glib, prelude::*};
use webkit2gtk::{DownloadExt, URIRequestExt, URIResponseExt, WebContextExt, WebViewExt};

pub(super) type Native = webkit2gtk::Download;
pub(super) type Delegate = ();
pub(super) type Timer = glib::SourceId;
pub(super) type SavePanel = gtk::FileChooserNative;
pub(super) type DirectoryPanel = gtk::FileChooserNative;
pub(super) type DialogLease = ();
pub(super) struct Source {
    view: webkit2gtk::WebView,
    permit: EventPermit,
    presentation: Arc<AtomicBool>,
    navigation: NavigationEpochTracker,
    activity: NavigationActivity,
}
impl Source {
    pub(super) fn live(&self) -> bool {
        self.permit.active_token().is_some()
            && self.presentation.load(Ordering::Acquire)
            && self.navigation.matches_activity(self.activity)
            && self.window().is_some_and(|window| window.is_visible())
    }
    fn window(&self) -> Option<gtk::Window> {
        self.view.toplevel()?.downcast().ok()
    }
}
struct Binding {
    manager: std::rc::Weak<Downloads>,
    partition: Partition,
    permit: EventPermit,
    presentation: Arc<AtomicBool>,
    navigation: NavigationEpochTracker,
}
const BINDING: &str = "zephium-linux-download-source";
const INSTALLED: &str = "zephium-linux-download-handler";

pub(in crate::host) fn install(
    view: &wry::WebView,
    manager: Option<&Rc<Downloads>>,
    partition: Partition,
    permit: EventPermit,
    presentation: Arc<AtomicBool>,
    navigation: NavigationEpochTracker,
) {
    use wry::WebViewExtUnix;
    let view = view.webview();
    // These private keys are used only with the exact types below. The binding
    // holds no strong view reference; the context handler holds no profile.
    unsafe {
        view.set_data(
            BINDING,
            Binding {
                manager: manager.map_or_else(std::rc::Weak::new, Rc::downgrade),
                partition,
                permit,
                presentation,
                navigation,
            },
        )
    };
    let Some(context) = view.context() else {
        return;
    };
    if unsafe { context.data::<bool>(INSTALLED).is_some() } {
        return;
    }
    context.connect_download_started(|_, download| {
        let source = download.web_view().and_then(|view| {
            let binding = unsafe { view.data::<Binding>(BINDING)?.as_ref() };
            let manager = binding.manager.upgrade()?;
            let source = Source {
                view: view.clone(),
                permit: binding.permit.clone(),
                presentation: binding.presentation.clone(),
                navigation: binding.navigation.clone(),
                activity: binding.navigation.activity_snapshot()?,
            };
            Some((manager, binding.partition, source))
        });
        match source {
            Some((manager, partition, source)) => manager.admit(partition, download, source),
            None => download.cancel(),
        }
    });
    unsafe { context.set_data(INSTALLED, true) };
}

pub(super) fn progress(native: &Native) -> Option<(u64, Option<u64>)> {
    Some((
        native.received_data_length(),
        native.response().map(|r| r.content_length()),
    ))
}
pub(super) fn completion_bytes(native: &Native) -> Result<Option<u64>, DownloadError> {
    // Content-Length may describe compressed transfer bytes; WebKit exposes
    // the bytes actually written separately.
    Ok(Some(native.received_data_length()))
}
pub(super) fn stop_timer(timer: Timer) {
    timer.remove();
}
pub(super) fn cancel_directory(panel: &DirectoryPanel) {
    panel.hide();
}
pub(super) fn open(path: &std::path::Path) -> Result<(), DownloadError> {
    gio::AppInfo::launch_default_for_uri(
        &gio::File::for_path(path).uri(),
        None::<&gio::AppLaunchContext>,
    )
    .map_err(|_| DownloadError::Unavailable)
}
pub(super) fn reveal(path: &std::path::Path) -> Result<(), DownloadError> {
    // File managers implementing the standard D-Bus API select the exact file.
    // Fall back to opening its parent when the desktop has no such service.
    let uri = gio::File::for_path(path).uri();
    let proxy = gio::DBusProxy::for_bus_sync(
        gio::BusType::Session,
        gio::DBusProxyFlags::NONE,
        None,
        "org.freedesktop.FileManager1",
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1",
        None::<&gio::Cancellable>,
    );
    if let Ok(proxy) = proxy {
        if proxy
            .call_sync(
                "ShowItems",
                Some(&(vec![uri.as_str()], "").to_variant()),
                gio::DBusCallFlags::NONE,
                1500,
                None::<&gio::Cancellable>,
            )
            .is_ok()
        {
            return Ok(());
        }
    }
    open(path.parent().ok_or(DownloadError::Destination)?)
}

impl Downloads {
    pub(super) fn resume(&self, _: Partition, _: DownloadId) -> Result<(), DownloadError> {
        // WebKitGTK does not expose a pause/resume API for native downloads.
        Err(DownloadError::Unsupported)
    }
    pub(super) fn ensure_timer(self: &Rc<Self>) {
        let (period, slow) = self.tick_period();
        if self.timer.borrow().is_some() {
            if self.timer_slow.get() == slow {
                return;
            }
            if let Some(timer) = self.timer.borrow_mut().take() {
                stop_timer(timer);
            }
        }
        self.timer_slow.set(slow);
        let weak = Rc::downgrade(self);
        let timer = glib::timeout_add_local(period, move || {
            let Some(manager) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            manager.tick();
            glib::ControlFlow::Continue
        });
        *self.timer.borrow_mut() = Some(timer);
    }
    fn admit(self: &Rc<Self>, partition: Partition, native: &Native, source: Source) {
        if self.stopping.get()
            || self.retired.borrow().contains(&partition.profile())
            || self.active.borrow().len() >= MAX_ACTIVE_DOWNLOADS
            || self.work.get() >= MAX_BACKGROUND_WORK
            || !self.private_cleanup_capacity(partition)
            || !source.live()
        {
            native.cancel();
            return;
        }
        let origin = source
            .view
            .uri()
            .and_then(|uri| url::Url::parse(&uri).ok())
            .and_then(|url| PageOrigin::from_url(&url).ok())
            .or_else(|| {
                native
                    .request()
                    .and_then(|request| request.uri())
                    .and_then(|uri| url::Url::parse(&uri).ok())
                    .and_then(|url| PageOrigin::from_url(&url).ok())
            });
        let Some(origin) = origin else {
            native.cancel();
            return;
        };
        let id = DownloadId::generate();
        let record = DownloadRecord {
            id,
            session: self.session,
            revision: 1,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |t| t.as_secs() as i64),
            filename: "download".into(),
            source: origin.as_str().into(),
            source_is_context: true,
            state: DownloadState::Pending,
            received: 0,
            total: None,
            error: None,
            destination: None,
            staging: None,
            staging_identity: None,
            identity: None,
            writer: None,
            writer_released: false,
        };
        self.active.borrow_mut().insert(
            id,
            Transfer {
                partition,
                record,
                native: native.clone(),
                _delegate: (),
                source: Some(source),
                destination_reply: None,
                destination: None,
                panel: None,
                panel_lease: None,
                on_started: None,
                authorized: false,
                cancelling: false,
                persisting_terminal: false,
                deadline: DecisionDeadline::new(DecisionPhase::Admission, Instant::now()),
            },
        );
        let weak = Rc::downgrade(self);
        native.connect_decide_destination(move |download, suggested| {
            let Some(manager) = weak.upgrade() else {
                download.cancel();
                return true;
            };
            let Some(partition) = manager.active.borrow_mut().get_mut(&id).map(|transfer| {
                transfer.record.filename = safe_filename(suggested);
                transfer.record.total = download.response().map(|r| r.content_length());
                let download = download.clone();
                transfer.destination_reply = Some(DestinationReply::new(move |path| match path {
                    Some(path) => download.set_destination(&gio::File::for_path(path).uri()),
                    None => download.cancel(),
                }));
                transfer.partition
            }) else {
                download.cancel();
                return true;
            };
            let preferences = manager
                .preferences
                .borrow()
                .get(&partition.profile())
                .cloned();
            if let Some(preferences) = preferences {
                manager.choose_destination(id, preferences);
            } else if matches!(partition, Partition::Ephemeral(_)) {
                manager.choose_destination(id, DownloadPreferences::default());
            } else {
                manager.work.set(manager.work.get() + 1);
                let sender = manager.sender.clone();
                if !manager.store.download_call(
                    partition.profile(),
                    DownloadStoreCall::Preferences,
                    Box::new(move |reply| {
                        let _ = sender.send(Message::Preferences(id, reply));
                    }),
                ) {
                    let _ = manager.sender.send(Message::Preferences(
                        id,
                        DownloadStoreReply::Error(DownloadError::Storage),
                    ));
                }
            }
            true
        });
        let failed = Rc::new(Cell::new(None));
        let failure = failed.clone();
        native.connect_failed(move |_, error| {
            let kind = match error.kind::<webkit2gtk::DownloadError>() {
                Some(webkit2gtk::DownloadError::CancelledByUser) => DownloadError::Cancelled,
                Some(webkit2gtk::DownloadError::Destination) => DownloadError::Destination,
                _ => DownloadError::Network,
            };
            failure.set(Some(kind));
        });
        let weak = Rc::downgrade(self);
        native.connect_finished(move |_| {
            if let Some(manager) = weak.upgrade() {
                if let Some(error) = failed.get() {
                    manager.native_failed_with_error(id, error);
                } else {
                    manager.native_finished(id);
                }
            }
        });
        self.ensure_recovery(partition);
        self.ensure_timer();
        (self.notify)(partition.profile());
    }
    pub(super) fn choose_destination(
        self: &Rc<Self>,
        id: DownloadId,
        preferences: DownloadPreferences,
    ) {
        let context = self.active.borrow().get(&id).and_then(|t| {
            let source = t.source.as_ref()?;
            source.live().then(|| {
                (
                    source.window(),
                    t.record.filename.clone(),
                    t.record.source.clone(),
                )
            })
        });
        let Some((Some(window), filename, source)) = context else {
            self.cancel(id, None);
            return;
        };
        let directory = preferences
            .directory
            .clone()
            .map(PathBuf::from)
            .or_else(dirs::download_dir);
        if !preferences.ask_destination {
            if let Some(directory) = directory {
                self.prepare(id, directory.join(filename), preferences.directory_identity);
                return;
            }
        }
        let panel = gtk::FileChooserNative::new(
            Some(&format!("Save download from {source}")),
            Some(&window),
            gtk::FileChooserAction::Save,
            Some("Save"),
            Some("Cancel"),
        );
        panel.set_current_name(&filename);
        if let Some(directory) = directory {
            panel.set_current_folder(directory);
        }
        if let Some(transfer) = self.active.borrow_mut().get_mut(&id) {
            transfer.panel = Some(panel.clone());
            transfer.deadline = DecisionDeadline::new(DecisionPhase::NativePicker, Instant::now());
        }
        let weak = Rc::downgrade(self);
        panel.connect_response(move |panel, response| {
            let path = (response == gtk::ResponseType::Accept)
                .then(|| panel.filename())
                .flatten();
            panel.hide();
            if let Some(manager) = weak.upgrade() {
                if let Some(t) = manager.active.borrow_mut().get_mut(&id) {
                    t.panel.take();
                }
                match path {
                    Some(path) => manager.prepare(id, path, None),
                    None => manager.cancel(id, None),
                }
            }
        });
        panel.show();
    }
    pub(super) fn cancel(&self, id: DownloadId, error: Option<DownloadError>) {
        let native = {
            let mut active = self.active.borrow_mut();
            let Some(t) = active.get_mut(&id) else { return };
            if t.record.state.terminal()
                || t.record.state == DownloadState::Finalizing
                || t.cancelling
            {
                return;
            }
            t.cancelling = true;
            t.record.state = DownloadState::Cancelling;
            t.record.revision += 1;
            t.record.error = error;
            (t.native.clone(), t.panel.take(), t.destination_reply.take())
        };
        if let Some(panel) = native.1 {
            panel.hide();
        }
        // Drop an unanswered destination before cancellation; its completion is
        // also a cancel, so no delayed picker can start this transfer again.
        drop(native.2);
        native.0.cancel();
    }
    pub(super) fn choose_directory(
        self: &Rc<Self>,
        token: u64,
        partition: Partition,
        preferences: DownloadPreferences,
        done: DownloadCompletion,
    ) {
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.is_active());
        let panel = gtk::FileChooserNative::new(
            Some("Choose where Zephium saves downloads"),
            window.as_ref(),
            gtk::FileChooserAction::SelectFolder,
            Some("Select"),
            Some("Cancel"),
        );
        self.calls.borrow_mut().insert(
            token,
            UiCall {
                partition,
                call: DownloadCall::ChooseDirectory,
                done,
            },
        );
        self.directory_panels
            .borrow_mut()
            .insert(token, (panel.clone(), ()));
        let weak = Rc::downgrade(self);
        panel.connect_response(move |panel, response| {
            let path = (response == gtk::ResponseType::Accept)
                .then(|| panel.filename())
                .flatten();
            panel.hide();
            let Some(manager) = weak.upgrade() else {
                return;
            };
            manager.directory_panels.borrow_mut().remove(&token);
            let Some(request) = manager.calls.borrow_mut().remove(&token) else {
                return;
            };
            if let Some(path) = path {
                manager.work.set(manager.work.get() + 1);
                let sender = manager.sender.clone();
                let preferences = preferences.clone();
                if std::thread::Builder::new()
                    .name("zephium-download-directory".into())
                    .spawn(move || {
                        let result = super::super::download_files::select_directory(path);
                        let _ =
                            sender.send(Message::DirectorySelected(request, preferences, result));
                    })
                    .is_err()
                {
                    manager.work.set(manager.work.get() - 1);
                }
            } else {
                request.done.finish(DownloadResponse::Error {
                    error: DownloadError::Cancelled,
                });
            }
        });
        panel.show();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    use wry::{WebViewBuilder, WebViewBuilderExtUnix, WebViewExtUnix};

    fn spin_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !condition() {
            assert!(Instant::now() < deadline, "native operation timed out");
            while glib::MainContext::default().pending() {
                glib::MainContext::default().iteration(false);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "requires a graphical desktop and WebKitGTK"]
    fn native_download_and_isolated_page_reader() {
        gtk::init().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let server = std::thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = [0; 4096];
                let count = stream.read(&mut request).unwrap_or(0);
                let file = request[..count].starts_with(b"GET /file");
                let body = if file {
                    "linux download fixture"
                } else {
                    "<html><head><title>Linux fixture</title></head><body><h1>Linux reader fixture</h1><p>This is a native WebKitGTK page reader test.</p><input type='file'><label><input id='check' type='checkbox'>Native checkbox</label></body></html>"
                };
                let extra = if file {
                    "Content-Disposition: attachment; filename=fixture.txt\r\nContent-Type: application/octet-stream\r\n"
                } else {
                    "Content-Type: text/html\r\n"
                };
                let _ = write!(stream, "HTTP/1.1 200 OK\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("store")).unwrap();
        let store = Arc::new(zephium_store::SqliteStore::open(root.path().join("store")).unwrap());
        let downloads = Downloads::new(store, Arc::new(|_| {}));
        let profile = ProfileId::generate();
        downloads.preferences.borrow_mut().insert(
            profile,
            DownloadPreferences {
                directory: Some(root.path().to_string_lossy().into()),
                ask_destination: false,
                ..Default::default()
            },
        );
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_default_size(640, 480);
        let fixed = gtk::Fixed::new();
        window.add(&fixed);
        let loaded = Rc::new(Cell::new(false));
        let view = WebViewBuilder::new()
            .with_linux_browser_mode(true)
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(640.0, 480.0).into(),
            })
            .build_gtk(&fixed)
            .unwrap();
        let native = view.webview();
        let ready = loaded.clone();
        native.connect_load_changed(move |_, event| {
            if event == webkit2gtk::LoadEvent::Finished {
                ready.set(true);
            }
        });
        let token = Arc::new(AtomicBool::new(true));
        let navigation = NavigationEpochTracker::new();
        navigation.begin(&base).unwrap();
        install(
            &view,
            Some(&downloads),
            Partition::Ephemeral(profile),
            EventPermit::bound(&token),
            Arc::new(AtomicBool::new(true)),
            navigation,
        );
        window.show_all();
        native.load_uri(&base);
        spin_until(|| loaded.get());
        native.download_uri(&format!("{base}/file"));
        spin_until(|| {
            downloads
                .recent
                .borrow()
                .iter()
                .any(|(_, r)| r.state.terminal())
        });
        let records = downloads.recent.borrow();
        let record = &records.front().unwrap().1;
        assert_eq!(record.state, DownloadState::Completed, "{:?}", record.error);
        assert_eq!(
            std::fs::read(root.path().join("fixture.txt")).unwrap(),
            b"linux download fixture"
        );
        drop(records);
        // Collision publication must retain the first file and number the next.
        native.download_uri(&format!("{base}/file"));
        spin_until(|| downloads.recent.borrow().len() == 2);
        assert_eq!(
            std::fs::read(root.path().join("fixture (1).txt")).unwrap(),
            b"linux download fixture"
        );
        #[cfg(feature = "agentic-browser")]
        {
            let tab = zephium_core::ids::ItemId::generate();
            let reader = crate::platform::linux::browse_reader::BrowseReader::new(
                &native,
                Rc::new(Cell::new(tab)),
                true,
                Rc::new(|| Some(1)),
                Rc::new(|| true),
            )
            .unwrap();
            reader.commit(1, &base);
            let result = Rc::new(RefCell::new(None));
            // Send result over a channel: the production completion is Send.
            let (sender, receiver) = mpsc::channel();
            reader.read(
                1,
                base.clone(),
                Box::new(move |answer| {
                    sender.send(answer).unwrap();
                }),
            );
            spin_until(|| {
                if let Ok(answer) = receiver.try_recv() {
                    *result.borrow_mut() = Some(answer);
                }
                result.borrow().is_some()
            });
            let answer = result.borrow_mut().take().unwrap().unwrap();
            assert!(answer
                .passages
                .iter()
                .any(|p| p.text.contains("Linux reader fixture")));
            assert_eq!(answer.title.as_deref(), Some("Linux fixture"));
            let checkbox = answer
                .passages
                .iter()
                .find(|passage| {
                    matches!(passage.role, crate::page_reader::PageRole::Checkbox { .. })
                })
                .expect("checkbox passage");
            let stored = crate::page_reader::planned_read(tab, answer.read).unwrap();
            let reference = stored.reference(checkbox.id).unwrap();
            use zephium_agentic::{
                SemanticActionIntent, SemanticActionProposal, SemanticEffectClass,
                SemanticSettleBudget, SemanticState, SemanticVerification, SemanticWaitCondition,
            };
            let proposal = SemanticActionProposal::try_new(
                SemanticActionIntent::Click { target: reference },
                SemanticEffectClass::LocalWrite,
                SemanticWaitCondition::Immediate,
                SemanticVerification::TargetState {
                    state: SemanticState::Checked,
                    present: true,
                },
                SemanticSettleBudget::try_new(250).unwrap(),
            )
            .unwrap();
            let (sender, receiver) = mpsc::channel();
            reader.act(
                1,
                answer.read,
                base.clone(),
                proposal,
                Box::new(move |answer| {
                    sender.send(answer).unwrap();
                }),
            );
            let mut action = None;
            spin_until(|| {
                if let Ok(value) = receiver.try_recv() {
                    action = Some(value);
                }
                action.is_some()
            });
            assert_eq!(action.unwrap(), Ok(()));
            let checked = Rc::new(Cell::new(false));
            let observed = checked.clone();
            native.evaluate_javascript(
                "document.getElementById('check').checked",
                None,
                None,
                None::<&gio::Cancellable>,
                move |value| {
                    use javascriptcore::ValueExt;
                    observed.set(value.unwrap().to_boolean());
                },
            );
            spin_until(|| checked.get());
        }
        let drained = Arc::new(AtomicBool::new(false));
        let drain = drained.clone();
        downloads.quiesce(
            None,
            Box::new(move |clean| {
                assert!(clean);
                drain.store(true, Ordering::Release);
            }),
        );
        spin_until(|| drained.load(Ordering::Acquire));
        unsafe {
            window.destroy();
        }
        stop.store(true, Ordering::Release);
        server.join().unwrap();
    }
}
