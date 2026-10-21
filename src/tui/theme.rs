//! Color themes: the color each part of the TUI is drawn in. `default`
//! keeps to the terminal's own 16 colors where it can; the rest are fixed
//! (24-bit) palettes, so they want a terminal with true color.

use crate::repl;
use ratatui::style::Color;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Theme {
    pub name: &'static str,
    /// Keys, the selection bar, and anything to act on.
    pub accent: Color,
    pub dir: Color,
    /// Marks, the clipboard buffer, warnings and the title chips.
    pub mark: Color,
    /// File entries (base64-stored files), told apart via the file index.
    pub file: Color,
    pub danger: Color,
    /// The passphrase and insert popups, so they stand apart from the rest.
    pub unlock: Color,
    /// Behind the title chips and the highlighted row.
    pub shade: Color,
    /// Behind the popup input fields; quieter than `shade`.
    pub field: Color,
    /// Pane borders.
    pub border: Color,
    /// The history graph's lanes, by column.
    pub lanes: [Color; 6],
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub const THEMES: &[Theme] = &[
    // Picked by eye on a dark terminal. 0-15 follow the terminal's theme;
    // indexed colors above that are fixed.
    Theme {
        name: "default",
        accent: Color::Indexed(214),
        dir: Color::Cyan,
        mark: Color::LightMagenta,
        file: Color::Magenta,
        danger: Color::Red,
        unlock: Color::LightRed,
        shade: Color::Indexed(236),
        field: Color::Indexed(234),
        border: Color::Indexed(242),
        lanes: [
            Color::Blue,
            Color::Magenta,
            Color::Cyan,
            Color::Yellow,
            Color::Green,
            Color::Red,
        ],
    },
    // Mocha.
    Theme {
        name: "catppuccin",
        accent: rgb(0xfab387),
        dir: rgb(0x89b4fa),
        mark: rgb(0xf5c2e7),
        file: rgb(0xcba6f7),
        danger: rgb(0xf38ba8),
        unlock: rgb(0xeba0ac),
        shade: rgb(0x313244),
        field: rgb(0x181825),
        border: rgb(0x6c7086),
        lanes: [
            rgb(0x89b4fa),
            rgb(0xcba6f7),
            rgb(0x94e2d5),
            rgb(0xf9e2af),
            rgb(0xa6e3a1),
            rgb(0xf38ba8),
        ],
    },
    Theme {
        name: "dracula",
        accent: rgb(0xffb86c),
        dir: rgb(0x8be9fd),
        mark: rgb(0xff79c6),
        file: rgb(0xbd93f9),
        danger: rgb(0xff5555),
        unlock: rgb(0xff6e6e),
        shade: rgb(0x44475a),
        field: rgb(0x21222c),
        border: rgb(0x6272a4),
        lanes: [
            rgb(0xbd93f9),
            rgb(0xff79c6),
            rgb(0x8be9fd),
            rgb(0xf1fa8c),
            rgb(0x50fa7b),
            rgb(0xff5555),
        ],
    },
    // Dark.
    Theme {
        name: "gruvbox",
        accent: rgb(0xfe8019),
        dir: rgb(0x83a598),
        mark: rgb(0xd3869b),
        file: rgb(0xb16286),
        danger: rgb(0xcc241d),
        unlock: rgb(0xfb4934),
        shade: rgb(0x3c3836),
        field: rgb(0x1d2021),
        border: rgb(0x7c6f64),
        lanes: [
            rgb(0x83a598),
            rgb(0xd3869b),
            rgb(0x8ec07c),
            rgb(0xfabd2f),
            rgb(0xb8bb26),
            rgb(0xfb4934),
        ],
    },
    Theme {
        name: "nord",
        accent: rgb(0xebcb8b),
        dir: rgb(0x88c0d0),
        mark: rgb(0xb48ead),
        file: rgb(0xa3be8c),
        danger: rgb(0xbf616a),
        unlock: rgb(0xd08770),
        shade: rgb(0x3b4252),
        field: rgb(0x242933),
        border: rgb(0x4c566a),
        lanes: [
            rgb(0x81a1c1),
            rgb(0xb48ead),
            rgb(0x88c0d0),
            rgb(0xebcb8b),
            rgb(0xa3be8c),
            rgb(0xbf616a),
        ],
    },
    Theme {
        name: "tokyo-night",
        accent: rgb(0xff9e64),
        dir: rgb(0x7aa2f7),
        mark: rgb(0xbb9af7),
        file: rgb(0x9d7cd8),
        danger: rgb(0xdb4b4b),
        unlock: rgb(0xf7768e),
        shade: rgb(0x292e42),
        field: rgb(0x16161e),
        border: rgb(0x545c7e),
        lanes: [
            rgb(0x7aa2f7),
            rgb(0xbb9af7),
            rgb(0x7dcfff),
            rgb(0xe0af68),
            rgb(0x9ece6a),
            rgb(0xf7768e),
        ],
    },
    // For a light terminal, in Solarized's accents.
    Theme {
        name: "light",
        accent: rgb(0x268bd2),
        dir: rgb(0x2aa198),
        mark: rgb(0xd33682),
        file: rgb(0x6c71c4),
        danger: rgb(0xdc322f),
        unlock: rgb(0xcb4b16),
        shade: rgb(0xdcdcdc),
        field: rgb(0xeeeeee),
        border: rgb(0xa8a8a8),
        lanes: [
            rgb(0x268bd2),
            rgb(0xd33682),
            rgb(0x2aa198),
            rgb(0xb58900),
            rgb(0x859900),
            rgb(0xdc322f),
        ],
    },
];

/// An index into `THEMES`.
static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// The theme the TUI draws in.
pub fn theme() -> &'static Theme {
    &THEMES[current()]
}

pub fn current() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

/// Draw in `THEMES[i]` from the next frame on.
pub fn set(i: usize) {
    CURRENT.store(i.min(THEMES.len() - 1), Ordering::Relaxed);
}

pub fn find(name: &str) -> Option<usize> {
    THEMES.iter().position(|t| t.name == name)
}

/// Draw in the theme picked last time, if there is one.
pub fn load() {
    let saved = repl::state_file("theme").and_then(|p| std::fs::read_to_string(p).ok());
    if let Some(i) = saved.as_deref().and_then(|s| find(s.trim())) {
        set(i);
    }
}

/// Remember the current theme for next time.
pub fn save() {
    if let Some(path) = repl::state_file("theme") {
        let _ = std::fs::write(path, format!("{}\n", theme().name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_words() {
        for (i, t) in THEMES.iter().enumerate() {
            assert_eq!(find(t.name), Some(i), "{} is named twice", t.name);
            assert!(!t.name.contains(char::is_whitespace));
        }
    }

    #[test]
    fn rgb_splits_the_channels() {
        assert_eq!(rgb(0x123456), Color::Rgb(0x12, 0x34, 0x56));
    }
}
