//! The `g` form: name, type and length for a new password.

use super::app::Input;
use crate::repl;

/// The password types, as pass flags (charsets mapped in `commands`).
pub const KINDS: &[(&str, Option<&str>)] = &[
    ("symbols", None),
    ("letters & digits", Some("-n")),
    ("url-safe", Some("--url-safe")),
    ("hex", Some("--hex")),
    ("digits", Some("--digits")),
    ("base-64", Some("--base-64")),
];

pub const MAX_LENGTH: u32 = 4096;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Kind,
    Length,
}

pub struct Form {
    /// The folder the name is relative to.
    pub folder: String,
    pub name: Input,
    pub kind: usize,
    pub length: Input,
    pub field: Field,
    pub error: Option<String>,
    /// The entry exists and ⏎ was pressed once: the next ⏎ replaces it.
    pub overwrite: bool,
}

impl Form {
    /// A form for a new password in `folder`, with the type and length
    /// used last time.
    pub fn new(folder: String) -> Self {
        let (kind, length) = load_prefs();
        Form {
            folder,
            name: Input::default(),
            kind,
            length: Input::new(length.to_string()),
            field: Field::Name,
            error: None,
            overwrite: false,
        }
    }

    pub fn next_field(&mut self, back: bool) {
        self.field = match (self.field, back) {
            (Field::Name, false) | (Field::Length, true) => Field::Kind,
            (Field::Kind, false) | (Field::Name, true) => Field::Length,
            (Field::Length, false) | (Field::Kind, true) => Field::Name,
        };
    }

    pub fn cycle_kind(&mut self, back: bool) {
        let n = KINDS.len();
        self.kind = if back {
            (self.kind + n - 1) % n
        } else {
            (self.kind + 1) % n
        };
    }

    pub fn length(&self) -> Option<u32> {
        self.length
            .text
            .parse()
            .ok()
            .filter(|n| (1..=MAX_LENGTH).contains(n))
    }

    pub fn nudge_length(&mut self, delta: i64) {
        let n = self.length.text.parse::<i64>().unwrap_or(0) + delta;
        self.length = Input::new(n.clamp(1, MAX_LENGTH as i64).to_string());
    }

    pub fn save_prefs(&self) {
        if let (Some(path), Some(length)) = (repl::state_file("generate"), self.length()) {
            let _ = std::fs::write(path, format!("{}\n{length}\n", KINDS[self.kind].0));
        }
    }
}

/// The last type and length used, else "symbols" and pass's default length.
fn load_prefs() -> (usize, u32) {
    let default_length = std::env::var("PASSWORD_STORE_GENERATED_LENGTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25);
    let saved = repl::state_file("generate").and_then(|p| std::fs::read_to_string(p).ok());
    let mut lines = saved.as_deref().unwrap_or("").lines();
    let kind = lines
        .next()
        .and_then(|k| KINDS.iter().position(|(name, _)| *name == k))
        .unwrap_or(0);
    let length = lines
        .next()
        .and_then(|l| l.parse().ok())
        .filter(|n| (1..=MAX_LENGTH).contains(n))
        .unwrap_or(default_length);
    (kind, length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_cycle_both_ways() {
        let mut f = Form {
            folder: String::new(),
            name: Input::default(),
            kind: 0,
            length: Input::new("25".into()),
            field: Field::Name,
            error: None,
            overwrite: false,
        };
        f.next_field(false);
        assert!(f.field == Field::Kind);
        f.next_field(true);
        assert!(f.field == Field::Name);
        f.next_field(true);
        assert!(f.field == Field::Length);
        f.cycle_kind(true);
        assert_eq!(f.kind, KINDS.len() - 1);
        f.nudge_length(-100);
        assert_eq!(f.length(), Some(1));
    }
}
