use std::{io, mem};

use ratatui::{
    backend::{Backend, ClearType, WindowSize},
    buffer::Cell,
    layout::{Position, Rect, Size},
    style::{Color, Modifier},
};
use unicode_width::UnicodeWidthStr;

pub struct SshBackend {
    size: Rect,
    buf: Vec<u8>,
}

impl SshBackend {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            size: Rect::new(0, 0, cols, rows),
            buf: Vec::new(),
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.size = Rect::new(0, 0, cols, rows);
    }

    pub fn take(&mut self) -> Vec<u8> {
        mem::take(&mut self.buf)
    }

    fn write(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }
}

fn fg(color: Color) -> String {
    match color {
        Color::Reset => "\x1b[39m".into(),
        Color::Black => "\x1b[30m".into(),
        Color::Red => "\x1b[31m".into(),
        Color::Green => "\x1b[32m".into(),
        Color::Yellow => "\x1b[33m".into(),
        Color::Blue => "\x1b[34m".into(),
        Color::Magenta => "\x1b[35m".into(),
        Color::Cyan => "\x1b[36m".into(),
        Color::Gray => "\x1b[37m".into(),
        Color::DarkGray => "\x1b[90m".into(),
        Color::LightRed => "\x1b[91m".into(),
        Color::LightGreen => "\x1b[92m".into(),
        Color::LightYellow => "\x1b[93m".into(),
        Color::LightBlue => "\x1b[94m".into(),
        Color::LightMagenta => "\x1b[95m".into(),
        Color::LightCyan => "\x1b[96m".into(),
        Color::White => "\x1b[97m".into(),
        Color::Rgb(r, g, b) => format!("\x1b[38;2;{r};{g};{b}m"),
        Color::Indexed(i) => format!("\x1b[38;5;{i}m"),
    }
}

fn bg(color: Color) -> String {
    match color {
        Color::Reset => "\x1b[49m".into(),
        Color::Black => "\x1b[40m".into(),
        Color::Red => "\x1b[41m".into(),
        Color::Green => "\x1b[42m".into(),
        Color::Yellow => "\x1b[43m".into(),
        Color::Blue => "\x1b[44m".into(),
        Color::Magenta => "\x1b[45m".into(),
        Color::Cyan => "\x1b[46m".into(),
        Color::Gray => "\x1b[47m".into(),
        Color::DarkGray => "\x1b[100m".into(),
        Color::LightRed => "\x1b[101m".into(),
        Color::LightGreen => "\x1b[102m".into(),
        Color::LightYellow => "\x1b[103m".into(),
        Color::LightBlue => "\x1b[104m".into(),
        Color::LightMagenta => "\x1b[105m".into(),
        Color::LightCyan => "\x1b[106m".into(),
        Color::White => "\x1b[107m".into(),
        Color::Rgb(r, g, b) => format!("\x1b[48;2;{r};{g};{b}m"),
        Color::Indexed(i) => format!("\x1b[48;5;{i}m"),
    }
}

fn modifiers(m: Modifier) -> String {
    let mut s = String::new();
    if m.contains(Modifier::BOLD) {
        s.push_str("\x1b[1m");
    }
    if m.contains(Modifier::DIM) {
        s.push_str("\x1b[2m");
    }
    if m.contains(Modifier::ITALIC) {
        s.push_str("\x1b[3m");
    }
    if m.contains(Modifier::UNDERLINED) {
        s.push_str("\x1b[4m");
    }
    if m.intersects(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK) {
        s.push_str("\x1b[5m");
    }
    if m.contains(Modifier::REVERSED) {
        s.push_str("\x1b[7m");
    }
    if m.contains(Modifier::CROSSED_OUT) {
        s.push_str("\x1b[9m");
    }
    s
}

impl Backend for SshBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut cells: Vec<_> = content.collect();
        cells.sort_by_key(|(x, y, _)| (*y, *x));

        let mut cursor: Option<(u16, u16)> = None;
        for (x, y, cell) in cells {
            if cursor != Some((x, y)) {
                self.write(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes());
            }
            let symbol = cell.symbol();
            let style = format!(
                "\x1b[0m{}{}{}",
                fg(cell.fg),
                bg(cell.bg),
                modifiers(cell.modifier)
            );
            self.write(style.as_bytes());
            self.write(symbol.as_bytes());
            cursor = Some((x + symbol.width() as u16, y));
        }
        self.write(b"\x1b[0m");
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.write(b"\x1b[?25l");
        Ok(())
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.write(b"\x1b[?25h");
        Ok(())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        // Cannot query the cursor over SSH without a round-trip.
        Ok(Position::ORIGIN)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let p = position.into();
        self.write(format!("\x1b[{};{}H", p.y + 1, p.x + 1).as_bytes());
        Ok(())
    }

    fn clear(&mut self) -> io::Result<()> {
        self.write(b"\x1b[2J\x1b[H");
        Ok(())
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.write(match clear_type {
            ClearType::All => b"\x1b[2J".as_slice(),
            ClearType::AfterCursor => b"\x1b[J",
            ClearType::BeforeCursor => b"\x1b[1J",
            ClearType::CurrentLine => b"\x1b[2K",
            ClearType::UntilNewLine => b"\x1b[K",
        });
        Ok(())
    }

    fn size(&self) -> io::Result<Size> {
        Ok(self.size.as_size())
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.size.as_size(),
            pixels: Size::default(),
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
