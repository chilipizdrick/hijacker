use std::collections::HashMap;
use zbus::blocking::Connection;
use zbus::zvariant::Value;

pub fn notify(summary: &str, body: &str, icon: &str) {
    let Ok(conn) = Connection::session() else {
        return;
    };

    let hints: HashMap<&str, Value> = HashMap::new();
    let actions: Vec<&str> = Vec::new();

    let _ = conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "Hijacker", // app_name
            0u32,       // replaces_id
            icon,       // app_icon
            summary,    // summary
            body,       // body
            actions,    // actions
            hints,      // hints
            3000i32,    // expire_timeout in ms (-1 for default)
        ),
    );
}
