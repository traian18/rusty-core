//! The colours and the caps on how much of a long block is drawn.

use ratatui::prelude::*;

pub(super) const BG: Color = Color::Rgb(13, 15, 19);

pub(super) const PANEL: Color = Color::Rgb(20, 23, 29);

pub(super) const MUTED: Color = Color::Rgb(129, 138, 152);

pub(super) const SUBTLE: Color = Color::Rgb(90, 98, 112);

pub(super) const TEXT: Color = Color::Rgb(230, 234, 241);

pub(super) const ACCENT: Color = Color::Rgb(122, 162, 255);

pub(super) const SUCCESS: Color = Color::Rgb(115, 209, 158);

pub(super) const WARNING: Color = Color::Rgb(240, 188, 96);

pub(super) const ERROR: Color = Color::Rgb(240, 110, 120);

pub(super) const BORDER: Color = Color::Rgb(48, 54, 66);

/// Cap on how much of a live/completed reasoning stream is shown inline in
/// the transcript. The full text is retained in `AppState` regardless (see
/// `AppState::fold_event`'s `ReasoningDelta` handling) — this only bounds
/// what gets rendered, so one very long thinking block can't push the rest
/// of the conversation off-screen.
pub(super) const MAX_REASONING_CHARS: usize = 2000;

/// Cap on how much of a tool result preview (including `shell.exec`
/// stdout/stderr) is shown inline in the transcript — long enough to read
/// a normal command's output in full, short enough that one very verbose
/// command can't dominate the scrollback.
pub(super) const MAX_TOOL_OUTPUT_CHARS: usize = 3000;
