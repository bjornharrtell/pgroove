//! Shared dialog footer: a plain gpui-kit `Dialog` renders no buttons by itself.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{Dialog, DialogFooter};
use gpui_kit::component::*;
use gpui_kit::*;

pub trait DialogActions {
    /// Adds an OK button (and optionally Cancel) in a footer, and wires Enter to OK.
    /// `ok` returns whether the dialog should close.
    fn actions(
        self,
        ok_label: &'static str,
        show_cancel: bool,
        ok: impl Fn(&ClickEvent, &mut Window, &mut App) -> bool + 'static,
    ) -> Self;
}

impl DialogActions for Dialog {
    fn actions(
        self,
        ok_label: &'static str,
        show_cancel: bool,
        ok: impl Fn(&ClickEvent, &mut Window, &mut App) -> bool + 'static,
    ) -> Self {
        let ok = Rc::new(ok);
        let button_ok = ok.clone();
        let mut footer = DialogFooter::new().gap_2().justify_end().pt_3();
        if show_cancel {
            footer = footer.child(
                Button::new("dialog-cancel")
                    .outline()
                    .label("Cancel")
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            );
        }
        footer = footer.child(Button::new("dialog-ok").primary().label(ok_label).on_click(
            move |e, window, cx| {
                if button_ok(e, window, cx) {
                    window.close_dialog(cx);
                }
            },
        ));
        self.footer(footer).on_ok(move |e, w, c| ok(e, w, c))
    }
}
