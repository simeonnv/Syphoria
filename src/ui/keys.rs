use std::fmt;

use termwiz::input::Modifiers;

use crate::app::{KeyCode, KeyEvent};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Alt(char),
    Ctrl(char),
    Up,
    Down,
    Left,
    Right,
    AltUp,
    AltDown,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
}

impl Key {
    pub fn new(event: &KeyEvent) -> Option<Self> {
        let has = |modifier| event.modifiers.contains(modifier);
        Some(match event.key {
            KeyCode::Char(c) if has(Modifiers::CTRL) => Key::Ctrl(c),
            KeyCode::Char(c) if has(Modifiers::ALT) => Key::Alt(c),
            KeyCode::Char(c) => Key::Char(c),
            KeyCode::UpArrow if has(Modifiers::ALT) => Key::AltUp,
            KeyCode::DownArrow if has(Modifiers::ALT) => Key::AltDown,
            KeyCode::UpArrow => Key::Up,
            KeyCode::DownArrow => Key::Down,
            KeyCode::LeftArrow => Key::Left,
            KeyCode::RightArrow => Key::Right,
            KeyCode::PageUp => Key::PageUp,
            KeyCode::PageDown => Key::PageDown,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            KeyCode::Enter => Key::Enter,
            KeyCode::Escape => Key::Esc,
            KeyCode::Tab if has(Modifiers::SHIFT) => Key::BackTab,
            KeyCode::Tab => Key::Tab,
            KeyCode::Backspace => Key::Backspace,
            _ => return None,
        })
    }

    const fn code(self) -> u32 {
        let (kind, c) = match self {
            Key::Char(c) => (0, c),
            Key::Alt(c) => (1, c),
            Key::Ctrl(c) => (2, c),
            Key::Up => (3, '\0'),
            Key::Down => (4, '\0'),
            Key::Left => (5, '\0'),
            Key::Right => (6, '\0'),
            Key::AltUp => (7, '\0'),
            Key::AltDown => (8, '\0'),
            Key::PageUp => (9, '\0'),
            Key::PageDown => (10, '\0'),
            Key::Home => (11, '\0'),
            Key::End => (12, '\0'),
            Key::Enter => (13, '\0'),
            Key::Esc => (14, '\0'),
            Key::Tab => (15, '\0'),
            Key::BackTab => (16, '\0'),
            Key::Backspace => (17, '\0'),
        };
        (kind << 21) | c as u32
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Key::Char(' ') => f.write_str("space"),
            Key::Char(c) => write!(f, "{c}"),
            Key::Alt(c) => write!(f, "alt-{c}"),
            Key::Ctrl(c) => write!(f, "ctrl-{c}"),
            Key::Up => f.write_str("↑"),
            Key::Down => f.write_str("↓"),
            Key::Left => f.write_str("←"),
            Key::Right => f.write_str("→"),
            Key::AltUp => f.write_str("alt-↑"),
            Key::AltDown => f.write_str("alt-↓"),
            Key::PageUp => f.write_str("pgup"),
            Key::PageDown => f.write_str("pgdn"),
            Key::Home => f.write_str("home"),
            Key::End => f.write_str("end"),
            Key::Enter => f.write_str("enter"),
            Key::Esc => f.write_str("esc"),
            Key::Tab => f.write_str("tab"),
            Key::BackTab => f.write_str("shift-tab"),
            Key::Backspace => f.write_str("backspace"),
        }
    }
}

pub struct Bind {
    pub keys: &'static [Key],
    pub help: &'static str,
    conflict: &'static str,
}

impl Bind {
    pub fn label(&self) -> String {
        self.keys
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("/")
    }
}

pub type Help = &'static [&'static [Bind]];

pub fn line(help: Help) -> String {
    help.iter()
        .flat_map(|binds| binds.iter())
        .map(|bind| format!("{} {}", bind.label(), bind.help))
        .collect::<Vec<_>>()
        .join(" · ")
}

const MAX_KEYS: usize = 64;

const fn disjoint(scopes: &[&[Bind]]) {
    let mut keys = [(Key::Esc, ""); MAX_KEYS];
    let mut len = 0;
    let mut s = 0;
    while s < scopes.len() {
        let mut b = 0;
        while b < scopes[s].len() {
            let bind = &scopes[s][b];
            let mut k = 0;
            while k < bind.keys.len() {
                assert!(len < MAX_KEYS, "too many keys in one layer");
                keys[len] = (bind.keys[k], bind.conflict);
                len += 1;
                k += 1;
            }
            b += 1;
        }
        s += 1;
    }
    let mut i = 0;
    while i < len {
        let mut j = i + 1;
        while j < len {
            if keys[i].0.code() == keys[j].0.code() {
                panic!("{}", keys[j].1);
            }
            j += 1;
        }
        i += 1;
    }
}

macro_rules! keymap {
    ($($scope:ident { $($cmd:ident: $($key:ident $(($arg:literal))?)|+ => $help:literal,)+ })+) => {$(
        #[derive(Clone, Copy, PartialEq, Eq)]
        pub enum $scope {
            $($cmd,)+
        }

        impl $scope {
            pub const KEYS: &[Bind] = &[$(Bind {
                keys: &[$(Key::$key $(($arg))?),+],
                help: $help,
                conflict: concat!("key conflict at ", stringify!($scope), "::", stringify!($cmd)),
            }),+];

            pub fn get(event: &KeyEvent) -> Option<Self> {
                match Key::new(event)? {
                    $($(Key::$key $(($arg))?)|+ => Some(Self::$cmd),)+
                    _ => None,
                }
            }
        }

        const _: () = disjoint(&[$scope::KEYS]);
    )+};
}

keymap! {
    Always {
        SidebarNext: AltDown => "next sidebar entry",
        SidebarPrev: AltUp => "previous sidebar entry",
    }
    Global {
        Command: Char(':') => "command",
        Search: Char('/') => "search",
        Goto: Char('g') => "goto",
        Space: Char(' ') => "space menu",
        NextMatch: Char('n') => "next match",
        PrevMatch: Char('N') => "previous match",
    }
    Goto {
        Top: Char('g') => "first item",
        Bottom: Char('e') => "last item",
        Inbox: Char('i') => "inbox",
        Drafts: Char('d') => "drafts",
        Sent: Char('s') => "sent",
        Archive: Char('a') => "archive",
        Junk: Char('j') => "spam",
        Trash: Char('t') => "trash",
        Next: Char('n') => "next sidebar entry",
        Prev: Char('p') => "previous sidebar entry",
    }
    Space {
        Compose: Char('c') => "compose",
        Addresses: Char('a') => "addresses",
        Vault: Char('v') => "vault",
        Lock: Char('l') => "lock vault",
        Help: Char('?') => "keymap",
    }
    Prompt {
        Cancel: Esc => "cancel",
        Erase: Backspace => "erase",
        Submit: Enter => "run",
    }
    Connecting {
        Quit: Char('q') | Esc => "quit",
    }
    Confirm {
        Yes: Char('y') => "yes",
    }
    Nav {
        Down: Char('j') | Down => "down",
        Up: Char('k') | Up => "up",
        Top: Home => "top",
        Bottom: End => "bottom",
        PageDown: PageDown => "page down",
        PageUp: PageUp => "page up",
    }
    MoveTo {
        Inbox: Char('i') => "inbox",
        Archive: Char('a') => "archive",
        Junk: Char('j') => "spam",
        Trash: Char('t') => "trash",
    }
    Mail {
        Trash: Char('d') => "trash",
        Archive: Char('a') => "archive",
        Junk: Char('!') => "spam / restore",
        Delete: Alt('d') => "delete",
        Move: Char('m') => "move",
        Unread: Char('u') => "unread",
        YankSender: Char('y') => "yank sender",
        YankSubject: Char('Y') => "yank subject",
        Back: Esc => "back",
    }
    Messages {
        Open: Enter => "open",
        Read: Char('U') => "read",
        Undo: Char('z') => "undo",
        Reload: Char('r') => "reload",
    }
    SearchInput {
        Run: Enter => "search",
        Complete: Tab => "complete",
        Next: Down | Ctrl('n') => "next suggestion",
        Prev: Up | Ctrl('p') => "previous suggestion",
        Results: Esc => "results",
    }
    SearchResults {
        Open: Enter => "open",
        Read: Char('U') => "read",
        Reload: Char('r') => "reload",
    }
    Reader {
        Reply: Char('r') => "reply",
        ReplyAll: Char('R') => "reply all",
        Forward: Char('f') => "forward",
        YankBody: Alt('y') => "yank body",
    }
    Addresses {
        Add: Char('a') => "add",
        Yank: Char('y') => "yank",
        Reload: Char('r') => "reload",
        Back: Esc => "back",
    }
    NewAddress {
        Create: Enter => "create",
        Next: Tab => "next domain",
        Prev: BackTab => "previous domain",
        Random: Ctrl('r') => "random",
        Cancel: Esc => "cancel",
    }
    Unlock {
        Submit: Enter => "submit",
        Clear: Ctrl('u') => "clear",
        Back: Esc => "back",
    }
    Vault {
        Show: Enter => "show",
        Add: Char('a') => "add",
        Edit: Char('e') => "edit",
        Delete: Char('d') => "delete",
        YankPassword: Char('y') => "yank password",
        YankUsername: Char('Y') => "yank username",
        Reload: Char('r') => "reload",
        Back: Esc => "back",
    }
    Account {
        Edit: Char('e') => "edit",
        YankPassword: Char('y') => "yank password",
        YankUsername: Char('Y') => "yank username",
        Back: Esc => "back",
    }
    Fields {
        Submit: Ctrl('s') => "send / save",
        Next: Tab => "next field",
        Prev: BackTab => "previous field",
        Left: Left => "previous option",
        Right: Right => "next option",
    }
    Edit {
        Insert: Char('i') => "insert",
        Down: Char('j') | Down => "next field",
        Up: Char('k') | Up => "previous field",
        Left: Char('h') => "previous option",
        Right: Char('l') => "next option",
        Yank: Char('y') => "yank field",
        Back: Esc => "back",
    }
    Insert {
        Normal: Esc => "normal mode",
        Next: Enter => "next field",
    }
    Password {
        Generate: Ctrl('r') => "generate password",
    }
}

const _: () = {
    disjoint(&[
        Always::KEYS,
        Global::KEYS,
        Nav::KEYS,
        Mail::KEYS,
        Messages::KEYS,
    ]);
    disjoint(&[
        Always::KEYS,
        Global::KEYS,
        Nav::KEYS,
        Mail::KEYS,
        Reader::KEYS,
    ]);
    disjoint(&[
        Always::KEYS,
        Global::KEYS,
        Nav::KEYS,
        Mail::KEYS,
        SearchResults::KEYS,
    ]);
    disjoint(&[Always::KEYS, SearchInput::KEYS]);
    disjoint(&[Always::KEYS, Global::KEYS, Nav::KEYS, Addresses::KEYS]);
    disjoint(&[Always::KEYS, NewAddress::KEYS]);
    disjoint(&[Always::KEYS, Unlock::KEYS]);
    disjoint(&[Always::KEYS, Global::KEYS, Nav::KEYS, Vault::KEYS]);
    disjoint(&[Always::KEYS, Global::KEYS, Account::KEYS]);
    disjoint(&[
        Always::KEYS,
        Global::KEYS,
        Fields::KEYS,
        Edit::KEYS,
        Password::KEYS,
    ]);
    disjoint(&[Always::KEYS, Fields::KEYS, Insert::KEYS, Password::KEYS]);
};
