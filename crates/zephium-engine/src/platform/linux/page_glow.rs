//! A static GTK outline for the active assistant tab; no animation timer.
use crate::page_reader::PageGlow;
use gtk::prelude::*;
use std::{cell::RefCell, collections::HashMap};
use wry::WebViewExtUnix;
use zephium_core::ids::ItemId;
thread_local! {
    static GLOWS: RefCell<HashMap<ItemId, (gtk::StyleContext, gtk::CssProvider)>> = RefCell::default();
}
pub(crate) fn forget(tab: ItemId) {
    GLOWS.with(|glows| {
        if let Some((context, provider)) = glows.borrow_mut().remove(&tab) {
            context.remove_provider(&provider);
        }
    });
}
pub(crate) fn clear() {
    GLOWS.with(|glows| {
        for (_, (context, provider)) in glows.borrow_mut().drain() {
            context.remove_provider(&provider);
        }
    });
}
pub(crate) fn set(tab: ItemId, view: &wry::WebView, glow: PageGlow, _: bool) {
    forget(tab);
    if glow == PageGlow::Off {
        return;
    }
    let context = view.webview().style_context();
    let provider = gtk::CssProvider::new();
    if provider
        .load_from_data(b"* { outline: 2px solid #a78bfa; outline-offset: -3px; }")
        .is_ok()
    {
        context.add_provider(&provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        GLOWS.with(|glows| {
            glows.borrow_mut().insert(tab, (context, provider));
        });
    }
}
