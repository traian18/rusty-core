use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, PartialEq, Eq)]
pub enum InputAction {
    Insert(char),
    Newline,
    Backspace,
    Submit,
    Cancel,
    Approve,
    Reject,
    ScrollUp,
    ScrollDown,
    Follow,
    OpenCommands,
    ToggleInspector,
    NewSession,
    PreviousSession,
    NextSession,
    NavigateUp,
    NavigateDown,
    Confirm,
    Quit,
    Ignore,
}

pub fn map_key(key: KeyEvent, permission_prompt: bool, modal_open: bool) -> InputAction {
    if modal_open {
        return match (key.code, key.modifiers) {
            (KeyCode::Char('q'), KeyModifiers::CONTROL) => InputAction::Quit,
            (KeyCode::Esc, _) => InputAction::Cancel,
            (KeyCode::Up, _) => InputAction::NavigateUp,
            (KeyCode::Down, _) => InputAction::NavigateDown,
            (KeyCode::Enter, _) => InputAction::Confirm,
            (KeyCode::Backspace, _) => InputAction::Backspace,
            (KeyCode::Char(character), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                InputAction::Insert(character)
            }
            _ => InputAction::Ignore,
        };
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), KeyModifiers::CONTROL) => InputAction::Quit,
        (KeyCode::Char('p'), KeyModifiers::CONTROL) => InputAction::OpenCommands,
        (KeyCode::Char('i'), KeyModifiers::CONTROL) => InputAction::ToggleInspector,
        (KeyCode::Up, KeyModifiers::CONTROL) => InputAction::PreviousSession,
        (KeyCode::Down, KeyModifiers::CONTROL) => InputAction::NextSession,
        (KeyCode::Char('n'), KeyModifiers::CONTROL) if permission_prompt => InputAction::Reject,
        (KeyCode::Char('n'), KeyModifiers::CONTROL) => InputAction::NewSession,
        (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => InputAction::Cancel,
        (KeyCode::Enter, KeyModifiers::ALT) | (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
            InputAction::Newline
        }
        (KeyCode::Enter, KeyModifiers::NONE) => InputAction::Submit,
        (KeyCode::Backspace, _) => InputAction::Backspace,
        (KeyCode::PageUp, _) => InputAction::ScrollUp,
        (KeyCode::PageDown, _) => InputAction::ScrollDown,
        (KeyCode::Char('G'), KeyModifiers::SHIFT) => InputAction::Follow,
        (KeyCode::Char('y'), KeyModifiers::CONTROL) if permission_prompt => InputAction::Approve,
        (KeyCode::Char(character), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            InputAction::Insert(character)
        }
        _ => InputAction::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_text_is_never_a_global_command() {
        for character in ['q', 'y', 'n'] {
            assert_eq!(
                map_key(
                    KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
                    true,
                    false,
                ),
                InputAction::Insert(character)
            );
        }
    }

    #[test]
    fn maps_multiline_and_terminal_controls() {
        assert_eq!(
            map_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                false,
                false,
            ),
            InputAction::Submit
        );
        assert_eq!(
            map_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
                false,
                false,
            ),
            InputAction::Newline
        );
        assert_eq!(
            map_key(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                false,
                false,
            ),
            InputAction::Quit
        );
    }

    #[test]
    fn permission_shortcut_requires_a_pending_prompt() {
        let key = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert_eq!(map_key(key, true, false), InputAction::Approve);
        assert_eq!(map_key(key, false, false), InputAction::Ignore);
    }

    #[test]
    fn modal_keys_are_isolated_from_composer_shortcuts() {
        assert_eq!(
            map_key(
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                false,
                true,
            ),
            InputAction::NavigateDown
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), false, true,),
            InputAction::Cancel
        );
        assert_eq!(
            map_key(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                false,
                true,
            ),
            InputAction::Quit
        );
    }
}
