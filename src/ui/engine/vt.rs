//! Pure std ANSI / VT100 escape sequence generator and terminal control.
//! Based on microsoft/edit architecture (zero external dependencies).

use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Reset,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
    Rgb(u8, u8, u8),
    Indexed(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}

impl Style {
    pub const fn new() -> Self {
        Self {
            fg: None,
            bg: None,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            reverse: false,
        }
    }

    pub fn fg(mut self, color: Color) -> Self {
        self.fg = Some(color);
        self
    }

    pub fn bg(mut self, color: Color) -> Self {
        self.bg = Some(color);
        self
    }

    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub fn dim(mut self) -> Self {
        self.dim = true;
        self
    }

    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    pub fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    pub fn reverse(mut self) -> Self {
        self.reverse = true;
        self
    }
}

pub struct Vt;

impl Vt {
    pub fn enter_alternate_screen(out: &mut String) {
        out.push_str("\x1b[?1049h");
    }

    pub fn leave_alternate_screen(out: &mut String) {
        out.push_str("\x1b[?1049l");
    }

    pub fn hide_cursor(out: &mut String) {
        out.push_str("\x1b[?25l");
    }

    pub fn show_cursor(out: &mut String) {
        out.push_str("\x1b[?25h");
    }

    pub fn move_cursor(out: &mut String, col: u16, row: u16) {
        let _ = write!(out, "\x1b[{};{}H", row + 1, col + 1);
    }

    pub fn clear_screen(out: &mut String) {
        out.push_str("\x1b[2J\x1b[H");
    }

    pub fn reset_style(out: &mut String) {
        out.push_str("\x1b[0m");
    }

    pub fn apply_style(out: &mut String, style: &Style) {
        out.push_str("\x1b[0");
        if style.bold {
            out.push_str(";1");
        }
        if style.dim {
            out.push_str(";2");
        }
        if style.italic {
            out.push_str(";3");
        }
        if style.underline {
            out.push_str(";4");
        }
        if style.reverse {
            out.push_str(";7");
        }

        if let Some(fg) = style.fg {
            match fg {
                Color::Reset => out.push_str(";39"),
                Color::Black => out.push_str(";30"),
                Color::Red => out.push_str(";31"),
                Color::Green => out.push_str(";32"),
                Color::Yellow => out.push_str(";33"),
                Color::Blue => out.push_str(";34"),
                Color::Magenta => out.push_str(";35"),
                Color::Cyan => out.push_str(";36"),
                Color::White => out.push_str(";37"),
                Color::BrightBlack => out.push_str(";90"),
                Color::BrightRed => out.push_str(";91"),
                Color::BrightGreen => out.push_str(";92"),
                Color::BrightYellow => out.push_str(";93"),
                Color::BrightBlue => out.push_str(";94"),
                Color::BrightMagenta => out.push_str(";95"),
                Color::BrightCyan => out.push_str(";96"),
                Color::BrightWhite => out.push_str(";97"),
                Color::Indexed(i) => {
                    let _ = write!(out, ";38;5;{}", i);
                }
                Color::Rgb(r, g, b) => {
                    let _ = write!(out, ";38;2;{};{};{}", r, g, b);
                }
            }
        }

        if let Some(bg) = style.bg {
            match bg {
                Color::Reset => out.push_str(";49"),
                Color::Black => out.push_str(";40"),
                Color::Red => out.push_str(";41"),
                Color::Green => out.push_str(";42"),
                Color::Yellow => out.push_str(";43"),
                Color::Blue => out.push_str(";44"),
                Color::Magenta => out.push_str(";45"),
                Color::Cyan => out.push_str(";46"),
                Color::White => out.push_str(";47"),
                Color::BrightBlack => out.push_str(";100"),
                Color::BrightRed => out.push_str(";101"),
                Color::BrightGreen => out.push_str(";102"),
                Color::BrightYellow => out.push_str(";103"),
                Color::BrightBlue => out.push_str(";104"),
                Color::BrightMagenta => out.push_str(";105"),
                Color::BrightCyan => out.push_str(";106"),
                Color::BrightWhite => out.push_str(";107"),
                Color::Indexed(i) => {
                    let _ = write!(out, ";48;5;{}", i);
                }
                Color::Rgb(r, g, b) => {
                    let _ = write!(out, ";48;2;{};{};{}", r, g, b);
                }
            }
        }

        out.push('m');
    }
}
