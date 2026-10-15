use std::{
    collections::{HashMap, HashSet},
    io, mem,
};

use ratatui::{
    backend::{Backend, ClearType, WindowSize},
    buffer::{Buffer, Cell},
    layout::{Position, Rect, Size},
    style::{Color, Modifier},
};
use termwiz::{
    escape::osc::{OperatingSystemCommand, Selection},
    hyperlink::Hyperlink,
};
use unicode_width::UnicodeWidthStr;
use zeroize::{Zeroize, Zeroizing};

const UNLINK: &str = "\x1b]8;;\x1b\\";

pub struct Link {
    pub url: String,
    pub cells: Vec<Position>,
}

pub struct SshBackend {
    size: Rect,
    buf: Vec<u8>,
    screen: Buffer,
    hrefs: Vec<String>,
    links: HashMap<Position, usize>,
}

impl SshBackend {
    pub fn new(cols: u16, rows: u16) -> Self {
        let size = Rect::new(0, 0, cols, rows);
        Self {
            size,
            buf: Vec::new(),
            screen: Buffer::empty(size),
            hrefs: Vec::new(),
            links: HashMap::new(),
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.size = Rect::new(0, 0, cols, rows);
        self.screen = Buffer::empty(self.size);
    }

    pub fn set_links(&mut self, links: Vec<Link>) {
        let mut hrefs = Vec::new();
        let mut cells = HashMap::new();
        for (i, link) in links.into_iter().enumerate() {
            let href = Hyperlink::new_with_id(link.url, i.to_string());
            hrefs.push(OperatingSystemCommand::SetHyperlink(Some(href)).to_string());
            cells.extend(link.cells.into_iter().map(|p| (p, i)));
        }
        let changed: HashSet<Position> = self
            .links
            .keys()
            .chain(cells.keys())
            .filter(|p| href(&self.hrefs, &self.links, p) != href(&hrefs, &cells, p))
            .copied()
            .collect();
        self.hrefs = hrefs;
        self.links = cells;
        if changed.is_empty() {
            return;
        }
        let cells = changed
            .into_iter()
            .filter_map(|p| self.screen.cell(p).map(|c| (p.x, p.y, c.clone())))
            .collect();
        self.write(b"\x1b7");
        self.paint(cells);
        self.write(b"\x1b8");
    }

    fn paint(&mut self, mut cells: Vec<(u16, u16, Cell)>) {
        cells.sort_by_key(|(x, y, _)| (*y, *x));

        let mut cursor: Option<(u16, u16)> = None;
        let mut open: Option<usize> = None;
        for (x, y, cell) in cells {
            let link = self.links.get(&Position::new(x, y)).copied();
            if link != open {
                let osc = link.map_or(UNLINK, |i| &self.hrefs[i]);
                self.buf.extend_from_slice(osc.as_bytes());
                open = link;
            }
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
        if open.is_some() {
            self.write(UNLINK.as_bytes());
        }
        self.write(b"\x1b[0m");
    }

    pub fn take(&mut self) -> Vec<u8> {
        mem::take(&mut self.buf)
    }

    pub fn clipboard(&mut self, text: &str) {
        let osc = OperatingSystemCommand::SetSelection(Selection::CLIPBOARD, text.to_owned());
        let bytes = Zeroizing::new(osc.to_string());
        if let OperatingSystemCommand::SetSelection(_, mut text) = osc {
            text.zeroize();
        }
        self.write(bytes.as_bytes());
    }

    fn write(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }
}

fn href<'a>(
    hrefs: &'a [String],
    links: &HashMap<Position, usize>,
    p: &Position,
) -> Option<&'a str> {
    links.get(p).map(|&i| hrefs[i].as_str())
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
        let cells: Vec<_> = content.map(|(x, y, c)| (x, y, c.clone())).collect();
        for (x, y, cell) in &cells {
            if let Some(shown) = self.screen.cell_mut((*x, *y)) {
                *shown = cell.clone();
            }
        }
        self.paint(cells);
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
        Ok(Position::ORIGIN)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let p = position.into();
        self.write(format!("\x1b[{};{}H", p.y + 1, p.x + 1).as_bytes());
        Ok(())
    }

    fn clear(&mut self) -> io::Result<()> {
        self.screen.reset();
        self.write(b"\x1b[2J\x1b[H");
        Ok(())
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        if clear_type == ClearType::All {
            self.screen.reset();
        }
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
