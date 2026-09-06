//! Pure std terminal event models and raw-mode input parser.

use std::time::Duration;

pub use super::sys::{DisableMouseCapture, EnableMouseCapture};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Delete,
    F(u8),
    Null,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyModifiers(pub u8);

impl KeyModifiers {
    pub const NONE: KeyModifiers = KeyModifiers(0);
    pub const SHIFT: KeyModifiers = KeyModifiers(1);
    pub const CONTROL: KeyModifiers = KeyModifiers(2);
    pub const ALT: KeyModifiers = KeyModifiers(4);

    pub fn contains(&self, other: KeyModifiers) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for KeyModifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        KeyModifiers(self.0 | rhs.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEventKind {
    Press,
    Release,
    Repeat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEventState(pub u8);

impl KeyEventState {
    pub const NONE: KeyEventState = KeyEventState(0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    pub kind: KeyEventKind,
    pub state: KeyEventState,
}

impl KeyEvent {
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEventKind {
    Down(MouseButton),
    Up(MouseButton),
    Drag(MouseButton),
    Moved,
    ScrollDown,
    ScrollUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseEventKind,
    pub column: u16,
    pub row: u16,
    pub modifiers: KeyModifiers,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
}

pub fn poll(_timeout: Duration) -> std::io::Result<bool> {
    Ok(false)
}

pub fn read() -> std::io::Result<Event> {
    use std::io::Read;
    let mut stdin = std::io::stdin();
    let mut byte = [0u8; 1];
    let n = stdin.read(&mut byte)?;
    if n == 0 {
        return Ok(Event::Key(KeyEvent::new(KeyCode::Null, KeyModifiers::NONE)));
    }

    match byte[0] {
        b'\r' | b'\n' => Ok(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))),
        b'\t' => Ok(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))),
        27 => Ok(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
        127 | 8 => Ok(Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))),
        b if b < 32 => {
            let ch = (b + b'a' - 1) as char;
            Ok(Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)))
        }
        b => Ok(Event::Key(KeyEvent::new(KeyCode::Char(b as char), KeyModifiers::NONE))),
    }
}
