//! The TUI's settings (`S`), kept between sessions in
//! `$XDG_CONFIG_HOME/ezer-pass/settings`, one `key = value` a line.

use super::theme::{self, THEMES};
use crate::context;

/// The idle lock's choices, in minutes; 0 never locks.
pub const LOCK_AFTER: &[u64] = &[0, 1, 5, 15, 30, 60];

const DEFAULT_LOCK_AFTER: u64 = 15;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    /// Hand the passphrase typed into the unlock popup to the agent too,
    /// so one-shot commands, scripts and the browser extension can decrypt
    /// until the agent locks, instead of keeping it to this session.
    pub unlock_agent: bool,
    /// Minutes without a key press before the TUI forgets the identity;
    /// 0 keeps it until you quit.
    pub lock_after: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            unlock_agent: false,
            lock_after: DEFAULT_LOCK_AFTER,
        }
    }
}

impl Settings {
    /// The settings saved last time, else the defaults.
    pub fn load() -> Self {
        let text = context::config_dir()
            .ok()
            .and_then(|dir| std::fs::read_to_string(dir.join("settings")).ok());
        Settings::parse(text.as_deref().unwrap_or(""))
    }

    fn parse(text: &str) -> Self {
        let mut settings = Settings::default();
        for line in text.lines().map(str::trim) {
            if line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match (key.trim(), value.trim()) {
                ("unlock-agent", "yes") => settings.unlock_agent = true,
                ("unlock-agent", "no") => settings.unlock_agent = false,
                ("lock-after", minutes) => {
                    if let Ok(minutes) = minutes.parse() {
                        settings.lock_after = minutes;
                    }
                }
                _ => {}
            }
        }
        settings
    }

    fn render(&self) -> String {
        format!(
            "# ezer-pass TUI settings (S in the TUI changes them)\n\
             unlock-agent = {}\n\
             lock-after = {}\n",
            if self.unlock_agent { "yes" } else { "no" },
            self.lock_after,
        )
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let dir = context::config_dir()?;
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("settings"), self.render())?;
        Ok(())
    }
}

/// The rows of the settings popup.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    UnlockAgent,
    LockAfter,
    Theme,
}

impl Row {
    pub const ALL: [Row; 3] = [Row::UnlockAgent, Row::LockAfter, Row::Theme];

    pub fn label(self) -> &'static str {
        match self {
            Row::UnlockAgent => "unlock the agent too",
            Row::LockAfter => "lock when idle",
            Row::Theme => "theme",
        }
    }

    /// The row's current value, as shown.
    pub fn value(self, settings: &Settings) -> String {
        match self {
            Row::UnlockAgent => if settings.unlock_agent { "on" } else { "off" }.into(),
            Row::LockAfter => match settings.lock_after {
                0 => "never".into(),
                1 => "after 1 minute".into(),
                m => format!("after {m} minutes"),
            },
            Row::Theme => theme::theme().name.into(),
        }
    }

    /// What the row does, under the list while it is highlighted.
    pub fn help(self, settings: &Settings) -> &'static str {
        match (self, settings.unlock_agent) {
            (Row::UnlockAgent, false) => {
                "Off: the passphrase you type here unlocks this TUI alone. \
                 An agent you unlocked yourself (`ezer-pass unlock`) is still used."
            }
            (Row::UnlockAgent, true) => {
                "On: the passphrase you type here also unlocks the agent, so \
                 commands, scripts, coding agents and the browser can decrypt \
                 until it locks (`:lock`, or its timeout). Takes effect at the next unlock."
            }
            (Row::LockAfter, _) => {
                "Forget the identity, and close a shown password, after this \
                 long without a key press. An unlocked agent keeps its own timeout."
            }
            (Row::Theme, _) => "The colors; :theme shows them side by side.",
        }
    }

    /// Step the row's value forward (or back), applying it at once.
    pub fn cycle(self, settings: &mut Settings, back: bool) {
        match self {
            Row::UnlockAgent => settings.unlock_agent = !settings.unlock_agent,
            Row::LockAfter => {
                // An edited file may hold a value off the list; go to the
                // nearest choice past it.
                let next = if back {
                    LOCK_AFTER
                        .iter()
                        .rev()
                        .find(|&&m| m < settings.lock_after)
                        .or(LOCK_AFTER.last())
                } else {
                    LOCK_AFTER
                        .iter()
                        .find(|&&m| m > settings.lock_after)
                        .or(LOCK_AFTER.first())
                };
                settings.lock_after = *next.expect("not empty");
            }
            Row::Theme => {
                let n = THEMES.len();
                let at = theme::current();
                theme::set(if back { (at + n - 1) % n } else { (at + 1) % n });
                theme::save();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_and_defaults() {
        let s = Settings {
            unlock_agent: true,
            lock_after: 5,
        };
        assert_eq!(Settings::parse(&s.render()), s);
        assert_eq!(Settings::parse(""), Settings::default());
        assert_eq!(
            Settings::parse("lock-after = soon\nunlock-agent = maybe\nnonsense"),
            Settings::default()
        );
        assert_eq!(Settings::parse("lock-after=0").lock_after, 0);
    }

    #[test]
    fn idle_lock_cycles_through_the_choices() {
        let mut s = Settings {
            lock_after: 60,
            ..Settings::default()
        };
        Row::LockAfter.cycle(&mut s, false);
        assert_eq!(s.lock_after, 0);
        Row::LockAfter.cycle(&mut s, true);
        assert_eq!(s.lock_after, 60);
        s.lock_after = 7;
        Row::LockAfter.cycle(&mut s, false);
        assert_eq!(s.lock_after, 15);
        s.lock_after = 7;
        Row::LockAfter.cycle(&mut s, true);
        assert_eq!(s.lock_after, 5);
    }
}
