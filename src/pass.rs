use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// All GPG/git/clipboard work is delegated to the `pass` binary. Store paths
/// handed to these functions must already be store-absolute (cwd applied).
fn pass_cmd() -> Command {
    Command::new("pass")
}

/// Run pass with inherited stdio, for anything interactive: pinentry, editor,
/// rm confirmation, clipboard messages. `args` must include the subcommand.
pub fn run_interactive(args: &[&str]) -> Result<()> {
    run_interactive_env(args, &[])
}

/// Same as `run_interactive`, with extra environment variables set (e.g.
/// `PASSWORD_STORE_CHARACTER_SET` to steer `pass generate`'s charset).
pub fn run_interactive_env(args: &[&str], env: &[(&str, &str)]) -> Result<()> {
    let status = pass_cmd()
        .args(args)
        .envs(env.iter().copied())
        .status()
        .context("failed to run `pass` (is it installed?)")?;
    if !status.success() {
        bail!("pass {} failed", args.first().unwrap_or(&""));
    }
    Ok(())
}

/// Run git in the store with inherited stdio. Used instead of `pass git`,
/// which exits 0 even when git fails (except for `pass git init`, which also
/// sets the repo up for pass and is still sent through pass).
pub fn git_interactive(store_root: &Path, args: &[&str]) -> Result<()> {
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
fn git_quiet(store_root: &Path, args: &[&str]) -> Result<String> {
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

/// The store's current commit, if it is a git repo with one.
pub fn git_head(store_root: &Path) -> Option<String> {
    git_quiet(store_root, &["rev-parse", "--verify", "-q", "HEAD"]).ok()
}

/// Make one change to the store as a new commit: `stage` puts it in the
/// index and work tree, then it is committed with `message`. Refuses if the
/// store has uncommitted changes, and on any failure (a conflict with later
/// changes, nothing to change, a signing prompt it can't show) puts the
/// store back exactly as it was.
fn git_commit_change(
    store_root: &Path,
    message: &str,
    stage: impl FnOnce() -> Result<String>,
) -> Result<()> {
    let status = git_quiet(
        store_root,
        &["status", "--porcelain", "--untracked-files=no"],
    )?;
    if !status.is_empty() {
        bail!("the store has uncommitted changes");
    }
    let head = git_head(store_root).context("the store has no commits")?;
    let result = stage().and_then(|_| git_quiet(store_root, &["commit", "-q", "-m", message]));
    if result.is_err() {
        let _ = git_quiet(store_root, &["revert", "--quit"]);
        let _ = git_quiet(store_root, &["reset", "-q", "--hard", &head]);
    }
    result.map(|_| ())
}

/// Undo one commit with a new one.
pub fn git_undo(store_root: &Path, hash: &str, message: &str) -> Result<()> {
    git_commit_change(store_root, message, || {
        git_quiet(store_root, &["revert", "--no-commit", hash])
    })
}

/// Put every file back as it was at commit `hash`, as a new commit.
pub fn git_restore(store_root: &Path, hash: &str, message: &str) -> Result<()> {
    git_commit_change(store_root, message, || {
        git_quiet(store_root, &["read-tree", "--reset", "-u", hash])
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
}

/// The newest `limit` commits of the store, newest first.
pub fn git_log(store_root: &Path, limit: usize) -> Result<Vec<Commit>> {
    let out = git_quiet(
        store_root,
        &[
            "log",
            &format!("-n{limit}"),
            "--format=%H%x1f%h%x1f%ct%x1f%an%x1f%P%x1f%s",
        ],
    )?;
    Ok(out
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.splitn(6, '\x1f').collect();
            let [hash, short, time, author, parents, subject] = f[..] else {
                return None;
            };
            Some(Commit {
                hash: hash.into(),
                short: short.into(),
                time: time.parse().unwrap_or(0),
                author: author.into(),
                parents: parents.split_whitespace().count(),
                subject: subject.into(),
            })
        })
        .collect())
}

/// Commits not yet on the upstream; empty without one.
pub fn git_unpushed(store_root: &Path) -> HashSet<String> {
    git_quiet(store_root, &["rev-list", "@{upstream}..HEAD"])
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
pub fn git_changes(store_root: &Path, commit: &Commit) -> Result<Vec<FileChange>> {
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
    let out = git_quiet(store_root, &args)?;
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
pub fn git_upstream(store_root: &Path) -> Option<String> {
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
pub fn git_ahead_count(store_root: &Path) -> u64 {
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

/// Decrypt an entry and capture its contents.
pub fn show(path: &str) -> Result<String> {
    capture(&["show", "--", path])
}

/// Run pass and capture its stdout. Only stdout is captured: stdin and stderr
/// stay on the terminal so gpg can locate the tty and pinentry (curses or
/// otherwise) can prompt for the passphrase.
pub fn capture(args: &[&str]) -> Result<String> {
    let child = pass_cmd()
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to run `pass` (is it installed?)")?;
    let out = child
        .wait_with_output()
        .context("failed to wait for pass")?;
    if !out.status.success() {
        bail!("pass {} failed", args.first().unwrap_or(&""));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A pass command whose gpg may not prompt: with no cached passphrase (or a
/// smartcard wanting its PIN) gpg fails instead of starting pinentry. pass
/// splits `PASSWORD_STORE_GPG_OPTS` into gpg's arguments, so the extra flags
/// ride along with any the user already set. Lets the TUI decrypt without
/// giving up the terminal whenever gpg-agent has the passphrase cached.
fn no_prompt_cmd(args: &[&str]) -> Command {
    let mut opts = std::env::var("PASSWORD_STORE_GPG_OPTS").unwrap_or_default();
    opts.push_str(" --batch --pinentry-mode error");
    let mut cmd = pass_cmd();
    cmd.args(args)
        .env("PASSWORD_STORE_GPG_OPTS", opts.trim_start())
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

/// `capture` that fails rather than prompting; see `no_prompt_cmd`.
pub fn capture_no_prompt(args: &[&str]) -> Result<String> {
    let out = no_prompt_cmd(args)
        .output()
        .context("failed to run `pass` (is it installed?)")?;
    if !out.status.success() {
        bail!("pass {} needs a passphrase", args.first().unwrap_or(&""));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run pass, discarding its output, failing rather than prompting; see
/// `no_prompt_cmd`. stdout goes to /dev/null instead of a pipe because
/// `pass show -c` can leave a clipboard daemon (wl-copy) holding it open.
pub fn run_no_prompt(args: &[&str]) -> Result<()> {
    let status = no_prompt_cmd(args)
        .stdout(Stdio::null())
        .status()
        .context("failed to run `pass` (is it installed?)")?;
    if !status.success() {
        bail!("pass {} needs a passphrase", args.first().unwrap_or(&""));
    }
    Ok(())
}

/// Write an entry by piping `content` to `pass insert -m -f` on stdin, so the
/// secret never appears in argv or a shell. Suppresses pass's interactive
/// "Enter contents..." banner; the caller prints its own confirmation.
pub fn insert_multiline(path: &str, content: &[u8]) -> Result<()> {
    let mut child = pass_cmd()
        .args(["insert", "-m", "-f", "--", path])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("failed to run `pass` (is it installed?)")?;
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(content)
        .context("failed to write to pass stdin")?;
    let status = child.wait().context("failed to wait for pass")?;
    if !status.success() {
        bail!("pass insert failed");
    }
    Ok(())
}
