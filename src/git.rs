//! git in the store: every change is a commit, which is what the history
//! view and sync are built on.

use crate::store::EXT;
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Run git in the store with inherited stdio, for anything that may print
/// or prompt (`:git log`, pull and push with an ssh passphrase).
pub fn interactive(store_root: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(store_root)
        .args(args)
        .status()
        .context("failed to run `git` (is it installed?)")?;
    if !status.success() {
        bail!("git {} failed", args.first().unwrap_or(&""));
    }
    Ok(())
}

/// Run `cmd` in its own session, with no controlling terminal, so nothing
/// it starts (an ssh passphrase prompt, pinentry-curses for a signed
/// commit) can draw over the TUI or read its keys. Prompts fail instead;
/// GUI helpers (`SSH_ASKPASS`, a graphical pinentry) still work.
pub fn detach(cmd: &mut Command) -> &mut Command {
    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        })
    }
}

/// Run git in the store with no terminal (see `detach`), capturing stdout.
/// On failure the error is git's first line of complaint.
fn quiet(store_root: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(store_root)
        .args(args)
        .stdin(Stdio::null());
    let out = detach(&mut cmd)
        .output()
        .context("failed to run `git` (is it installed?)")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let why = stderr
            .lines()
            .chain(stdout.lines())
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("hint:"))
            .unwrap_or("")
            .to_string();
        bail!("git {} failed: {why}", args.first().unwrap_or(&""));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn is_repo(store_root: &Path) -> bool {
    store_root.join(".git").exists()
}

/// Commit a change ezer-pass just made to `paths` (store-relative files or
/// folders, added, changed or removed). Does nothing outside a git repo, or
/// if nothing actually changed.
pub fn commit(store_root: &Path, paths: &[&str], message: &str) -> Result<()> {
    if !is_repo(store_root) {
        return Ok(());
    }
    // git refuses a path it has never seen that isn't on disk either (an
    // empty folder that was removed), so those are left out.
    let mut known = Vec::new();
    for path in paths {
        let tracked = quiet(store_root, &["ls-files", "--", path]).is_ok_and(|o| !o.is_empty());
        if tracked || store_root.join(path).exists() {
            known.push(*path);
        }
    }
    if known.is_empty() {
        return Ok(());
    }
    quiet(store_root, &with_paths(&["add", "-A"], &known))?;
    if quiet(
        store_root,
        &with_paths(&["diff", "--cached", "--quiet"], &known),
    )
    .is_ok()
    {
        return Ok(());
    }
    quiet(
        store_root,
        &with_paths(&["commit", "-q", "-m", message], &known),
    )
    .map(|_| ())
}

/// `args -- paths`.
fn with_paths<'a>(args: &[&'a str], paths: &[&'a str]) -> Vec<&'a str> {
    let mut v = args.to_vec();
    v.push("--");
    v.extend(paths);
    v
}

/// Tell git that entries are binary, so it never tries to diff or merge
/// them line by line (a merge would corrupt them).
pub fn mark_binary(store_root: &Path) -> Result<()> {
    let line = format!("*{EXT} binary");
    let file = store_root.join(".gitattributes");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    if !text.lines().any(|l| l.trim() == line) {
        std::fs::write(&file, format!("{text}{line}\n")).context("cannot write .gitattributes")?;
    }
    Ok(())
}

/// Make the store a git repo holding everything already in it.
pub fn init(store_root: &Path) -> Result<()> {
    if is_repo(store_root) {
        bail!("the store is already a git repo");
    }
    quiet(store_root, &["init", "-q"])?;
    mark_binary(store_root)?;
    quiet(store_root, &["add", "-A"])?;
    quiet(
        store_root,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "Add current contents of password store.",
        ],
    )
    .map(|_| ())
}

/// The store's current commit, if it is a git repo with one.
pub fn head(store_root: &Path) -> Option<String> {
    quiet(store_root, &["rev-parse", "--verify", "-q", "HEAD"]).ok()
}

/// Make one change to the store as a new commit: `stage` puts it in the
/// index and work tree, then it is committed with `message`. Refuses if the
/// store has uncommitted changes, and on any failure (a conflict with later
/// changes, nothing to change, a signing prompt it can't show) puts the
/// store back exactly as it was.
fn commit_change(
    store_root: &Path,
    message: &str,
    stage: impl FnOnce() -> Result<String>,
) -> Result<()> {
    let status = quiet(
        store_root,
        &["status", "--porcelain", "--untracked-files=no"],
    )?;
    if !status.is_empty() {
        bail!("the store has uncommitted changes");
    }
    let head = head(store_root).context("the store has no commits")?;
    let result = stage()
        .and_then(|_| no_gpg_entries(store_root))
        .and_then(|_| quiet(store_root, &["commit", "-q", "-m", message]));
    if result.is_err() {
        let _ = quiet(store_root, &["revert", "--quit"]);
        let _ = quiet(store_root, &["reset", "-q", "--hard", &head]);
    }
    result.map(|_| ())
}

/// Refuse a change that would bring back entries from before the store
/// moved to age: nothing can read them any more.
fn no_gpg_entries(store_root: &Path) -> Result<String> {
    if !quiet(store_root, &["ls-files", "--", "*.gpg"])?.is_empty() {
        bail!("that would bring back gpg entries from before the store moved to age");
    }
    Ok(String::new())
}

/// Undo one commit with a new one.
pub fn undo(store_root: &Path, hash: &str, message: &str) -> Result<()> {
    let parent = format!("{hash}^");
    if !quiet(
        store_root,
        &["diff", "--name-only", &parent, hash, "--", "*.gpg"],
    )?
    .is_empty()
    {
        bail!("that change touches gpg entries from before the move to age; it can't be undone");
    }
    commit_change(store_root, message, || {
        quiet(store_root, &["revert", "--no-commit", hash])
    })
}

/// Put every file back as it was at commit `hash`, as a new commit.
pub fn restore(store_root: &Path, hash: &str, message: &str) -> Result<()> {
    commit_change(store_root, message, || {
        quiet(store_root, &["read-tree", "--reset", "-u", hash])
    })
}

pub struct Commit {
    pub hash: String,
    pub short: String,
    /// Commit time, seconds since the epoch.
    pub time: u64,
    pub author: String,
    pub parents: usize,
    pub subject: String,
    /// `git log --graph`'s drawing on the commit's own line ("| * "), and
    /// the connector-only lines ("|/") that follow it.
    pub graph: String,
    pub after: Vec<String>,
}

/// The newest `limit` commits of the store, newest first, with git's
/// graph drawing.
pub fn log(store_root: &Path, limit: usize) -> Result<Vec<Commit>> {
    let out = quiet(
        store_root,
        &[
            "log",
            "--graph",
            &format!("-n{limit}"),
            "--format=%x1f%H%x1f%h%x1f%ct%x1f%an%x1f%P%x1f%s",
        ],
    )?;
    let mut commits: Vec<Commit> = Vec::new();
    for line in out.lines() {
        let f: Vec<&str> = line.splitn(7, '\x1f').collect();
        let [graph, hash, short, time, author, parents, subject] = f[..] else {
            // A connector-only line belongs under the commit above it.
            if let Some(last) = commits.last_mut() {
                last.after.push(line.trim_end().to_string());
            }
            continue;
        };
        commits.push(Commit {
            hash: hash.into(),
            short: short.into(),
            time: time.parse().unwrap_or(0),
            author: author.into(),
            parents: parents.split_whitespace().count(),
            subject: subject.into(),
            graph: graph.trim_end().to_string(),
            after: Vec::new(),
        });
    }
    Ok(commits)
}

/// The newest `limit` commits touching `pathspec`, newest first, as
/// (commit time, subject). `follow` tracks a single file across renames.
pub fn path_log(
    store_root: &Path,
    pathspec: &str,
    follow: bool,
    limit: usize,
) -> Result<Vec<(u64, String)>> {
    let limit = format!("-n{limit}");
    let mut args = vec!["log", &limit, "--format=%ct%x1f%s"];
    if follow {
        args.push("--follow");
    }
    args.extend(["--", pathspec]);
    let out = quiet(store_root, &args)?;
    Ok(out
        .lines()
        .filter_map(|line| {
            let (time, subject) = line.split_once('\x1f')?;
            Some((time.parse().unwrap_or(0), subject.to_string()))
        })
        .collect())
}

/// Commits not yet on the upstream; empty without one.
pub fn unpushed(store_root: &Path) -> HashSet<String> {
    quiet(store_root, &["rev-list", "@{upstream}..HEAD"])
        .map(|out| out.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// One file a commit touched: `status` is git's letter (A, D, M, R, ...),
/// and `to` the new name of a rename.
pub struct FileChange {
    pub status: char,
    pub path: String,
    pub to: Option<String>,
}

/// What `commit` changed, against its first parent.
pub fn changes(store_root: &Path, commit: &Commit) -> Result<Vec<FileChange>> {
    let parent = format!("{}^", commit.hash);
    let args: Vec<&str> = if commit.parents == 0 {
        vec![
            "diff-tree",
            "-r",
            "-z",
            "-M",
            "--name-status",
            "--root",
            "--no-commit-id",
            &commit.hash,
        ]
    } else {
        vec!["diff", "-z", "-M", "--name-status", &parent, &commit.hash]
    };
    let out = quiet(store_root, &args)?;
    let mut fields = out.split('\0').filter(|f| !f.is_empty());
    let mut changes = Vec::new();
    while let Some(code) = fields.next() {
        let status = code.chars().next().unwrap_or('?');
        let Some(path) = fields.next() else { break };
        let to = if matches!(status, 'R' | 'C') {
            fields.next().map(str::to_string)
        } else {
            None
        };
        changes.push(FileChange {
            status,
            path: path.to_string(),
            to,
        });
    }
    Ok(changes)
}

/// The store's upstream branch (e.g. "origin/master"), if the store is a git
/// repo with a configured upstream. `None` means pull/push would only error,
/// so auto-sync should stay out of the way.
pub fn upstream(store_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["-C"])
        .arg(store_root)
        .args(["rev-parse", "--abbrev-ref", "@{upstream}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let upstream = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!upstream.is_empty()).then_some(upstream)
}

/// How many local commits the store is ahead of its upstream; 0 on any failure.
pub fn ahead_count(store_root: &Path) -> u64 {
    let Ok(out) = Command::new("git")
        .args(["-C"])
        .arg(store_root)
        .args(["rev-list", "--count", "@{upstream}..HEAD"])
        .output()
    else {
        return 0;
    };
    if !out.status.success() {
        return 0;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}
