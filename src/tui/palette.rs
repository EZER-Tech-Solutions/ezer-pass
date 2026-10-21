//! The `:` command palette: suggestions above the command line that narrow
//! as you type — command names first, then entries and folders for a
//! command's path argument.

use super::fuzzy;
use crate::store::{self, Store};

/// What a command's arguments are, for path suggestions.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Arg {
    None,
    /// Any store path (entries and folders).
    Path,
    /// Folders only.
    Dir,
    /// `context`'s subcommands, then context names.
    Context,
    /// A file on disk, from the current directory, then store paths.
    File,
}

pub struct Spec {
    pub name: &'static str,
    /// The arguments, as shown after the name.
    pub usage: &'static str,
    pub desc: &'static str,
    pub arg: Arg,
}

impl Spec {
    /// Whether the command does nothing useful without arguments.
    pub fn needs_args(&self) -> bool {
        self.usage.contains('<')
    }
}

/// Every command the TUI's command line takes, most used first.
pub const SPECS: &[Spec] = &[
    spec(
        "generate",
        "[-n|--url-safe|--digits|--hex|--passphrase] [--no-ambiguous] [-i] <entry> [len]",
        "generate a password",
        Arg::Path,
    ),
    spec("insert", "[-m] [-e] <entry>", "add an entry", Arg::Path),
    spec(
        "show",
        "[-c] <entry>",
        "view an entry, or -c to copy it",
        Arg::Path,
    ),
    spec("edit", "<entry>", "edit in $EDITOR", Arg::Path),
    spec("mv", "<src> <dest>", "rename or move", Arg::Path),
    spec("cp", "<src> <dest>", "copy", Arg::Path),
    spec("rm", "[-r] <path>", "delete an entry or folder", Arg::Path),
    spec("mkdir", "<path>", "create a folder", Arg::Dir),
    spec("ls", "[path]", "go to a folder in the tree", Arg::Dir),
    spec("cd", "[path]", "go to a folder in the tree", Arg::Dir),
    spec(
        "import",
        "[--force] <file> <entry>",
        "store a file as an entry",
        Arg::File,
    ),
    spec(
        "export",
        "[-f] <entry> [dest]",
        "write a file entry to disk",
        Arg::Path,
    ),
    spec(
        "reindex",
        "[path]",
        "find file entries made before ezer-pass",
        Arg::Dir,
    ),
    spec("find", "<terms>", "search entry names", Arg::None),
    spec("grep", "[-i] <text>", "search inside entries", Arg::None),
    spec("git", "<git-args>", "run git in the store", Arg::None),
    spec(
        "init",
        "[-p folder] [age1...]",
        "set who a folder is encrypted to",
        Arg::None,
    ),
    spec(
        "context",
        "[list|use|add|rm|rename|move] [name]",
        "switch, add or manage contexts",
        Arg::Context,
    ),
    spec("pwd", "", "show the current folder", Arg::None),
    spec("help", "", "list the keys", Arg::None),
    spec("quit", "", "leave (offers to push)", Arg::None),
];

const fn spec(name: &'static str, usage: &'static str, desc: &'static str, arg: Arg) -> Spec {
    Spec {
        name,
        usage,
        desc,
        arg,
    }
}

/// `context`'s subcommands, as the palette offers them.
const CONTEXT_WORDS: &[(&str, &str)] = &[
    ("use", "<name>  switch to a context"),
    ("list", "list contexts"),
    (
        "add",
        "<name> [path]  add one, with a new store or the one at path",
    ),
    ("rm", "<name>  forget one (its store stays)"),
    ("rename", "<old> <new>  give one another name"),
    ("move", "<name> <path>  move one's store"),
];

pub fn find(name: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.name == name)
}

pub enum Item {
    Command(&'static Spec),
    /// A word to complete to (a subcommand, a context's name), with a note.
    Word {
        label: String,
        note: String,
    },
    /// A store path to complete to; `label` is what was typed plus the
    /// rest of the name.
    Path {
        label: String,
        is_dir: bool,
    },
}

pub struct Suggestion {
    pub item: Item,
    /// The whole command line once this suggestion is taken.
    pub line: String,
}

pub struct Palette {
    /// The command being typed, once its name is complete.
    pub command: Option<&'static Spec>,
    pub items: Vec<Suggestion>,
}

/// Suggestions for the command line `text`, with relative paths resolved
/// against `cwd`.
pub fn suggest(text: &str, store: &Store, cwd: &str) -> Palette {
    let Some((name, _)) = text.split_once(char::is_whitespace) else {
        // Still typing the command's name.
        let mut ranked: Vec<(i64, usize, &'static Spec)> = SPECS
            .iter()
            .enumerate()
            .filter_map(|(i, s)| fuzzy::score(text, s.name).map(|score| (score, i, s)))
            .collect();
        ranked.sort_by_key(|&(score, i, _)| (-score, i));
        return Palette {
            command: None,
            items: ranked
                .into_iter()
                .map(|(_, _, s)| Suggestion {
                    item: Item::Command(s),
                    line: format!("{} ", s.name),
                })
                .collect(),
        };
    };
    let command = find(name);
    let mut items = Vec::new();
    // The word being typed: everything after the last space.
    let start = text.rfind(char::is_whitespace).map_or(0, |i| i + 1);
    let word = &text[start..];
    if command.is_some_and(|s| s.arg == Arg::Context) {
        let before: Vec<&str> = text[..start].split_whitespace().collect();
        let words: Vec<(String, String)> = match (before.len(), before.get(1).copied()) {
            (1, _) => CONTEXT_WORDS
                .iter()
                .map(|(w, note)| (w.to_string(), note.to_string()))
                .collect(),
            (2, Some("use" | "rm" | "rename" | "move")) => crate::context::load()
                .unwrap_or_default()
                .into_iter()
                .map(|c| (c.name, crate::context::tilde(&c.path)))
                .collect(),
            _ => Vec::new(),
        };
        let mut ranked: Vec<(i64, usize, String, String)> = words
            .into_iter()
            .enumerate()
            .filter_map(|(i, (w, note))| fuzzy::score(word, &w).map(|s| (s, i, w, note)))
            .collect();
        ranked.sort_by_key(|r| (-r.0, r.1));
        for (_, _, label, note) in ranked {
            items.push(Suggestion {
                line: format!("{}{label} ", &text[..start]),
                item: Item::Word { label, note },
            });
        }
    } else if command.is_some_and(|s| s.arg == Arg::File)
        && !word.starts_with('-')
        && text[..start]
            .split_whitespace()
            .skip(1)
            .all(|w| w.starts_with('-'))
    {
        items = files(&text[..start], word);
    } else if let Some(spec) = command
        && spec.arg != Arg::None
        && !word.starts_with('-')
    {
        let (dir_part, prefix) = match word.rfind('/') {
            Some(i) => word.split_at(i + 1),
            None => ("", word),
        };
        let folder = store::resolve(cwd, dir_part);
        let mut ranked: Vec<(i64, String, bool)> = store
            .list(&folder)
            .unwrap_or_default()
            .into_iter()
            .filter(|n| spec.arg != Arg::Dir || n.is_dir)
            .filter_map(|n| fuzzy::score(prefix, &n.name).map(|s| (s, n.name, n.is_dir)))
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.2.cmp(&a.2)).then(a.1.cmp(&b.1)));
        for (_, child, is_dir) in ranked {
            let label = format!("{dir_part}{child}{}", if is_dir { "/" } else { "" });
            // A folder stays open for more typing; an entry is a whole word.
            let line = format!("{}{label}{}", &text[..start], if is_dir { "" } else { " " });
            items.push(Suggestion {
                item: Item::Path { label, is_dir },
                line,
            });
        }
    }
    Palette { command, items }
}

/// Files and folders on disk matching `word`, relative to the current
/// directory (or `~`). Hidden ones only once `.` is typed.
fn files(before: &str, word: &str) -> Vec<Suggestion> {
    let (dir_part, prefix) = match word.rfind('/') {
        Some(i) => word.split_at(i + 1),
        None => ("", word),
    };
    let dir = match dir_part {
        "" => Ok(".".into()),
        d => crate::context::expand(d),
    };
    let Ok(read) = dir.and_then(|d| Ok(std::fs::read_dir(d)?)) else {
        return Vec::new();
    };
    let mut ranked: Vec<(i64, String, bool)> = read
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if name.starts_with('.') && !prefix.starts_with('.') {
                return None;
            }
            // Follows symlinks, so a link to a folder opens like one.
            let is_dir = e.path().is_dir();
            fuzzy::score(prefix, &name).map(|s| (s, name, is_dir))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.2.cmp(&a.2)).then(a.1.cmp(&b.1)));
    ranked
        .into_iter()
        .map(|(_, name, is_dir)| {
            let label = format!("{dir_part}{name}{}", if is_dir { "/" } else { "" });
            // A folder stays open for more typing; a file is a whole word.
            let line = if is_dir {
                format!("{before}{label}")
            } else {
                format!("{before}{} ", super::app::quote(&label))
            };
            Suggestion {
                item: Item::Path { label, is_dir },
                line,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spec_is_a_real_command() {
        for s in SPECS {
            assert!(
                crate::commands::COMMANDS.contains(&s.name),
                "{} is not a command",
                s.name
            );
        }
    }

    #[test]
    fn context_words_match_the_command() {
        let words: Vec<&str> = CONTEXT_WORDS.iter().map(|w| w.0).collect();
        for w in crate::repl::CONTEXT_SUBCOMMANDS {
            assert!(words.contains(w), "{w} is missing from the palette");
        }
        assert_eq!(words.len(), crate::repl::CONTEXT_SUBCOMMANDS.len());
    }

    #[test]
    fn files_complete_from_disk() {
        let dir = std::env::temp_dir().join(format!("ezer-palette-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("keys")).unwrap();
        std::fs::write(dir.join("my notes.txt"), "").unwrap();
        std::fs::write(dir.join(".hidden"), "").unwrap();
        let base = format!("{}/", dir.display());
        let lines: Vec<String> = files("import ", &base)
            .into_iter()
            .map(|s| s.line)
            .collect();
        assert_eq!(
            lines,
            [
                format!("import {base}keys/"),
                format!("import '{base}my notes.txt' "),
            ]
        );
        assert_eq!(files("import ", &format!("{base}.h")).len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn needs_args() {
        assert!(find("mv").unwrap().needs_args());
        assert!(!find("reindex").unwrap().needs_args());
        assert!(!find("pwd").unwrap().needs_args());
    }
}
