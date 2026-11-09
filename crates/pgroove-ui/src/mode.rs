//! App-wide editing mode.

use gpui_kit::Global;

/// When true (the default), writes are blocked server-side and editing UI is hidden.
pub struct ReadOnlyMode(pub bool);

impl Global for ReadOnlyMode {}
