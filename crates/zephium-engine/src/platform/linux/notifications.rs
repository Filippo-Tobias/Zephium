//! Page notifications through the desktop's freedesktop notification service.
use gtk::{gio, glib, prelude::*};
use std::collections::HashMap;
use webkit2gtk::{NotificationExt, WebViewExt};

pub(super) fn install(view: &webkit2gtk::WebView) {
    view.connect_show_notification(|view, notification| {
        let origin = view
            .uri()
            .and_then(|uri| url::Url::parse(&uri).ok())
            .map(|url| url.origin().ascii_serialization())
            .unwrap_or_else(|| "Website".into());
        let title: String = notification
            .title()
            .unwrap_or_default()
            .chars()
            .take(256)
            .collect();
        let body: String = notification
            .body()
            .unwrap_or_default()
            .chars()
            .take(4096)
            .collect();
        let summary = format!("{origin} — {title}");
        let body = glib::markup_escape_text(&body).to_string();
        let notification = notification.clone();
        glib::MainContext::default().spawn_local(async move {
            let Ok(bus) = gio::bus_get_future(gio::BusType::Session).await else {
                notification.close();
                return;
            };
            let hints = HashMap::<&str, glib::Variant>::from([(
                "desktop-entry",
                "app.zephium".to_variant(),
            )]);
            let params = (
                "Zephium",
                0u32,
                "web-browser",
                summary,
                body,
                Vec::<String>::new(),
                hints,
                -1i32,
            )
                .to_variant();
            let result = bus
                .call_future(
                    Some("org.freedesktop.Notifications"),
                    "/org/freedesktop/Notifications",
                    "org.freedesktop.Notifications",
                    "Notify",
                    Some(&params),
                    None,
                    gio::DBusCallFlags::NONE,
                    5000,
                )
                .await;
            if result.is_err() {
                notification.close();
            }
        });
        true
    });
}
