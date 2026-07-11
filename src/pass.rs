use anyhow::{bail, Context, Result};
use std::io::Write;
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
    let status = pass_cmd()
        .args(args)
        .status()
        .context("failed to run `pass` (is it installed?)")?;
    if !status.success() {
        bail!("pass {} failed", args.first().unwrap_or(&""));
    }
    Ok(())
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
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
}

/// Decrypt an entry and capture its contents. Only stdout is captured: stdin
/// and stderr stay on the terminal so gpg can locate the tty and pinentry
/// (curses or otherwise) can prompt for the passphrase.
pub fn show(path: &str) -> Result<String> {
    let child = pass_cmd()
        .args(["show", "--", path])
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to run `pass` (is it installed?)")?;
    let out = child
        .wait_with_output()
        .context("failed to wait for pass")?;
    if !out.status.success() {
        bail!("pass show failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
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
