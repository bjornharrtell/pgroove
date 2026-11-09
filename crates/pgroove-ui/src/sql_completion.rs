//! Editor completion backed by the connected database's schema.

use std::sync::{Arc, RwLock};

use gpui_kit::component::input::{CompletionProvider, Rope, RopeExt};
use gpui_kit::*;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    InsertReplaceEdit, Range,
};
use pgroove_core::completion::{self, Index, Kind};

/// Shared slot the workspace fills after connecting.
pub type SharedIndex = Arc<RwLock<Option<Index>>>;

pub struct SqlCompletions {
    pub index: SharedIndex,
}

fn item_kind(k: Kind) -> CompletionItemKind {
    match k {
        Kind::Keyword => CompletionItemKind::KEYWORD,
        Kind::Schema => CompletionItemKind::MODULE,
        Kind::Table => CompletionItemKind::STRUCT,
        Kind::Column => CompletionItemKind::FIELD,
    }
}

/// Whether typing `new_text` should open the completion list.
pub fn is_trigger(new_text: &str) -> bool {
    let mut chars = new_text.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_alphanumeric() || c == '_' || c == '.')
}

impl CompletionProvider for SqlCompletions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let guard = self.index.read().ok();
        let Some(index) = guard.as_deref().and_then(Option::as_ref) else {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        };
        let full = text.to_string();
        let offset = offset.min(full.len());
        let stmt = completion::current_statement(&full, offset);
        let stmt_start = stmt.as_ptr() as usize - full.as_ptr() as usize;
        let before = &full[stmt_start..offset];
        let result = completion::suggest(index, before, stmt);
        let start = offset - result.prefix_len;
        let range = Range::new(
            text.offset_to_position(start),
            text.offset_to_position(offset),
        );
        let items = result
            .items
            .into_iter()
            .map(|s| CompletionItem {
                label: s.label.clone(),
                kind: Some(item_kind(s.kind)),
                detail: s.detail,
                text_edit: Some(CompletionTextEdit::InsertAndReplace(InsertReplaceEdit {
                    new_text: s.label,
                    insert: range,
                    replace: range,
                })),
                ..Default::default()
            })
            .collect();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        is_trigger(new_text)
    }
}

#[cfg(test)]
mod tests {
    use super::is_trigger;

    #[test]
    fn triggers() {
        assert!(is_trigger("a"));
        assert!(is_trigger("_"));
        assert!(is_trigger("."));
        assert!(is_trigger("7"));
        assert!(!is_trigger(" "));
        assert!(!is_trigger("ab"));
        assert!(!is_trigger(""));
        assert!(!is_trigger("("));
    }
}
