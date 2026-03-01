//! Key event mapping to app actions.

use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;

/// Actions that can be triggered by user input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Move selection up (↑/k — vertical navigation on Screen 2).
    Up,
    /// Move selection down (↓/j — vertical navigation on Screen 2).
    Down,
    /// Move selection left (←/h — horizontal navigation on Screen 1).
    Left,
    /// Move selection right (→/l — horizontal navigation on Screen 1).
    Right,
    /// Confirm current selection / enter.
    Select,
    /// Toggle checkbox (bookmark assignment screen).
    Toggle,
    /// Cancel / go back.
    Cancel,
    /// Quit immediately (Ctrl-C).
    Quit,
    /// No action for this event.
    None,
}

/// Map a crossterm event to an app action.
pub fn map_event(event: &Event) -> Action {
    match event {
        Event::Key(KeyEvent {
            code, modifiers, ..
        }) => map_key(*code, *modifiers),
        _ => Action::None,
    }
}

fn map_key(code: KeyCode, modifiers: KeyModifiers) -> Action {
    // Ctrl-C always quits immediately.
    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        return Action::Quit;
    }

    match code {
        KeyCode::Up | KeyCode::Char('k') => Action::Up,
        KeyCode::Down | KeyCode::Char('j') => Action::Down,
        KeyCode::Left | KeyCode::Char('h') => Action::Left,
        KeyCode::Right | KeyCode::Char('l') => Action::Right,
        KeyCode::Enter => Action::Select,
        KeyCode::Char(' ') => Action::Toggle,
        KeyCode::Esc | KeyCode::Char('q') => Action::Cancel,
        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEventKind;
    use crossterm::event::KeyEventState;

    use super::*;

    fn key_event(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    fn key_event_ctrl(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn arrow_keys() {
        assert_eq!(map_event(&key_event(KeyCode::Up)), Action::Up);
        assert_eq!(map_event(&key_event(KeyCode::Down)), Action::Down);
        assert_eq!(map_event(&key_event(KeyCode::Left)), Action::Left);
        assert_eq!(map_event(&key_event(KeyCode::Right)), Action::Right);
    }

    #[test]
    fn vim_keys() {
        assert_eq!(map_event(&key_event(KeyCode::Char('k'))), Action::Up);
        assert_eq!(map_event(&key_event(KeyCode::Char('j'))), Action::Down);
        assert_eq!(map_event(&key_event(KeyCode::Char('h'))), Action::Left);
        assert_eq!(map_event(&key_event(KeyCode::Char('l'))), Action::Right);
    }

    #[test]
    fn enter_and_space() {
        assert_eq!(map_event(&key_event(KeyCode::Enter)), Action::Select);
        assert_eq!(map_event(&key_event(KeyCode::Char(' '))), Action::Toggle);
    }

    #[test]
    fn cancel_keys() {
        assert_eq!(map_event(&key_event(KeyCode::Esc)), Action::Cancel);
        assert_eq!(map_event(&key_event(KeyCode::Char('q'))), Action::Cancel);
        assert_eq!(map_event(&key_event_ctrl(KeyCode::Char('c'))), Action::Quit);
    }

    #[test]
    fn unknown_key_is_none() {
        assert_eq!(map_event(&key_event(KeyCode::Char('x'))), Action::None);
    }
}
