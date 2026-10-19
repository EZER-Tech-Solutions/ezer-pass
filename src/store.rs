use crate::crypto::{self, Keys};
use age::x25519::Recipient;
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// An entry's file is its store path plus this.
pub const EXT: &str = ".age";

/// The file in a folder listing who its entries are encrypted to.
pub const RECIPIENTS: &str = ".age-recipients";

/// The password store on disk.
/// Entry/directory names are not encrypted, so the tree is read straight from the filesystem;
/// only entry *contents* are encrypted, with age (see `crypto`).
pub struct Store {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub is_dir: bool,
}

impl Store {
    /// Where the store lives: `$PASSWORD_STORE_DIR` if set, otherwise
    /// `$HOME/.password-store`.
    pub fn location() -> Result<PathBuf> {
        match std::env::var_os("PASSWORD_STORE_DIR") {
            Some(dir) => Ok(PathBuf::from(dir)),
            None => {
                let home = std::env::var_os("HOME").context("HOME is not set")?;
                Ok(Path::new(&home).join(".password-store"))
            }
        }
    }

    /// Opens the user password store, returning an error if it does not exist.
    pub fn open() -> Result<Self> {
        let root = Self::location()?;
        if !root.is_dir() {
            bail!(
                "password store not found at {} (run `ezer-pass init` to make one)",
                root.display()
            );
        }
        Ok(Store { root })
    }

    /// Opens the store, making an empty one first if there is none.
    pub fn create() -> Result<Self> {
        let root = Self::location()?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)
            .with_context(|| format!("cannot create {}", root.display()))?;
        Ok(Store { root })
    }

    /// Whether the store is still encrypted with gpg (made by `pass`), and
    /// needs `ezer-pass migrate`.
    pub fn uses_gpg(&self) -> bool {
        self.root.join(".gpg-id").exists()
    }

    /// Returns the root path of the store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the absolute path of a store directory, given its store path.
    fn dir_path(&self, store_path: &str) -> PathBuf {
        if store_path.is_empty() {
            self.root.clone()
        } else {
            self.root.join(store_path)
        }
    }

    /// Returns true if the given store path is a directory in the store.
    pub fn is_dir(&self, store_path: &str) -> bool {
        self.dir_path(store_path).is_dir()
    }

    /// Returns true if the given store path is an entry in the store.
    pub fn is_entry(&self, store_path: &str) -> bool {
        !store_path.is_empty() && self.entry_file(store_path).is_file()
    }

    /// The file holding an entry.
    pub fn entry_file(&self, store_path: &str) -> PathBuf {
        self.root.join(format!("{store_path}{EXT}"))
    }

    /// Who entries in `dir` are encrypted to: the nearest `.age-recipients`
    /// in it or above it. Also returns the folder that file is in.
    pub fn recipients(&self, dir: &str) -> Result<(String, Vec<Recipient>)> {
        let mut dir = dir;
        loop {
            let file = self.dir_path(dir).join(RECIPIENTS);
            if file.is_file() {
                let shown = format!("{}{RECIPIENTS}", display_dir(dir));
                let text =
                    fs::read_to_string(&file).with_context(|| format!("cannot read {shown}"))?;
                let recipients = crypto::parse_recipients(&text).with_context(|| shown.clone())?;
                if recipients.is_empty() {
                    bail!("{shown} lists no recipients");
                }
                return Ok((dir.to_string(), recipients));
            }
            if dir.is_empty() {
                bail!("the store has no {RECIPIENTS} (run `ezer-pass init`)");
            }
            dir = parent(dir);
        }
    }

    /// Decrypt an entry.
    pub fn read_entry(&self, store_path: &str, keys: &Keys) -> Result<Zeroizing<String>> {
        let file = self.entry_file(store_path);
        let ciphertext =
            fs::read(&file).with_context(|| format!("cannot read {}", display(store_path)))?;
        let plain = crypto::decrypt(keys, &ciphertext)
            .with_context(|| format!("cannot decrypt {}", display(store_path)))?;
        Ok(Zeroizing::new(String::from_utf8_lossy(&plain).into_owned()))
    }

    /// Encrypt `plaintext` to the entry's recipients and write it, making
    /// its folder if needed.
    pub fn write_entry(&self, store_path: &str, plaintext: &[u8]) -> Result<()> {
        let (_, recipients) = self.recipients(parent(store_path))?;
        let ciphertext = crypto::encrypt(&recipients, plaintext)?;
        self.write_file(&self.entry_file(store_path), &ciphertext)
            .with_context(|| format!("cannot write {}", display(store_path)))
    }

    /// Write a file in the store all at once: to a temporary file beside
    /// it, then renamed over it, so a crash never leaves half an entry.
    pub fn write_file(&self, file: &Path, data: &[u8]) -> Result<()> {
        let dir = file.parent().context("no parent folder")?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let name = file.file_name().context("no file name")?.to_string_lossy();
        let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        let written = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut f| f.write_all(data).and_then(|_| f.sync_all()))
            .and_then(|_| fs::rename(&tmp, file));
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        Ok(written?)
    }

    /// Remove `dir` and the folders above it while they are empty, as
    /// after the last entry in them is deleted or moved away.
    pub fn remove_empty_dirs(&self, dir: &str) {
        let mut dir = dir;
        while !dir.is_empty() && fs::remove_dir(self.dir_path(dir)).is_ok() {
            dir = parent(dir);
        }
    }

    /// List a store directory: subdirectories plus entries (`.age` stripped),
    /// dotfiles (`.git`, `.age-recipients`, ...) skipped. Directories sort first.
    pub fn list(&self, store_path: &str) -> Result<Vec<Node>> {
        // Get the absolute path of the directory in the store.
        let dir = self.dir_path(store_path);

        // Get the entries in the directory.
        let entries =
            fs::read_dir(&dir).with_context(|| format!("cannot read {}", display(store_path)))?;

        // Initialize a vector to hold the nodes (subdirectories and entries).
        let mut nodes = Vec::new();

        // Insert entries as nodes, skipping dotfiles and stripping `.age` from entries.
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if entry.file_type()?.is_dir() {
                nodes.push(Node { name, is_dir: true });
            } else if let Some(stem) = name.strip_suffix(EXT) {
                nodes.push(Node {
                    name: stem.to_string(),
                    is_dir: false,
                });
            }
        }
        nodes.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
        Ok(nodes)
    }

    /// Every entry and directory in the store as store paths, depth-first in
    /// `list` order. Unreadable directories are skipped rather than failing
    /// the whole walk.
    pub fn walk(&self) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        let mut stack = vec![String::new()];
        while let Some(dir) = stack.pop() {
            let Ok(nodes) = self.list(&dir) else { continue };
            let mut subdirs = Vec::new();
            for node in nodes {
                let path = if dir.is_empty() {
                    node.name
                } else {
                    format!("{dir}/{}", node.name)
                };
                if node.is_dir {
                    subdirs.push(path.clone());
                }
                out.push((path, node.is_dir));
            }
            stack.extend(subdirs.into_iter().rev());
        }
        out
    }

    /// Last modification time of an entry's file.
    pub fn modified(&self, store_path: &str) -> Option<std::time::SystemTime> {
        fs::metadata(self.entry_file(store_path))
            .and_then(|m| m.modified())
            .ok()
    }

    /// Create a store directory (and any missing parents). Directories
    /// normally appear when an entry is written into them, and git won't
    /// track one until it holds an entry.
    pub fn mkdir(&self, store_path: &str) -> Result<()> {
        if store_path.is_empty() || self.is_dir(store_path) {
            bail!("{} already exists", display(store_path));
        }
        if self.is_entry(store_path) {
            bail!("{} is an entry", display(store_path));
        }
        // Refuse to create a directory below an entry name (e.g. `web1/x`
        // when `web1` is an entry) — legal on disk but confusing.
        let mut prefix = String::new();
        for part in store_path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if self.is_entry(&prefix) {
                bail!("{} is an entry, not a directory", display(&prefix));
            }
        }
        fs::create_dir_all(self.dir_path(store_path))
            .with_context(|| format!("cannot create {}", display(store_path)))
    }
}

/// Resolve `input` against the current store directory `cwd`. Store paths are
/// normalized strings without leading/trailing slashes ("" is the root).
/// Supports `.`, `..`, and a leading `/` for store-absolute paths; `..` clamps
/// at the root rather than escaping the store.
pub fn resolve(cwd: &str, input: &str) -> String {
    let mut parts: Vec<&str> = if input.starts_with('/') {
        Vec::new()
    } else {
        cwd.split('/').filter(|s| !s.is_empty()).collect()
    };
    for comp in input.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    parts.join("/")
}

/// Human-facing form of a store path: the root is `/`, others get a leading slash.
pub fn display(store_path: &str) -> String {
    format!("/{store_path}")
}

/// A folder's human-facing form with a trailing slash: `/`, `/a/b/`.
pub fn display_dir(store_path: &str) -> String {
    if store_path.is_empty() {
        "/".into()
    } else {
        format!("/{store_path}/")
    }
}

/// The folder holding a store path ("" for the root).
pub fn parent(store_path: &str) -> &str {
    store_path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_follow_the_nearest_recipients() {
        let root = std::env::temp_dir().join(format!("ezer-pass-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("team")).unwrap();
        let store = Store { root: root.clone() };
        let (me, other) = (Keys::generate(), Keys::generate());
        let line = |k: &Keys| format!("{}\n", k.recipients()[0]);
        fs::write(root.join(RECIPIENTS), line(&me)).unwrap();
        fs::write(root.join("team").join(RECIPIENTS), line(&other)).unwrap();

        store.write_entry("a/b", b"mine\n").unwrap();
        store.write_entry("team/c", b"theirs\n").unwrap();
        assert!(store.is_entry("a/b") && store.list("a").unwrap()[0].name == "b");
        assert_eq!(&*store.read_entry("a/b", &me).unwrap(), "mine\n");
        assert!(store.read_entry("a/b", &other).is_err());
        assert_eq!(&*store.read_entry("team/c", &other).unwrap(), "theirs\n");
        assert_eq!(store.recipients("team/x/y").unwrap().0, "team");
        assert_eq!(store.recipients("a").unwrap().0, "");

        fs::remove_file(store.entry_file("a/b")).unwrap();
        store.remove_empty_dirs("a");
        assert!(!store.is_dir("a"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn relative_paths() {
        assert_eq!(resolve("", "servers"), "servers");
        assert_eq!(resolve("servers", "web1"), "servers/web1");
        assert_eq!(resolve("servers", "prod/web1"), "servers/prod/web1");
    }

    #[test]
    fn absolute_paths() {
        assert_eq!(resolve("servers", "/apple/id"), "apple/id");
        assert_eq!(resolve("servers", "/"), "");
    }

    #[test]
    fn dot_and_dotdot() {
        assert_eq!(resolve("servers/prod", ".."), "servers");
        assert_eq!(resolve("servers/prod", "../../apple"), "apple");
        assert_eq!(resolve("servers", "./web1"), "servers/web1");
        assert_eq!(resolve("servers", "prod/../web1"), "servers/web1");
    }

    #[test]
    fn dotdot_clamps_at_root() {
        assert_eq!(resolve("", "../../.."), "");
        assert_eq!(resolve("servers", "../../../etc"), "etc");
    }

    #[test]
    fn stray_slashes() {
        assert_eq!(resolve("", "servers/"), "servers");
        assert_eq!(resolve("", "servers//web1"), "servers/web1");
    }
}
