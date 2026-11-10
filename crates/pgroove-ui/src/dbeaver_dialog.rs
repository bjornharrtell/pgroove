//! Dialog for importing connections found in DBeaver.

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::*;
use gpui_kit::*;
use pgroove_core::import_dbeaver::Candidate;

/// Which of the found connections are ticked, and whether passwords come along.
pub struct ImportState {
    pub candidates: Vec<Candidate>,
    pub selected: Vec<bool>,
    pub passwords: bool,
}

impl ImportState {
    pub fn new(candidates: Vec<Candidate>) -> Self {
        Self {
            selected: vec![true; candidates.len()],
            candidates,
            passwords: true,
        }
    }

    /// The ticked connections; passwords are dropped unless they were asked for.
    pub fn picks(&self) -> Vec<Candidate> {
        self.candidates
            .iter()
            .zip(&self.selected)
            .filter(|(_, on)| **on)
            .map(|(c, _)| {
                let mut c = c.clone();
                if !self.passwords {
                    c.password = None;
                }
                c
            })
            .collect()
    }

    pub fn set_all(&mut self, on: bool) {
        self.selected.iter_mut().for_each(|s| *s = on);
    }
}

pub fn render(state: &Entity<ImportState>, cx: &App) -> impl IntoElement {
    let theme = cx.theme().clone();
    let s = state.read(cx);
    let with_pw = s.candidates.iter().filter(|c| c.password.is_some()).count();
    let rows = s.candidates.iter().enumerate().map(|(i, c)| {
        let entity = state.clone();
        let detail = [
            (!c.profile.group.is_empty()).then(|| c.profile.group.clone()),
            Some(c.profile.label()),
            c.note.clone(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        v_flex()
            .gap_0p5()
            .child(
                Checkbox::new(("dbeaver-pick", i))
                    .label(c.profile.name.clone())
                    .checked(s.selected[i])
                    .on_click(move |on, window, cx| {
                        entity.update(cx, |s, _| s.selected[i] = *on);
                        window.refresh();
                    }),
            )
            .child(
                div()
                    .pl_6()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(detail),
            )
    });
    let (all, none) = (state.clone(), state.clone());
    let entity = state.clone();
    v_flex()
        .gap_3()
        .child(div().text_sm().child(format!(
            "Found {} PostgreSQL connection{} in DBeaver that pgroove doesn't have yet. \
             DBeaver's files are only read, never changed.",
            s.candidates.len(),
            if s.candidates.len() == 1 { "" } else { "s" }
        )))
        .child(
            h_flex()
                .gap_3()
                .text_xs()
                .child(
                    div()
                        .id("dbeaver-all")
                        .cursor_pointer()
                        .text_color(theme.primary)
                        .child("Select all")
                        .on_click(move |_, window, cx| {
                            all.update(cx, |s, _| s.set_all(true));
                            window.refresh();
                        }),
                )
                .child(
                    div()
                        .id("dbeaver-none")
                        .cursor_pointer()
                        .text_color(theme.primary)
                        .child("Select none")
                        .on_click(move |_, window, cx| {
                            none.update(cx, |s, _| s.set_all(false));
                            window.refresh();
                        }),
                ),
        )
        .child(
            v_flex()
                .id("dbeaver-list")
                .gap_2()
                .max_h(px(300.))
                .overflow_y_scroll()
                .children(rows),
        )
        .children((with_pw > 0).then(|| {
            Checkbox::new("dbeaver-passwords")
                .label(format!(
                    "Also import saved passwords ({with_pw} found) into the system keychain"
                ))
                .checked(s.passwords)
                .on_click(move |on, window, cx| {
                    entity.update(cx, |s, _| s.passwords = *on);
                    window.refresh();
                })
        }))
}

#[cfg(test)]
mod tests {
    use super::ImportState;
    use pgroove_core::ConnectionProfile;
    use pgroove_core::import_dbeaver::Candidate;

    fn cand(name: &str, pw: Option<&str>) -> Candidate {
        Candidate {
            profile: ConnectionProfile {
                name: name.into(),
                ..Default::default()
            },
            password: pw.map(str::to_string),
            note: None,
        }
    }

    #[test]
    fn picks_respect_selection_and_password_choice() {
        let mut s = ImportState::new(vec![cand("a", Some("x")), cand("b", Some("y"))]);
        assert_eq!(s.picks().len(), 2);
        s.selected[1] = false;
        assert_eq!(s.picks()[0].password.as_deref(), Some("x"));
        s.passwords = false;
        assert_eq!(s.picks()[0].password, None);
        s.set_all(false);
        assert!(s.picks().is_empty());
        s.set_all(true);
        assert_eq!(s.picks().len(), 2);
    }
}
