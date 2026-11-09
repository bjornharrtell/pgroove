//! Connection form dialog.

use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::{ConnectionProfile, SslMode};
use std::rc::Rc;

/// Input entities backing the connection dialog.
#[derive(Clone)]
pub struct ConnectionForm {
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    database: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    uri: Entity<InputState>,
    ssl: Entity<SslMode>,
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
            uri: field(
                window,
                cx,
                "",
                "postgres://user:password@host:5432/database",
                false,
            ),
            ssl: cx.new(|_| profile.ssl),
        }
    }

    /// Fills the fields from the URI input; returns an error message when it can't be parsed.
    pub fn fill_from_uri(&self, window: &mut Window, cx: &mut App) -> Result<(), &'static str> {
        let text = self.uri.read(cx).value().to_string();
        if text.trim().is_empty() {
            return Ok(());
        }
        let (p, pw) = ConnectionProfile::from_uri(&text)?;
        let set = |input: &Entity<InputState>, v: String, window: &mut Window, cx: &mut App| {
            input.update(cx, |i, cx| i.set_value(v, window, cx));
        };
        if self.name.read(cx).value().trim().is_empty() {
            set(&self.name, p.name.clone(), window, cx);
        }
        set(&self.host, p.host, window, cx);
        set(&self.port, p.port.to_string(), window, cx);
        set(&self.database, p.database, window, cx);
        set(&self.user, p.user, window, cx);
        if let Some(pw) = pw {
            set(&self.password, pw, window, cx);
        }
        self.ssl.update(cx, |s, _| *s = p.ssl);
        set(&self.uri, String::new(), window, cx);
        window.refresh();
        Ok(())
    }

    pub fn values(&self, cx: &App) -> FormValues {
        FormValues {
            profile: ConnectionProfile {
                name: self.name.read(cx).value().trim().to_string(),
                host: self.host.read(cx).value().trim().to_string(),
                port: parse_port(&self.port.read(cx).value()),
                database: self.database.read(cx).value().trim().to_string(),
                user: self.user.read(cx).value().trim().to_string(),
                ssl: *self.ssl.read(cx),
            },
            password: self.password.read(cx).value().to_string(),
        }
    }

    pub fn render(
        &self,
        cx: &App,
        on_test: Rc<dyn Fn(FormValues, &mut Window, &mut App)>,
    ) -> impl IntoElement {
        let row = |label: &'static str, input: &Entity<InputState>| {
            v_flex()
                .gap_1()
                .child(div().text_sm().child(label))
                .child(Input::new(input))
        };
        let form = self.clone();
        let fill = Button::new("fill-uri")
            .outline()
            .small()
            .label("Fill")
            .on_click({
                let form = self.clone();
                move |_, window, cx| {
                    if let Err(msg) = form.fill_from_uri(window, cx) {
                        window.push_notification(msg, cx);
                    }
                }
            });
        let ssl = *self.ssl.read(cx);
        let ssl_button = Button::new("ssl-mode")
            .outline()
            .small()
            .label(format!("SSL: {}", ssl.label()))
            .tooltip("Click to change the TLS mode")
            .on_click({
                let ssl_entity = self.ssl.clone();
                move |_, window, cx| {
                    ssl_entity.update(cx, |s, _| *s = s.next());
                    window.refresh();
                }
            });
        let test = Button::new("test-connection")
            .outline()
            .small()
            .label("Test connection")
            .on_click(move |_, window, cx| on_test(form.values(cx), window, cx));
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(div().flex_1().child(row("Connection URI", &self.uri)))
                    .child(fill),
            )
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
            .child(h_flex().gap_2().child(ssl_button).child(test))
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
