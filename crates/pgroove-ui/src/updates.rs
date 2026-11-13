use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::update::{self, Release};

use crate::dialogs::DialogActions;
use crate::workspace::{Workspace, flatten};

/// Where the self-update process stands.
#[derive(Clone, Debug)]
pub(crate) enum UpdateState {
    Idle,
    Checking,
    Available(Release),
    Installing(pgroove_core::update::Version),
    Ready(pgroove_core::update::Version),
}

impl Workspace {
    /// Looks for a newer release; `manual` checks report their outcome in a notification.
    pub(crate) fn check_updates(
        &mut self,
        manual: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            self.update,
            UpdateState::Checking | UpdateState::Installing(_) | UpdateState::Ready(_)
        ) {
            return;
        }
        self.update = UpdateState::Checking;
        cx.notify();
        let http = self.http.clone();
        let etag = self.update_etag.clone();
        // PGROOVE_UPDATE_URL points the check at another release feed (for testing or mirrors).
        let url = std::env::var("PGROOVE_UPDATE_URL")
            .unwrap_or_else(|_| update::LATEST_RELEASE_URL.to_string());
        let task = self
            .runtime
            .spawn(async move { update::fetch_latest(&http, &url, etag.as_deref()).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = flatten(task.await);
            this.update_in(cx, move |this, window, cx| {
                let previous = std::mem::replace(&mut this.update, UpdateState::Idle);
                match result {
                    Ok(update::Latest::Release { release, etag }) => {
                        this.update_etag = etag;
                        if release.is_newer_than(&update::current_version()) {
                            if manual {
                                window.push_notification(
                                    format!("Version {} is available", release.version),
                                    cx,
                                );
                            }
                            this.update = UpdateState::Available(release);
                        } else if manual {
                            window.push_notification(
                                format!(
                                    "You're up to date (version {})",
                                    update::current_version()
                                ),
                                cx,
                            );
                        }
                    }
                    // GitHub saw the ETag we sent: nothing changed since the last check.
                    Ok(update::Latest::NotModified) => {
                        if matches!(previous, UpdateState::Available(_)) {
                            // The offered release is still the latest one.
                            this.update = previous;
                        } else if manual {
                            window.push_notification(
                                format!(
                                    "You're up to date (version {})",
                                    update::current_version()
                                ),
                                cx,
                            );
                        }
                    }
                    Err(e) => {
                        log::warn!("update check failed: {e}");
                        if manual {
                            window.push_notification(
                                Notification::error(format!("Couldn't check for updates: {e}")),
                                cx,
                            );
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Downloads, verifies and installs the available update.
    fn install_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let UpdateState::Available(release) = self.update.clone() else {
            return;
        };
        let Some((asset, package)) =
            update::pick_asset(&release, &update::current_target()).map(|(a, p)| (a.clone(), p))
        else {
            window.push_notification(
                Notification::error("There is no download for this platform."),
                cx,
            );
            return;
        };
        self.update = UpdateState::Installing(release.version.clone());
        cx.notify();
        let http = self.http.clone();
        let rel = release.clone();
        let task = self.runtime.spawn(async move {
            let bytes = update::download_verified(&http, &rel, &asset).await?;
            let binary = if cfg!(windows) {
                "pgroove.exe"
            } else {
                "pgroove"
            };
            let program = update::extract_binary(&bytes, package, binary)?;
            update::install(&program, package)?;
            anyhow::Ok(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = flatten(task.await);
            this.update_in(cx, move |this, window, cx| {
                match result {
                    Ok(()) => this.update = UpdateState::Ready(release.version.clone()),
                    Err(e) => {
                        log::warn!("update failed: {e}");
                        window.push_notification(
                            Notification::error(format!("Update failed: {e}")),
                            cx,
                        );
                        this.update = UpdateState::Available(release);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Starts the updated program and quits this one.
    fn restart_app(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_session(cx);
        match update::restart() {
            Ok(()) => cx.quit(),
            Err(e) => {
                window.push_notification(Notification::error(format!("Couldn't restart: {e}")), cx)
            }
        }
    }

    pub(crate) fn open_update_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let UpdateState::Available(release) = self.update.clone() else {
            return;
        };
        let this = cx.entity();
        let notes: String = release.notes.chars().take(1500).collect();
        let title = format!("Version {} is available", release.version);
        let current = format!("You have version {}.", update::current_version());
        window.open_dialog(cx, move |dialog, _, _| {
            let this = this.clone();
            dialog
                .title(title.clone())
                .w(px(520.))
                .child(
                    v_flex()
                        .id("update-notes")
                        .max_h(px(300.))
                        .overflow_y_scroll()
                        .gap_2()
                        .child(div().text_sm().child(current.clone()))
                        .child(div().text_xs().child(notes.clone())),
                )
                .actions("Install update", true, move |_, window, cx| {
                    this.update(cx, |ws, cx| ws.install_update(window, cx));
                    true
                })
        });
    }

    /// The title bar control for the update state, if there is something to show.
    pub(crate) fn update_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match &self.update {
            UpdateState::Available(r) => Some(
                Button::new("update-available")
                    .xsmall()
                    .outline()
                    .icon(IconName::Download)
                    .label(format!("Update to {}", r.version))
                    .tooltip("A new version is available")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_update_dialog(window, cx)),
                    )
                    .into_any_element(),
            ),
            UpdateState::Installing(v) => Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Updating to {v}…"))
                    .into_any_element(),
            ),
            UpdateState::Ready(v) => Some(
                Button::new("update-restart")
                    .xsmall()
                    .outline()
                    .icon(IconName::RefreshCw)
                    .label("Restart to update")
                    .tooltip(format!("Version {v} is installed"))
                    .on_click(cx.listener(|this, _, window, cx| this.restart_app(window, cx)))
                    .into_any_element(),
            ),
            UpdateState::Idle | UpdateState::Checking => None,
        }
    }
}
