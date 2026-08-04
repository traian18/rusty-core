//! Identical key mapping to `apps/harness/src/input.rs` — same keybindings
//! so anyone used to the standalone TUI feels at home here.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, PartialEq, Eq)]
pub enum InputAction {
    Insert(char),
    Backspace,
    Submit,
    Cancel,
    Approve,
    Reject,
    Quit,
    Ignore,
}

pub fn map_key(key: KeyEvent) -> InputAction {
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => InputAction::Cancel,
        (KeyCode::Char('q'), KeyModifiers::NONE) => InputAction::Quit,
        (KeyCode::Char('y'), KeyModifiers::NONE) => InputAction::Approve,
        (KeyCode::Char('n'), KeyModifiers::NONE) => InputAction::Reject,
        (KeyCode::Enter, _) => InputAction::Submit,
        (KeyCode::Backspace, _) => InputAction::Backspace,
        (KeyCode::Char(character), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            InputAction::Insert(character)
        }
        _ => InputAction::Ignore,
    }
}
