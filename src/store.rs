use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};

/// The password store on disk.
/// Entry/directory names are not encrypted, so the tree is read straight from the filesystem;
/// only entry *contents* go through `pass`.
pub struct Store {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub is_dir: bool,
}

impl Store {
    /// Opens the user password store, returning an error if it does not exist.
    /// The store is located at `$PASSWORD_STORE_DIR` if set, otherwise at `$HOME/.password-store`.
    pub fn open() -> Result<Self> {
        // Check for a user-specified store directory, then fall back to `$HOME/.password-store`.
        let root = match std::env::var_os("PASSWORD_STORE_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => {
                let home = std::env::var_os("HOME").context("HOME is not set")?;
                Path::new(&home).join(".password-store")
            }
        };
        // Check that the store directory exists and is a directory.
        if !root.is_dir() {
            bail!(
                "password store not found at {} (run `pass init <gpg-id>` first)",
                root.display()
            );
        }
        // Return the store
        Ok(Store { root })
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
        !store_path.is_empty() && self.root.join(format!("{store_path}.gpg")).is_file()
    }

    /// List a store directory: subdirectories plus entries (`.gpg` stripped),
    /// dotfiles (`.git`, `.gpg-id`, ...) skipped. Directories sort first.
    pub fn list(&self, store_path: &str) -> Result<Vec<Node>> {
        // Get the absolute path of the directory in the store.
        let dir = self.dir_path(store_path);

        // Get the entries in the directory.
        let entries =
            fs::read_dir(&dir).with_context(|| format!("cannot read {}", display(store_path)))?;

        // Initialize a vector to hold the nodes (subdirectories and entries).
        let mut nodes = Vec::new();

        // Insert entries as nodes, skipping dotfiles and stripping `.gpg` from entries.
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if entry.file_type()?.is_dir() {
                nodes.push(Node { name, is_dir: true });
            } else if let Some(stem) = name.strip_suffix(".gpg") {
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

    /// Last modification time of an entry's `.gpg` file.
    pub fn modified(&self, store_path: &str) -> Option<std::time::SystemTime> {
        fs::metadata(self.root.join(format!("{store_path}.gpg")))
            .and_then(|m| m.modified())
            .ok()
    }

    /// Create a store directory (and any missing parents). pass itself has no
    /// mkdir: directories normally appear when an entry is written into them,
    /// and git won't track one until it holds an entry.
    pub fn mkdir(&self, store_path: &str) -> Result<()> {
        if store_path.is_empty() || self.is_dir(store_path) {
            bail!("{} already exists", display(store_path));
        }
        if self.is_entry(store_path) {
            bail!("{} is an entry", display(store_path));
        }
        // Refuse to create a directory below an entry name (e.g. `web1/x`
        // when `web1` is an entry) — legal on disk but confusing in pass.
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

#[cfg(test)]
mod tests {
    use super::resolve;

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
