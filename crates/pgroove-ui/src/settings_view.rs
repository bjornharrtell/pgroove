//! The settings tab: preferences kept in `settings.toml`, shown with the kit's settings
//! component. The pane holds a copy of the settings and reports every change as one event
//! carrying the new snapshot; the workspace compares it with what it had and applies only what
//! differs, so each control takes effect at once, as the palette commands did.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::setting::{
    SettingField, SettingGroup, SettingItem, SettingPage, Settings as SettingsPane,
};
use gpui_kit::component::{Sizable, h_flex};
use gpui_kit::*;
use pgroove_core::settings::{Appearance, Settings, limit_options, nearest_option, scale_options};

/// The settings were changed in the pane; `0` is the complete new state.
pub struct SettingsChanged(pub Settings);

/// The "Load new theme files" button was pressed.
pub struct ReloadThemesRequested;

impl EventEmitter<SettingsChanged> for SettingsView {}
impl EventEmitter<ReloadThemesRequested> for SettingsView {}

/// What a change in the settings requires the workspace to redo.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// Theme, appearance or interface size changed: re-apply the theme.
    pub theme: bool,
    /// The grid size changed: open grids rebuild their columns.
    pub grid: bool,
    /// The default row limit changed: query tabs take it.
    pub row_limit: bool,
    /// The default page size changed: table tabs take it.
    pub page_size: bool,
    /// Anything at all differs, so the file is written.
    pub save: bool,
}

/// Compares two states of the settings.
pub fn effects(old: &Settings, new: &Settings) -> Effects {
    Effects {
        theme: old.appearance != new.appearance
            || old.light_theme != new.light_theme
            || old.dark_theme != new.dark_theme
            || old.ui_scale != new.ui_scale
            || old.grid_scale != new.grid_scale,
        grid: old.grid_scale != new.grid_scale,
        row_limit: old.query_row_limit != new.query_row_limit,
        page_size: old.table_page_size != new.table_page_size,
        save: old != new,
    }
}

/// The settings tab.
pub struct SettingsView {
    settings: Settings,
    /// The themes on offer, as `(name, is_dark)`, taken when the tab opens and on reload.
    themes: Vec<(String, bool)>,
}

impl SettingsView {
    pub fn new(settings: Settings, themes: Vec<(String, bool)>) -> Self {
        Self { settings, themes }
    }

    /// Takes over settings that changed elsewhere (a hand edit, the row limit button).
    pub fn sync(
        &mut self,
        settings: Settings,
        themes: Vec<(String, bool)>,
        cx: &mut Context<Self>,
    ) {
        if self.settings != settings || self.themes != themes {
            self.settings = settings;
            self.themes = themes;
            cx.notify();
        }
    }

    /// Changes the pane's copy and tells the workspace.
    fn change(&mut self, edit: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let mut next = self.settings.clone();
        edit(&mut next);
        if next != self.settings {
            self.settings = next.clone();
            cx.emit(SettingsChanged(next));
            cx.notify();
        }
    }
}

/// `(value, label)` pairs for a dropdown.
fn pairs<T: ToString>(
    items: impl IntoIterator<Item = (T, String)>,
) -> Vec<(SharedString, SharedString)> {
    items
        .into_iter()
        .map(|(v, l)| (v.to_string().into(), l.into()))
        .collect()
}

/// The appearance choices as dropdown options.
fn appearance_options() -> Vec<(SharedString, SharedString)> {
    Appearance::ALL
        .iter()
        .map(|a| {
            let key = match a {
                Appearance::System => "system",
                Appearance::Light => "light",
                Appearance::Dark => "dark",
            };
            (key.into(), capitalize(a.label()).into())
        })
        .collect()
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

fn parse_appearance(key: &str) -> Appearance {
    match key {
        "light" => Appearance::Light,
        "dark" => Appearance::Dark,
        _ => Appearance::System,
    }
}

fn appearance_key(a: Appearance) -> &'static str {
    match a {
        Appearance::System => "system",
        Appearance::Light => "light",
        Appearance::Dark => "dark",
    }
}

/// The names of the themes of one kind, as dropdown options.
fn theme_options(themes: &[(String, bool)], dark: bool) -> Vec<(SharedString, SharedString)> {
    themes
        .iter()
        .filter(|(_, d)| *d == dark)
        .map(|(n, _)| (n.clone().into(), n.clone().into()))
        .collect()
}

/// How one setting is edited.
#[derive(Clone)]
enum Control {
    /// On or off.
    Switch {
        read: fn(&Settings) -> bool,
        write: fn(&mut Settings, bool),
    },
    /// One of a list of numbers, as `(value, label)`.
    Numbers {
        options: Vec<(u32, String)>,
        read: fn(&Settings) -> u32,
        write: fn(&mut Settings, u32),
    },
    /// One of a list of names, as `(value, label)`.
    Names {
        options: Vec<(SharedString, SharedString)>,
        read: fn(&Settings) -> String,
        write: fn(&mut Settings, String),
    },
    /// A button that asks for the theme files to be loaded again.
    ReloadThemes,
}

/// One row of the pane.
#[derive(Clone)]
struct Item {
    title: &'static str,
    help: &'static str,
    control: Control,
}

/// A titled block of rows.
struct Group {
    title: &'static str,
    items: Vec<Item>,
}

/// A page of the pane: its own entry in the pane's sidebar.
struct Page {
    title: &'static str,
    icon: IconName,
    groups: Vec<Group>,
}

fn item(title: &'static str, help: &'static str, control: Control) -> Item {
    Item {
        title,
        help,
        control,
    }
}

/// Everything the pane shows, as data. `themes` are the `(name, is_dark)` pairs on offer.
fn pages(themes: &[(String, bool)]) -> Vec<Page> {
    let limits = limit_options();
    vec![
        Page {
            title: "General",
            icon: IconName::Settings2,
            groups: vec![
                Group {
                    title: "Updates",
                    items: vec![item(
                        "Check for updates on start",
                        "Looks for a newer release a few seconds after the app starts.",
                        Control::Switch {
                            read: |s| s.check_for_updates,
                            write: |s, v| s.check_for_updates = v,
                        },
                    )],
                },
                Group {
                    title: "Data",
                    items: vec![
                        item(
                            "Query row limit",
                            "Rows a query returns at most. Each query tab can still change its \
                             own limit; this is what new and open tabs use.",
                            Control::Numbers {
                                options: limits.clone(),
                                read: |s| s.query_row_limit,
                                write: |s, v| s.query_row_limit = v,
                            },
                        ),
                        item(
                            "Table page size",
                            "Rows per page when a table is opened from the tree.",
                            Control::Numbers {
                                options: limits,
                                read: |s| s.table_page_size,
                                write: |s, v| s.table_page_size = v,
                            },
                        ),
                    ],
                },
            ],
        },
        Page {
            title: "Appearance",
            icon: IconName::Palette,
            groups: vec![
                Group {
                    title: "Theme",
                    items: vec![
                        item(
                            "Appearance",
                            "Light, dark, or follow the desktop.",
                            Control::Names {
                                options: appearance_options(),
                                read: |s| appearance_key(s.appearance).to_string(),
                                write: |s, v| s.appearance = parse_appearance(&v),
                            },
                        ),
                        item(
                            "Light theme",
                            "Used when the appearance is light.",
                            Control::Names {
                                options: theme_options(themes, false),
                                read: |s| s.light_theme.clone(),
                                write: |s, v| s.light_theme = v,
                            },
                        ),
                        item(
                            "Dark theme",
                            "Used when the appearance is dark.",
                            Control::Names {
                                options: theme_options(themes, true),
                                read: |s| s.dark_theme.clone(),
                                write: |s, v| s.dark_theme = v,
                            },
                        ),
                        item(
                            "Theme files",
                            "Picks up theme files added to the themes folder.",
                            Control::ReloadThemes,
                        ),
                    ],
                },
                Group {
                    title: "Size",
                    items: vec![
                        item(
                            "Interface size",
                            "Text, rows and bars of the whole interface.",
                            Control::Numbers {
                                options: scale_options(),
                                read: |s| s.ui_scale,
                                write: |s, v| s.ui_scale = v,
                            },
                        ),
                        item(
                            "Grid size",
                            "Text, rows and columns of result grids and tables.",
                            Control::Numbers {
                                options: scale_options(),
                                read: |s| s.grid_scale,
                                write: |s, v| s.grid_scale = v,
                            },
                        ),
                    ],
                },
            ],
        },
        Page {
            title: "SQL",
            icon: IconName::Code,
            groups: vec![Group {
                title: "Generated SQL",
                items: vec![item(
                    "Upper-case keywords",
                    "Writes keywords such as SELECT and FROM in capitals in generated SQL.",
                    Control::Switch {
                        read: |s| s.uppercase_generated_sql,
                        write: |s, v| s.uppercase_generated_sql = v,
                    },
                )],
            }],
        },
    ]
}

/// Reads and writes the pane's settings from inside the kit's field closures, which must be
/// `'static`: it holds only a weak handle to the view.
#[derive(Clone)]
struct Access(WeakEntity<SettingsView>);

impl Access {
    fn get<T>(&self, cx: &App, read: impl FnOnce(&Settings) -> T, fallback: T) -> T {
        self.0
            .upgrade()
            .map_or(fallback, |v| read(&v.read(cx).settings))
    }

    fn set(&self, cx: &mut App, edit: impl FnOnce(&mut Settings)) {
        self.0.update(cx, |v, cx| v.change(edit, cx)).ok();
    }

    /// The kit item for one row.
    fn item(&self, item: Item) -> SettingItem {
        let (a, b) = (self.clone(), self.clone());
        let (title, help) = (item.title, item.help);
        let built = match item.control {
            Control::Switch { read, write } => SettingItem::new(
                title,
                SettingField::switch(
                    move |cx| a.get(cx, read, false),
                    move |v, cx| b.set(cx, |s| write(s, v)),
                ),
            ),
            Control::Numbers {
                options,
                read,
                write,
            } => {
                let shown = options.clone();
                SettingItem::new(
                    title,
                    SettingField::dropdown(
                        pairs(options),
                        move |cx| displayed(&shown, a.get(cx, read, 0)).into(),
                        move |v, cx| {
                            if let Some(n) = parse_choice(&v) {
                                b.set(cx, |s| write(s, n));
                            }
                        },
                    ),
                )
            }
            Control::Names {
                options,
                read,
                write,
            } => SettingItem::new(
                title,
                SettingField::dropdown(
                    options,
                    move |cx| a.get(cx, read, String::new()).into(),
                    move |v, cx| b.set(cx, |s| write(s, v.to_string())),
                ),
            ),
            Control::ReloadThemes => SettingItem::new(
                title,
                SettingField::render(move |_, _, _| {
                    let view = a.0.clone();
                    h_flex().child(
                        Button::new("reload-themes")
                            .outline()
                            .small()
                            .label("Load new theme files")
                            .on_click(move |_, _, cx| {
                                view.update(cx, |_, cx| cx.emit(ReloadThemesRequested)).ok();
                            }),
                    )
                }),
            ),
        };
        built.description(help)
    }
}

/// The option a number is shown as: the number itself when it is one of the options, else the
/// nearest one, so a hand-edited value shows as something rather than as nothing.
fn displayed(options: &[(u32, String)], value: u32) -> String {
    nearest_option(options, value).unwrap_or(value).to_string()
}

/// The number behind a dropdown choice.
fn parse_choice(value: &str) -> Option<u32> {
    value.parse().ok()
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let me = Access(cx.weak_entity());
        let pages = pages(&self.themes).into_iter().map(|page| {
            let mut kit = SettingPage::new(page.title).icon(page.icon);
            if page.title == "General" {
                kit = kit.default_open(true);
            }
            for group in page.groups {
                kit = kit.group(
                    SettingGroup::new()
                        .title(group.title)
                        .items(group.items.into_iter().map(|i| me.item(i))),
                );
            }
            kit
        });
        div()
            .size_full()
            .child(SettingsPane::new("settings").pages(pages))
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: a glob of `super::*` would bring in `gpui_kit::test`, which shadows the
    // built-in `#[test]`.
    use super::{
        Control, Effects, appearance_key, appearance_options, capitalize, displayed, effects,
        pages, pairs, parse_appearance, parse_choice, theme_options,
    };
    use pgroove_core::settings::{Appearance, Settings};

    /// The effects of changing one thing in the default settings.
    fn changed(edit: impl FnOnce(&mut Settings)) -> Effects {
        let old = Settings::default();
        let mut new = old.clone();
        edit(&mut new);
        effects(&old, &new)
    }

    fn only_save() -> Effects {
        Effects {
            save: true,
            ..Effects::default()
        }
    }

    #[test]
    fn nothing_changed_has_no_effects() {
        let s = Settings::default();
        assert_eq!(effects(&s, &s), Effects::default());
    }

    #[test]
    fn plain_preferences_only_save() {
        assert_eq!(
            changed(|s| s.check_for_updates = !s.check_for_updates),
            only_save()
        );
        assert_eq!(changed(|s| s.uppercase_generated_sql = true), only_save());
    }

    #[test]
    fn theme_changes_reapply_the_theme() {
        let theme_only = Effects {
            theme: true,
            save: true,
            ..Effects::default()
        };
        assert_eq!(changed(|s| s.appearance = Appearance::Dark), theme_only);
        assert_eq!(changed(|s| s.light_theme = "Other".into()), theme_only);
        assert_eq!(changed(|s| s.dark_theme = "Other".into()), theme_only);
        assert_eq!(changed(|s| s.ui_scale = 110), theme_only);
    }

    #[test]
    fn grid_size_also_rebuilds_grids() {
        assert_eq!(
            changed(|s| s.grid_scale = 90),
            Effects {
                theme: true,
                grid: true,
                save: true,
                ..Effects::default()
            }
        );
    }

    #[test]
    fn limits_go_to_their_own_tabs() {
        assert_eq!(
            changed(|s| s.query_row_limit = 1_000),
            Effects {
                row_limit: true,
                save: true,
                ..Effects::default()
            }
        );
        assert_eq!(
            changed(|s| s.table_page_size = 1_000),
            Effects {
                page_size: true,
                save: true,
                ..Effects::default()
            }
        );
    }

    fn themes() -> Vec<(String, bool)> {
        vec![("Light A".into(), false), ("Dark A".into(), true)]
    }

    /// Every row of the pane, with the control it uses.
    fn controls() -> Vec<(&'static str, Control)> {
        pages(&themes())
            .into_iter()
            .flat_map(|p| p.groups)
            .flat_map(|g| g.items)
            .map(|i| (i.title, i.control))
            .collect()
    }

    #[test]
    fn the_pane_has_the_three_pages() {
        let titles: Vec<_> = pages(&themes()).iter().map(|p| p.title).collect();
        assert_eq!(titles, ["General", "Appearance", "SQL"]);
    }

    #[test]
    fn every_preference_has_a_row() {
        let rows: Vec<_> = controls().into_iter().map(|(t, _)| t).collect();
        assert_eq!(
            rows,
            [
                "Check for updates on start",
                "Query row limit",
                "Table page size",
                "Appearance",
                "Light theme",
                "Dark theme",
                "Theme files",
                "Interface size",
                "Grid size",
                "Upper-case keywords",
            ]
        );
    }

    #[test]
    fn rows_read_and_write_their_own_setting() {
        let mut s = Settings::default();
        for (title, control) in controls() {
            match (title, control) {
                ("Check for updates on start", Control::Switch { read, write }) => {
                    write(&mut s, false);
                    assert!(!read(&s) && !s.check_for_updates);
                }
                ("Upper-case keywords", Control::Switch { read, write }) => {
                    write(&mut s, true);
                    assert!(read(&s) && s.uppercase_generated_sql);
                }
                ("Query row limit", Control::Numbers { read, write, .. }) => {
                    write(&mut s, 1_000);
                    assert_eq!((read(&s), s.query_row_limit), (1_000, 1_000));
                }
                ("Table page size", Control::Numbers { read, write, .. }) => {
                    write(&mut s, 50);
                    assert_eq!((read(&s), s.table_page_size), (50, 50));
                }
                ("Interface size", Control::Numbers { read, write, .. }) => {
                    write(&mut s, 110);
                    assert_eq!((read(&s), s.ui_scale), (110, 110));
                }
                ("Grid size", Control::Numbers { read, write, .. }) => {
                    write(&mut s, 90);
                    assert_eq!((read(&s), s.grid_scale), (90, 90));
                }
                ("Appearance", Control::Names { read, write, .. }) => {
                    write(&mut s, "dark".into());
                    assert_eq!(
                        (read(&s).as_str(), s.appearance),
                        ("dark", Appearance::Dark)
                    );
                }
                (
                    "Light theme",
                    Control::Names {
                        read,
                        write,
                        options,
                    },
                ) => {
                    write(&mut s, "Light A".into());
                    assert_eq!(
                        (read(&s).as_str(), s.light_theme.as_str()),
                        ("Light A", "Light A")
                    );
                    assert_eq!(options.len(), 1, "only light themes");
                }
                (
                    "Dark theme",
                    Control::Names {
                        read,
                        write,
                        options,
                    },
                ) => {
                    write(&mut s, "Dark A".into());
                    assert_eq!(read(&s), "Dark A");
                    assert_eq!(options.len(), 1, "only dark themes");
                }
                ("Theme files", Control::ReloadThemes) => {}
                other => panic!("unexpected row {}", other.0),
            }
        }
        // Each write touched only its own setting: the others kept their other changes.
        assert!(!s.check_for_updates && s.uppercase_generated_sql);
        assert_eq!((s.ui_scale, s.grid_scale), (110, 90));
    }

    #[test]
    fn every_choice_offered_is_a_value_the_app_accepts() {
        use pgroove_core::settings::{clamp_row_limit, clamp_scale};
        for (title, control) in controls() {
            if let Control::Numbers { options, .. } = control {
                assert!(!options.is_empty(), "{title}");
                for (v, _) in options {
                    let kept = if title.contains("size") && title != "Table page size" {
                        clamp_scale(v)
                    } else {
                        clamp_row_limit(v)
                    };
                    assert_eq!(kept, v, "{title} offers {v}, which would be changed");
                }
            }
        }
    }

    #[test]
    fn numbers_show_as_themselves_or_the_nearest() {
        let options = vec![(100, "100".to_string()), (200, "200".to_string())];
        assert_eq!(displayed(&options, 200), "200");
        assert_eq!(displayed(&options, 120), "100");
        assert_eq!(displayed(&[], 7), "7");
        assert_eq!(parse_choice("500"), Some(500));
        assert_eq!(parse_choice("x"), None);
        assert_eq!(parse_choice("-1"), None);
    }

    #[test]
    fn appearance_keys_roundtrip() {
        for a in Appearance::ALL {
            assert_eq!(parse_appearance(appearance_key(a)), a);
        }
        assert_eq!(parse_appearance("nonsense"), Appearance::System);
        let options = appearance_options();
        assert_eq!(options.len(), 3);
        assert_eq!(options[0].1.as_ref(), "Follow the system");
        assert_eq!(options[1].1.as_ref(), "Always light");
    }

    #[test]
    fn theme_options_split_by_kind() {
        let themes = vec![
            ("Light A".to_string(), false),
            ("Dark A".to_string(), true),
            ("Light B".to_string(), false),
        ];
        let light = theme_options(&themes, false);
        assert_eq!(light.len(), 2);
        assert_eq!(light[0].0.as_ref(), "Light A");
        assert_eq!(theme_options(&themes, true).len(), 1);
        assert!(theme_options(&[], true).is_empty());
    }

    #[test]
    fn dropdown_pairs_carry_value_and_label() {
        let p = pairs(vec![
            (200u32, "200".to_string()),
            (1000, "1 000".to_string()),
        ]);
        assert_eq!(p[1].0.as_ref(), "1000");
        assert_eq!(p[1].1.as_ref(), "1 000");
        assert_eq!(capitalize("always dark"), "Always dark");
        assert_eq!(capitalize(""), "");
    }
}
