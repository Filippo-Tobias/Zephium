//! Native camera and microphone observations for the browser indicator.
use std::rc::Rc;
use webkit2gtk::WebViewExt;
use wry::WebViewExtUnix;
use zephium_core::ports::engine::{CaptureDeviceState, MediaCaptureState};
fn device(state: webkit2gtk::MediaCaptureState) -> CaptureDeviceState {
    match state {
        webkit2gtk::MediaCaptureState::Active => CaptureDeviceState::Active,
        webkit2gtk::MediaCaptureState::Muted => CaptureDeviceState::Muted,
        _ => CaptureDeviceState::None,
    }
}
fn sample(view: &webkit2gtk::WebView) -> MediaCaptureState {
    MediaCaptureState {
        camera: device(view.camera_capture_state()),
        microphone: device(view.microphone_capture_state()),
    }
}
pub(crate) fn install(view: &wry::WebView, changed: impl Fn(MediaCaptureState) + 'static) {
    let native = view.webview();
    let changed = Rc::new(changed);
    let microphone_changed = changed.clone();
    native.connect_microphone_capture_state_notify(move |view| microphone_changed(sample(view)));
    native.connect_camera_capture_state_notify(move |view| changed(sample(view)));
}
