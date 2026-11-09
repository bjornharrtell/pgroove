//! Connection form dialog.

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::ConnectionProfile;

/// Input entities backing the connection dialog.
#[derive(Clone)]
pub struct ConnectionForm {
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    database: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
}

/// Raw form values.
pub struct FormValues {
    pub profile: ConnectionProfile,
    pub password: String,
}

fn field(
    window: &mut Window,
    cx: &mut App,
    value: &str,
    placeholder: &str,
    masked: bool,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .default_value(value.to_string())
            .placeholder(placeholder.to_string())
            .masked(masked)
    })
}

/// Parses a port field, returning 0 (rejected by validation) when invalid.
pub fn parse_port(text: &str) -> u16 {
    text.trim().parse().unwrap_or(0)
}

impl ConnectionForm {
    pub fn new(profile: &ConnectionProfile, window: &mut Window, cx: &mut App) -> Self {
        Self {
            name: field(window, cx, &profile.name, "prod-replica", false),
            host: field(window, cx, &profile.host, "localhost", false),
            port: field(window, cx, &profile.port.to_string(), "5432", false),
            database: field(window, cx, &profile.database, "postgres", false),
            user: field(window, cx, &profile.user, "postgres", false),
            password: field(window, cx, "", "Leave empty to keep saved password", true),
        }
    }

    pub fn values(&self, cx: &App) -> FormValues {
        FormValues {
            profile: ConnectionProfile {
                name: self.name.read(cx).value().trim().to_string(),
                host: self.host.read(cx).value().trim().to_string(),
                port: parse_port(&self.port.read(cx).value()),
                database: self.database.read(cx).value().trim().to_string(),
                user: self.user.read(cx).value().trim().to_string(),
            },
            password: self.password.read(cx).value().to_string(),
        }
    }

    pub fn render(&self) -> impl IntoElement {
        let row = |label: &'static str, input: &Entity<InputState>| {
            v_flex()
                .gap_1()
                .child(div().text_sm().child(label))
                .child(Input::new(input))
        };
        v_flex()
            .gap_3()
            .child(row("Name", &self.name))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(row("Host", &self.host)))
                    .child(div().w(px(90.)).child(row("Port", &self.port))),
            )
            .child(row("Database", &self.database))
            .child(row("User", &self.user))
            .child(row("Password", &self.password))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_port;

    #[test]
    fn port_parsing() {
        assert_eq!(parse_port(" 5432 "), 5432);
        assert_eq!(parse_port("abc"), 0);
        assert_eq!(parse_port("70000"), 0);
        assert_eq!(parse_port(""), 0);
    }
}
