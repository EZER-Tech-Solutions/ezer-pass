//! The `g` form: name, type, length and look-alikes for a new password.

use super::app::Input;
use crate::password::{Kind, Spec};
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
    pub length: Input,
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
        let (kind, length, no_look_alikes) = load_prefs();
        Form {
            folder,
            name: Input::default(),
            kind,
            length: Input::new(length.to_string()),
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

    /// What the password is drawn from.
    pub fn spec(&self) -> Spec {
        Spec {
            no_look_alikes: self.no_look_alikes,
            ..Spec::new(Kind::ALL[self.kind])
        }
    }

    pub fn save_prefs(&self) {
        if let (Some(path), Some(length)) = (repl::state_file("generate"), self.length()) {
            let look_alikes = if self.no_look_alikes {
                "no-ambiguous"
            } else {
                ""
            };
            let _ = std::fs::write(
                path,
                format!("{}\n{length}\n{look_alikes}\n", Kind::ALL[self.kind].name()),
            );
        }
    }
}

/// The last type, length and look-alike setting used, else symbols, the
/// default length and look-alikes allowed.
fn load_prefs() -> (usize, u32, bool) {
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
    (kind, length, no_look_alikes)
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
    }
}
