use std::rc::Rc;

use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::*;
use gpui_kit::*;

use crate::mode::ReadOnlyMode;
use crate::palette::{self, PaletteCommand};
use crate::updates::UpdateState;
use crate::workspace::Workspace;

impl Workspace {
    /// Runs a palette command; query commands act on the active (or a new) query tab.
    fn run_command(&mut self, cmd: PaletteCommand, window: &mut Window, cx: &mut Context<Self>) {
        use PaletteCommand::*;
        match cmd {
            NewQuery => {
                self.new_query(window, cx);
            }
            RunQuery | ExplainQuery | AnalyzeQuery | FormatSql | ShowHistory | ShowSnippets => {
                let tab = self
                    .active_query()
                    .unwrap_or_else(|| self.query_tab(window, cx));
                tab.update(cx, |t, cx| match cmd {
                    RunQuery => t.run_query(cx),
                    ExplainQuery => t.explain(false, cx),
                    AnalyzeQuery => t.explain(true, cx),
                    FormatSql => t.format_sql(window, cx),
                    ShowHistory => t.open_history(window, cx),
                    _ => t.open_snippets(window, cx),
                });
            }
            ToggleReadOnly => self.toggle_read_only(cx),
            OpenActivity => {
                if let Some(sid) = self.focused_session(cx) {
                    self.open_activity(sid, window, cx);
                }
            }
            RefreshTree => self.refresh_all(cx),
            NewConnection => self.open_form(None, window, cx),
            ImportDbeaver => self.offer_dbeaver_import(false, window, cx),
            ImportDbeaverFrom => self.import_dbeaver_from_folder(window, cx),
            Connect(name) => {
                if let Some(p) = self
                    .store
                    .profiles()
                    .iter()
                    .find(|p| p.name == name)
                    .cloned()
                {
                    self.open_session(p, cx);
                }
            }
            Disconnect => {
                if let Some(sid) = self.focused_session(cx) {
                    self.close_session(sid, cx);
                }
            }
            CheckUpdates => self.check_updates(true, window, cx),
            InstallUpdate => self.open_update_dialog(window, cx),
            OpenSettings => self.open_settings(window, cx),
        }
    }

    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let names: Vec<String> = self
            .store
            .profiles()
            .iter()
            .map(|p| p.name.clone())
            .collect();
        let groups = std::rc::Rc::new(palette::groups(
            &names,
            &palette::PaletteContext {
                connected: self.ready_sessions().next().is_some(),
                read_only: cx.global::<ReadOnlyMode>().0,
                update_available: matches!(self.update, UpdateState::Available(_)),
            },
        ));
        let state = cx.new(|cx| CommandState::new(window, cx));
        let this = cx.entity();
        let focus_on_mount = Rc::new(std::cell::Cell::new(true));
        window.open_dialog(cx, move |dialog, _, _| {
            let (state, groups, this) = (state.clone(), groups.clone(), this.clone());
            let focus_on_mount = focus_on_mount.clone();
            dialog
                .close_button(false)
                .p_0()
                .content(move |content, window, cx| {
                    if focus_on_mount.replace(false) {
                        let state = state.clone();
                        window.defer(cx, move |window, cx| {
                            state.read(cx).focus_handle(cx).focus(window, cx);
                        });
                    }
                    let mut command = Command::new(&state)
                        .bordered(false)
                        .placeholder("Type a command…");
                    for g in groups.iter() {
                        command = command.group(g.entries.iter().fold(
                            CommandGroup::new().label(g.label),
                            |grp, e| {
                                grp.item(
                                    CommandItem::new()
                                        .label(e.title.clone())
                                        .keywords(e.keywords.clone()),
                                )
                            },
                        ));
                    }
                    let (groups, this) = (groups.clone(), this.clone());
                    content.child(command.on_confirm(move |ix, window, cx| {
                        window.close_dialog(cx);
                        if let Some(cmd) = palette::command_at(&groups, ix.section, ix.row) {
                            this.update(cx, |ws, cx| ws.run_command(cmd, window, cx));
                        }
                    }))
                })
        });
    }

    pub(crate) fn toggle_read_only(&mut self, cx: &mut Context<Self>) {
        let read_only = !cx.global::<ReadOnlyMode>().0;
        cx.set_global(ReadOnlyMode(read_only));
        for client in self.ready_sessions().filter_map(|s| s.client.clone()) {
            self.runtime.spawn(async move {
                if let Err(e) = pgroove_core::edit::set_read_only(&client, read_only).await {
                    log::warn!("could not switch read-only mode: {e:#}");
                }
            });
        }
        cx.refresh_windows();
    }
}
