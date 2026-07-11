use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// The password store on disk. Entry/directory names are not encrypted, so the
/// tree is read straight from the filesystem; only entry *contents* go through
/// `pass`.
pub struct Store {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub is_dir: bool,
}

impl Store {
    pub fn open() -> Result<Self> {
        let root = match std::env::var_os("PASSWORD_STORE_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => {
                let home = std::env::var_os("HOME").context("HOME is not set")?;
                Path::new(&home).join(".password-store")
            }
        };
        if !root.is_dir() {
            bail!(
                "password store not found at {} (run `pass init <gpg-id>` first)",
                root.display()
            );
        }
        Ok(Store { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn dir_path(&self, store_path: &str) -> PathBuf {
        if store_path.is_empty() {
            self.root.clone()
        } else {
            self.root.join(store_path)
        }
    }

    pub fn is_dir(&self, store_path: &str) -> bool {
        self.dir_path(store_path).is_dir()
    }

    pub fn is_entry(&self, store_path: &str) -> bool {
        !store_path.is_empty() && self.root.join(format!("{store_path}.gpg")).is_file()
    }

    /// List a store directory: subdirectories plus entries (`.gpg` stripped),
    /// dotfiles (`.git`, `.gpg-id`, ...) skipped. Directories sort first.
    pub fn list(&self, store_path: &str) -> Result<Vec<Node>> {
        let dir = self.dir_path(store_path);
        let entries = fs::read_dir(&dir)
            .with_context(|| format!("cannot read {}", display(store_path)))?;
        let mut nodes = Vec::new();
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
