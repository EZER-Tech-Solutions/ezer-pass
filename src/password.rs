//! What a generated password is made of: a type's groups of characters,
//! less look-alikes and any excluded characters, holding at least one of
//! each group a site would check for (capitals, small letters, digits,
//! symbols) — or, for a passphrase, words from the EFF's large list.

use crate::crypto;
use anyhow::{Result, bail};
use zeroize::Zeroizing;

const UPPER: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LOWER: &str = "abcdefghijklmnopqrstuvwxyz";
const DIGITS: &str = "0123456789";
const SYMBOLS: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

/// Characters easily mistaken for one another when read off a screen.
pub const LOOK_ALIKES: &str = "0O1lI|";

/// The EFF's large diceware list: 7776 words, one per line.
const WORDS: &str = include_str!("wordlist.txt");

/// How many words a passphrase has unless told otherwise.
pub const DEFAULT_WORDS: u32 = 7;

/// What can go between a passphrase's words, the default first.
pub const SEPARATORS: [&str; 5] = ["-", " ", ".", "_", ""];

/// A separator as the `g` form shows it.
pub fn separator_name(separator: &str) -> &str {
    match separator {
        " " => "space",
        "" => "none",
        s => s,
    }
}

/// A type of password.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Letters, digits and symbols: the default.
    Symbols,
    /// Letters and digits, for sites that refuse symbols.
    Alnum,
    /// Letters, digits, `-` and `_`: safe in a database URL, an `.env`
    /// file or a shell without quoting or encoding.
    UrlSafe,
    /// A PIN.
    Digits,
    Hex,
    /// Words, easy to type and remember.
    Passphrase,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Symbols,
        Kind::Alnum,
        Kind::UrlSafe,
        Kind::Digits,
        Kind::Hex,
        Kind::Passphrase,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Symbols => "symbols",
            Kind::Alnum => "letters & digits",
            Kind::UrlSafe => "url-safe",
            Kind::Digits => "digits",
            Kind::Hex => "hex",
            Kind::Passphrase => "passphrase",
        }
    }

    /// What its length counts.
    pub fn unit(self) -> &'static str {
        match self {
            Kind::Passphrase => "words",
            _ => "characters",
        }
    }

    /// The `generate` flag that picks it, if any.
    pub fn from_flag(flag: &str) -> Option<Kind> {
        match flag {
            "-n" | "--no-symbols" => Some(Kind::Alnum),
            "--url-safe" => Some(Kind::UrlSafe),
            "--digits" => Some(Kind::Digits),
            "--hex" => Some(Kind::Hex),
            "--passphrase" => Some(Kind::Passphrase),
            _ => None,
        }
    }

    /// Its groups of characters, each with whether a password must hold at
    /// least one of it.
    fn groups(self) -> &'static [(&'static str, bool)] {
        match self {
            Kind::Symbols => &[
                (UPPER, true),
                (LOWER, true),
                (DIGITS, true),
                (SYMBOLS, true),
            ],
            Kind::Alnum => &[(UPPER, true), (LOWER, true), (DIGITS, true)],
            Kind::UrlSafe => &[(UPPER, true), (LOWER, true), (DIGITS, true), ("-_", false)],
            Kind::Digits => &[(DIGITS, false)],
            Kind::Hex => &[("0123456789abcdef", false)],
            Kind::Passphrase => &[],
        }
    }
}

/// Everything that decides which characters a password is drawn from.
#[derive(Clone, Debug)]
pub struct Spec {
    pub kind: Kind,
    /// Leave out `LOOK_ALIKES`.
    pub no_look_alikes: bool,
    /// Characters a site won't take.
    pub exclude: String,
    /// What goes between a passphrase's words.
    pub separator: String,
    /// Capitalize a passphrase's words and end it with a digit, for sites
    /// that demand both.
    pub capitalize: bool,
}

impl Spec {
    pub fn new(kind: Kind) -> Self {
        Spec {
            kind,
            no_look_alikes: false,
            exclude: String::new(),
            separator: SEPARATORS[0].into(),
            capitalize: false,
        }
    }

    /// The kind's groups with the left-out characters removed; a group
    /// left empty is dropped, and with it the need for one of it.
    fn groups(&self) -> Vec<(String, bool)> {
        self.kind
            .groups()
            .iter()
            .map(|&(chars, required)| {
                let kept: String = chars
                    .chars()
                    .filter(|c| !(self.no_look_alikes && LOOK_ALIKES.contains(*c)))
                    .filter(|c| !self.exclude.contains(*c))
                    .collect();
                (kept, required)
            })
            .filter(|(chars, _)| !chars.is_empty())
            .collect()
    }

    /// Every character a password may hold.
    pub fn chars(&self) -> String {
        self.groups().into_iter().map(|(chars, _)| chars).collect()
    }

    /// About how strong a password of `length` is, in bits.
    pub fn bits(&self, length: usize) -> f64 {
        if self.kind == Kind::Passphrase {
            let digit = if self.capitalize { 10f64.log2() } else { 0.0 };
            return (WORDS.lines().count() as f64).log2() * length as f64 + digit;
        }
        match self.chars().len() {
            0 | 1 => 0.0,
            n => (n as f64).log2() * length as f64,
        }
    }

    pub fn generate(&self, length: usize) -> Result<Zeroizing<String>> {
        if self.kind == Kind::Passphrase {
            return self.passphrase(length);
        }
        let groups = self.groups();
        let chars: String = groups.iter().map(|(c, _)| c.as_str()).collect();
        if chars.is_empty() {
            bail!("that leaves no characters to make a password from");
        }
        let mut required: Vec<&str> = groups
            .iter()
            .filter(|(_, required)| *required)
            .map(|(c, _)| c.as_str())
            .collect();
        // Too short to hold one of each: then nothing is required.
        if length < required.len() {
            required.clear();
        }
        // Drawing the whole password again until it qualifies keeps it
        // uniform over the passwords that do.
        for _ in 0..10_000 {
            let password = crypto::random_password(&chars, length)?;
            if required
                .iter()
                .all(|group| password.chars().any(|c| group.contains(c)))
            {
                return Ok(password);
            }
        }
        bail!("couldn't make a password with every kind of character in it; make it longer")
    }

    /// `length` words from the list, between each the separator.
    fn passphrase(&self, length: usize) -> Result<Zeroizing<String>> {
        let words: Vec<&str> = WORDS.lines().collect();
        let mut out = Zeroizing::new(String::new());
        for i in 0..length {
            if i > 0 {
                out.push_str(&self.separator);
            }
            let word = words[crypto::random_below(words.len())?];
            if self.capitalize {
                // Every listed word starts with a small ASCII letter.
                out.push_str(&word[..1].to_ascii_uppercase());
                out.push_str(&word[1..]);
            } else {
                out.push_str(word);
            }
        }
        if self.capitalize {
            out.push_str(&self.separator);
            out.push(char::from(b'0' + crypto::random_below(10)? as u8));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_passwords_still_hold_every_group() {
        let spec = Spec::new(Kind::Symbols);
        for _ in 0..200 {
            let pw = spec.generate(4).unwrap();
            for group in [UPPER, LOWER, DIGITS, SYMBOLS] {
                assert!(
                    pw.chars().any(|c| group.contains(c)),
                    "{} lacks {group}",
                    *pw
                );
            }
        }
        // Shorter than the number of groups: anything goes.
        assert_eq!(spec.generate(2).unwrap().len(), 2);
    }

    #[test]
    fn left_out_characters_never_appear() {
        let spec = Spec {
            kind: Kind::Alnum,
            no_look_alikes: true,
            exclude: "aeiou".into(),
            ..Spec::new(Kind::Alnum)
        };
        let pw = spec.generate(500).unwrap();
        assert!(
            !pw.chars()
                .any(|c| LOOK_ALIKES.contains(c) || "aeiou".contains(c))
        );
        assert_eq!(spec.chars().len(), 62 - 5 - 5);
    }

    #[test]
    fn an_emptied_group_is_no_longer_required() {
        let spec = Spec {
            kind: Kind::Alnum,
            no_look_alikes: false,
            exclude: DIGITS.into(),
            ..Spec::new(Kind::Alnum)
        };
        assert!(
            spec.generate(3)
                .unwrap()
                .chars()
                .all(|c| c.is_ascii_alphabetic())
        );
        let none = Spec {
            kind: Kind::Digits,
            no_look_alikes: false,
            exclude: DIGITS.into(),
            ..Spec::new(Kind::Alnum)
        };
        assert!(none.generate(4).is_err());
    }

    #[test]
    fn strength() {
        assert_eq!(Spec::new(Kind::Hex).bits(10), 40.0);
        assert!((Spec::new(Kind::Symbols).bits(25) - 163.9).abs() < 0.1);
        assert!((Spec::new(Kind::Passphrase).bits(6) - 77.5).abs() < 0.1);
        let capitalized = Spec {
            capitalize: true,
            ..Spec::new(Kind::Passphrase)
        };
        assert!((capitalized.bits(6) - 80.9).abs() < 0.1);
    }

    #[test]
    fn passphrases_are_words_from_the_list() {
        assert_eq!(WORDS.lines().count(), 7776);
        let pw = Spec::new(Kind::Passphrase).generate(6).unwrap();
        // A few listed words (`yo-yo`) hold a `-` themselves.
        let pieces: Vec<&str> = WORDS.lines().flat_map(|w| w.split('-')).collect();
        assert!(pw.split('-').all(|p| pieces.contains(&p)), "{}", *pw);
        assert!(pw.split('-').count() >= 6);
    }

    #[test]
    fn capitalized_passphrases_end_in_a_digit() {
        let spec = Spec {
            separator: ".".into(),
            capitalize: true,
            ..Spec::new(Kind::Passphrase)
        };
        let pw = spec.generate(4).unwrap();
        let parts: Vec<&str> = pw.split('.').collect();
        assert_eq!(parts.len(), 5, "{}", *pw);
        assert!(
            parts[..4]
                .iter()
                .all(|w| w.starts_with(|c: char| c.is_ascii_uppercase()))
        );
        assert!(parts[4].len() == 1 && parts[4].bytes().all(|b| b.is_ascii_digit()));
    }
}
