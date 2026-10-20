//! Contexts: named stores (work, personal, ...), each its own folder with
//! its own git repo and recipients. They are listed in
//! `$XDG_CONFIG_HOME/ezer-pass/contexts`:
//!
//! ```text
//! [work]
//! path = ~/.local/share/ezer-pass/work
//! # optional; otherwise the shared identity
//! identity = ~/.config/ezer-pass/work.age
//! ```
//!
//! `default` is always there: `$PASSWORD_STORE_DIR` or `~/.password-store`,
//! unless the file gives it another path.

use anyhow::{Context as _, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT: &str = "default";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub name: String,
    pub path: PathBuf,
    /// Its own identity file, instead of the shared one.
    pub identity: Option<PathBuf>,
}

impl Context {
    /// The identity that decrypts this context: `$EZER_PASS_IDENTITY`,
    /// else its own, else the shared `$XDG_CONFIG_HOME/ezer-pass/identity.age`.
    pub fn identity(&self) -> Result<PathBuf> {
        if let Some(path) = std::env::var_os("EZER_PASS_IDENTITY") {
            return Ok(PathBuf::from(path));
        }
        match &self.identity {
            Some(path) => Ok(path.clone()),
            None => Ok(config_dir()?.join("identity.age")),
        }
    }
}

fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("HOME").context("HOME is not set")?,
    ))
}

/// `$XDG_CONFIG_HOME/ezer-pass`.
pub fn config_dir() -> Result<PathBuf> {
    let config = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => home()?.join(".config"),
    };
    Ok(config.join("ezer-pass"))
}

fn contexts_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("contexts"))
}

/// Where a new context's store goes: `$XDG_DATA_HOME/ezer-pass/<name>`.
pub fn new_path(name: &str) -> Result<PathBuf> {
    let data = match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => home()?.join(".local/share"),
    };
    Ok(data.join("ezer-pass").join(name))
}

fn default_path() -> Result<PathBuf> {
    match std::env::var_os("PASSWORD_STORE_DIR") {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => Ok(home()?.join(".password-store")),
    }
}

/// `~/x` with the home directory filled in.
pub fn expand(path: &str) -> Result<PathBuf> {
    match path.strip_prefix("~/") {
        Some(rest) => Ok(home()?.join(rest)),
        None if path == "~" => home(),
        None => Ok(PathBuf::from(path)),
    }
}

/// A path with the home directory shown as `~`.
pub fn tilde(path: &Path) -> String {
    let shown = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && shown.starts_with(&home) => {
            format!("~{}", &shown[home.len()..])
        }
        _ => shown,
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Every context, `default` first, then in the order the file lists them.
pub fn load() -> Result<Vec<Context>> {
    let file = contexts_file()?;
    let text = match fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", file.display())),
    };
    let mut contexts = parse(&text).with_context(|| format!("in {}", file.display()))?;
    match contexts.iter().position(|c| c.name == DEFAULT) {
        // `$PASSWORD_STORE_DIR` still wins, as with pass.
        Some(i) => {
            let mut default = contexts.remove(i);
            if std::env::var_os("PASSWORD_STORE_DIR").is_some() {
                default.path = default_path()?;
            }
            contexts.insert(0, default);
        }
        None => contexts.insert(
            0,
            Context {
                name: DEFAULT.into(),
                path: default_path()?,
                identity: None,
            },
        ),
    }
    Ok(contexts)
}

fn parse(text: &str) -> Result<Vec<Context>> {
    let mut contexts: Vec<Context> = Vec::new();
    let mut current: Option<(String, Option<PathBuf>, Option<PathBuf>)> = None;
    let finish = |current: Option<(String, Option<PathBuf>, Option<PathBuf>)>,
                  contexts: &mut Vec<Context>|
     -> Result<()> {
        if let Some((name, path, identity)) = current {
            let path = path.with_context(|| format!("[{name}] has no path"))?;
            contexts.push(Context {
                name,
                path,
                identity,
            });
        }
        Ok(())
    };
    for (n, line) in text.lines().enumerate().map(|(n, l)| (n + 1, l.trim())) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let name = name.trim();
            if !valid_name(name) {
                bail!("line {n}: {name:?} is not a context name (letters, digits, - and _)");
            }
            if contexts.iter().any(|c| c.name == name)
                || current.as_ref().is_some_and(|c| c.0 == name)
            {
                bail!("line {n}: [{name}] is listed twice");
            }
            finish(current.take(), &mut contexts)?;
            current = Some((name.to_string(), None, None));
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("line {n}: expected `key = value` or `[name]`");
        };
        let Some(ctx) = current.as_mut() else {
            bail!("line {n}: {} comes before any [name]", key.trim());
        };
        let value = expand(value.trim())?;
        match key.trim() {
            "path" => ctx.1 = Some(value),
            "identity" => ctx.2 = Some(value),
            other => bail!("line {n}: unknown key {other:?} (path, identity)"),
        }
    }
    finish(current, &mut contexts)?;
    Ok(contexts)
}

/// The context named `name`.
pub fn find(name: &str) -> Result<Context> {
    let contexts = load()?;
    match contexts.iter().find(|c| c.name == name) {
        Some(c) => Ok(c.clone()),
        None => bail!(
            "no context named {name:?} (contexts: {}); `ezer-pass -C {name} init` makes it",
            contexts
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Add a context to the file. `path` defaults to `new_path(name)`.
pub fn add(name: &str, path: Option<PathBuf>) -> Result<Context> {
    if !valid_name(name) {
        bail!("{name:?} is not a context name (letters, digits, - and _)");
    }
    if load()?.iter().any(|c| c.name == name) {
        bail!("there is already a context named {name}");
    }
    let path = match path {
        Some(p) => p,
        None => new_path(name)?,
    };
    let file = contexts_file()?;
    let mut text = fs::read_to_string(&file).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&format!("[{name}]\npath = {}\n", tilde(&path)));
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    fs::write(&file, text).with_context(|| format!("cannot write {}", file.display()))?;
    Ok(Context {
        name: name.into(),
        path,
        identity: None,
    })
}

/// The context to open: `-C`/`$EZER_PASS_CONTEXT` if given, else the one
/// last picked in the TUI. With `$PASSWORD_STORE_DIR` set and nothing
/// named, `default` (its store), so a throwaway store isn't overruled.
pub fn current(named: Option<&str>) -> Result<Context> {
    let env = std::env::var("EZER_PASS_CONTEXT")
        .ok()
        .filter(|s| !s.is_empty());
    if let Some(name) = named.map(str::to_string).or(env) {
        return find(&name);
    }
    if std::env::var_os("PASSWORD_STORE_DIR").is_some() {
        return find(DEFAULT);
    }
    let last = crate::repl::state_file("context")
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string());
    match last {
        // A context removed from the file since falls back to default.
        Some(name) if !name.is_empty() => find(&name).or_else(|_| find(DEFAULT)),
        _ => find(DEFAULT),
    }
}

/// Remember `name` as the context to open next time.
pub fn remember(name: &str) {
    if let Some(path) = crate::repl::state_file("context") {
        let _ = fs::write(path, format!("{name}\n"));
    }
}

/// A per-context state file: `name` for default (as before contexts
/// existed), `name-<context>` for the others.
pub fn state_file(context: &str, name: &str) -> Option<PathBuf> {
    if context == DEFAULT {
        crate::repl::state_file(name)
    } else {
        crate::repl::state_file(&format!("{name}-{context}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections() {
        let text = "# mine\n[work]\npath = /w\nidentity = /w.age\n\n[personal]\n path=/p \n";
        let contexts = parse(text).unwrap();
        assert_eq!(contexts.len(), 2);
        assert_eq!(contexts[0].name, "work");
        assert_eq!(contexts[0].path, PathBuf::from("/w"));
        assert_eq!(contexts[0].identity, Some(PathBuf::from("/w.age")));
        assert_eq!(contexts[1].name, "personal");
        assert_eq!(contexts[1].path, PathBuf::from("/p"));
        assert_eq!(contexts[1].identity, None);
    }

    #[test]
    fn rejects_bad_files() {
        assert!(parse("path = /x\n").is_err());
        assert!(parse("[a]\n").is_err());
        assert!(parse("[a]\npath = /x\n[a]\npath = /y\n").is_err());
        assert!(parse("[a b]\npath = /x\n").is_err());
        assert!(parse("[a]\ncolor = red\n").is_err());
    }

    #[test]
    fn names() {
        assert!(valid_name("work_2-x"));
        assert!(!valid_name(""));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("a b"));
    }
}
