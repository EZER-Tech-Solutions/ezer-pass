//! The `g` form: name, type, length and look-alikes for a new password.

use super::app::Input;
use crate::password::{DEFAULT_WORDS, Kind, Spec};
use crate::repl;

pub const MAX_LENGTH: u32 = 4096;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Kind,
    Length,
    LookAlikes,
}

/// The fields, top to bottom.
const FIELDS: [Field; 4] = [Field::Name, Field::Kind, Field::Length, Field::LookAlikes];

pub struct Form {
    /// The folder the name is relative to.
    pub folder: String,
    pub name: Input,
    /// An index into `Kind::ALL`.
    pub kind: usize,
    /// The length in characters, for every type but a passphrase.
    pub length: Input,
    /// The length in words, for a passphrase.
    pub words: Input,
    pub no_look_alikes: bool,
    pub field: Field,
    pub error: Option<String>,
    /// The entry exists and ⏎ was pressed once: the next ⏎ replaces it.
    pub overwrite: bool,
}

impl Form {
    /// A form for a new password in `folder`, with the settings used last
    /// time.
    pub fn new(folder: String) -> Self {
        let (kind, length, no_look_alikes, words) = load_prefs();
        Form {
            folder,
            name: Input::default(),
            kind,
            length: Input::new(length.to_string()),
            words: Input::new(words.to_string()),
            no_look_alikes,
            field: Field::Name,
            error: None,
            overwrite: false,
        }
    }

    pub fn next_field(&mut self, back: bool) {
        let n = FIELDS.len();
        let i = FIELDS.iter().position(|f| *f == self.field).unwrap_or(0);
        self.field = FIELDS[if back { (i + n - 1) % n } else { (i + 1) % n }];
    }

    pub fn cycle_kind(&mut self, back: bool) {
        let n = Kind::ALL.len();
        self.kind = if back {
            (self.kind + n - 1) % n
        } else {
            (self.kind + 1) % n
        };
    }

    fn passphrase(&self) -> bool {
        Kind::ALL[self.kind] == Kind::Passphrase
    }

    /// The length field the type uses.
    pub fn length_input(&self) -> &Input {
        if self.passphrase() {
            &self.words
        } else {
            &self.length
        }
    }

    pub fn length_input_mut(&mut self) -> &mut Input {
        if self.passphrase() {
            &mut self.words
        } else {
            &mut self.length
        }
    }

    pub fn length(&self) -> Option<u32> {
        self.length_input()
            .text
            .parse()
            .ok()
            .filter(|n| (1..=MAX_LENGTH).contains(n))
    }

    pub fn nudge_length(&mut self, delta: i64) {
        let input = self.length_input_mut();
        let n = input.text.parse::<i64>().unwrap_or(0) + delta;
        *input = Input::new(n.clamp(1, MAX_LENGTH as i64).to_string());
    }

    /// What the password is drawn from.
    pub fn spec(&self) -> Spec {
        Spec {
            no_look_alikes: self.no_look_alikes,
            ..Spec::new(Kind::ALL[self.kind])
        }
    }

    pub fn save_prefs(&self) {
        let valid = |input: &Input| {
            input
                .text
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=MAX_LENGTH).contains(n))
        };
        if let (Some(path), Some(length), Some(words)) = (
            repl::state_file("generate"),
            valid(&self.length),
            valid(&self.words),
        ) {
            let look_alikes = if self.no_look_alikes {
                "no-ambiguous"
            } else {
                ""
            };
            let _ = std::fs::write(
                path,
                format!(
                    "{}\n{length}\n{look_alikes}\n{words}\n",
                    Kind::ALL[self.kind].name()
                ),
            );
        }
    }
}

/// The last type, length, look-alike setting and word count used, else
/// symbols, the default length, look-alikes allowed and the default word
/// count.
fn load_prefs() -> (usize, u32, bool, u32) {
    let default_length = crate::commands::default_length();
    let saved = repl::state_file("generate").and_then(|p| std::fs::read_to_string(p).ok());
    let mut lines = saved.as_deref().unwrap_or("").lines();
    let kind = lines
        .next()
        .and_then(|k| Kind::ALL.iter().position(|kind| kind.name() == k))
        .unwrap_or(0);
    let length = lines
        .next()
        .and_then(|l| l.parse().ok())
        .filter(|n| (1..=MAX_LENGTH).contains(n))
        .unwrap_or(default_length);
    let no_look_alikes = lines.next() == Some("no-ambiguous");
    let words = lines
        .next()
        .and_then(|l| l.parse().ok())
        .filter(|n| (1..=MAX_LENGTH).contains(n))
        .unwrap_or(DEFAULT_WORDS);
    (kind, length, no_look_alikes, words)
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
            words: Input::new("6".into()),
            no_look_alikes: false,
            field: Field::Name,
            error: None,
            overwrite: false,
        };
        f.next_field(false);
        assert!(f.field == Field::Kind);
        f.next_field(true);
        assert!(f.field == Field::Name);
        f.next_field(true);
        assert!(f.field == Field::LookAlikes);
        f.next_field(true);
        assert!(f.field == Field::Length);
        f.cycle_kind(true);
        assert_eq!(f.kind, Kind::ALL.len() - 1);
        f.nudge_length(-100);
        assert_eq!(f.length(), Some(1));
        // Back to symbols, then round to a passphrase: words count apart.
        f.cycle_kind(false);
        assert_eq!(f.length(), Some(25));
        f.cycle_kind(true);
        assert_eq!(f.length(), Some(1));
        assert_eq!(f.length.text, "25");
    }
}
