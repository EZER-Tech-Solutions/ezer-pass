//! Which entries are file entries. Entry contents are encrypted, so this
//! can't be read off the store; instead ezer-pass keeps an index in the
//! store root as `.files` (git-tracked, so it syncs with the store) listing each file
//! entry's path and decoded size. It is updated by ezer-pass's own imports,
//! moves, copies and deletes, and corrected whenever an entry is decrypted,
//! so changes made with plain `pass` heal the next time the entry is viewed.
//! It reveals only *that* an entry is a file — names are already unencrypted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const FILE_NAME: &str = ".files";

#[derive(Default)]
pub struct FileIndex {
    /// Store path → decoded size in bytes.
    files: BTreeMap<String, u64>,
    dirty: bool,
}

impl FileIndex {
    /// Read the index from a store, treating a missing or unreadable one as empty.
    pub fn load(store_root: &Path) -> Self {
        let files = std::fs::read_to_string(store_root.join(FILE_NAME))
            .map(|s| parse(&s))
            .unwrap_or_default();
        FileIndex {
            files,
            dirty: false,
        }
    }

    /// The decoded size of a file entry, or `None` for a password (or an
    /// entry the index hasn't learned about).
    pub fn size(&self, path: &str) -> Option<u64> {
        self.files.get(path).copied()
    }

    pub fn is_file(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    /// Record what decrypting `path` revealed it to be.
    pub fn learn(&mut self, path: &str, file_size: Option<u64>) {
        match file_size {
            Some(size) => self.insert(path, size),
            None => self.remove(path),
        }
    }

    pub fn insert(&mut self, path: &str, size: u64) {
        // A tab or newline would corrupt the format; such an entry just
        // shows as a password.
        if path.contains(['\t', '\n']) {
            return;
        }
        if self.files.insert(path.to_string(), size) != Some(size) {
            self.dirty = true;
        }
    }

    /// Forget `path`, and everything under it if it is a directory.
    pub fn remove(&mut self, path: &str) {
        let before = self.files.len();
        self.files.retain(|p, _| !under(p, path));
        self.dirty |= self.files.len() != before;
    }

    /// Follow a `pass mv` (`keep_source = false`) or `pass cp` of an entry
    /// or directory.
    pub fn transfer(&mut self, from: &str, to: &str, keep_source: bool) {
        let moved: Vec<(String, u64)> = self
            .files
            .iter()
            .filter(|(p, _)| under(p, from))
            .map(|(p, size)| (format!("{to}{}", &p[from.len()..]), *size))
            .collect();
        if !keep_source {
            self.remove(from);
        }
        // The destination is overwritten, so drop what was there first.
        self.remove(to);
        for (p, size) in moved {
            self.insert(&p, size);
        }
    }

    /// Drop paths that are no longer entries (removed or moved by plain
    /// `pass`, or by another machine).
    pub fn prune(&mut self, is_entry: impl Fn(&str) -> bool) {
        let before = self.files.len();
        self.files.retain(|p, _| is_entry(p));
        self.dirty |= self.files.len() != before;
    }

    /// Write the index back if it changed and commit it when the store is
    /// a git repo. Best effort: a failed commit leaves the file staged, so
    /// pass's next commit picks it up. Git runs with no stdio, so it can't
    /// disturb the TUI.
    pub fn save(&mut self, store_root: &Path) {
        if !self.dirty {
            return;
        }
        let path: PathBuf = store_root.join(FILE_NAME);
        let result = if self.files.is_empty() {
            std::fs::remove_file(&path).or_else(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(e),
            })
        } else {
            std::fs::write(&path, render(&self.files))
        };
        if result.is_err() {
            return;
        }
        self.dirty = false;
        if !store_root.join(".git").exists() {
            return;
        }
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(store_root)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if git(&["add", "-A", "--", FILE_NAME]) {
            git(&[
                "commit",
                "-q",
                "-m",
                "Update ezer-pass file index.",
                "--",
                FILE_NAME,
            ]);
        }
    }
}

/// Whether `p` is `dir` itself or inside it.
fn under(p: &str, dir: &str) -> bool {
    p == dir
        || p.strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn parse(s: &str) -> BTreeMap<String, u64> {
    s.lines()
        .filter_map(|line| {
            let (path, size) = line.rsplit_once('\t')?;
            Some((path.to_string(), size.trim().parse().ok()?))
        })
        .collect()
}

fn render(files: &BTreeMap<String, u64>) -> String {
    files
        .iter()
        .map(|(p, size)| format!("{p}\t{size}\n"))
        .collect()
}

/// "48.2 KB"-style size.
pub fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(paths: &[(&str, u64)]) -> FileIndex {
        let mut idx = FileIndex::default();
        for (p, s) in paths {
            idx.insert(p, *s);
        }
        idx.dirty = false;
        idx
    }

    #[test]
    fn roundtrip_format() {
        let idx = index(&[("apple/codes", 48213), ("my dir/cert", 10)]);
        assert_eq!(parse(&render(&idx.files)), idx.files);
        assert!(parse("garbage\nno-size\t\n").is_empty());
    }

    #[test]
    fn remove_covers_directories_but_not_siblings() {
        let mut idx = index(&[("a/x", 1), ("a/y/z", 2), ("ab", 3)]);
        idx.remove("a");
        assert!(!idx.is_file("a/x") && !idx.is_file("a/y/z"));
        assert!(idx.is_file("ab"));
        assert!(idx.dirty);
    }

    #[test]
    fn transfer_moves_and_copies() {
        let mut idx = index(&[("a/x", 1), ("a/y/z", 2), ("b", 3)]);
        idx.transfer("a", "c/a", false);
        assert_eq!(idx.size("c/a/x"), Some(1));
        assert_eq!(idx.size("c/a/y/z"), Some(2));
        assert!(!idx.is_file("a/x"));

        idx.transfer("b", "b2", true);
        assert_eq!(idx.size("b"), Some(3));
        assert_eq!(idx.size("b2"), Some(3));

        // Moving a password over a file entry: the file is gone.
        idx.transfer("not-a-file", "b2", false);
        assert!(!idx.is_file("b2"));
    }

    #[test]
    fn learn_corrects_both_ways() {
        let mut idx = index(&[("x", 5)]);
        idx.learn("x", None);
        assert!(!idx.is_file("x"));
        idx.learn("y", Some(9));
        assert_eq!(idx.size("y"), Some(9));
    }

    #[test]
    fn unchanged_insert_stays_clean() {
        let mut idx = index(&[("x", 5)]);
        idx.insert("x", 5);
        assert!(!idx.dirty);
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(500), "500 B");
        assert_eq!(human_size(48213), "47.1 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
    }
}
