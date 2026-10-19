//! The clipboard, through wl-copy (Wayland) or xclip (X11). A copied
//! password is cleared after a timeout by a detached copy of ezer-pass
//! that outlives the program, and only if the clipboard still holds it.

use crate::git;
use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;
use zeroize::Zeroizing;

/// The hidden argument that runs ezer-pass as the clearer.
pub const CLEAR_ARG: &str = "__clear-clipboard";

/// Seconds a copied password stays on the clipboard.
pub fn clear_after() -> u64 {
    std::env::var("PASSWORD_STORE_CLIP_TIME")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(45)
}

#[derive(Clone, Copy)]
enum Tool {
    Wayland,
    X11,
}

fn tool() -> Result<Tool> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        Ok(Tool::Wayland)
    } else if std::env::var_os("DISPLAY").is_some() {
        Ok(Tool::X11)
    } else {
        bail!("no clipboard here (needs a Wayland or X11 session)")
    }
}

/// The command that reads stdin onto the clipboard.
fn copy_cmd(tool: Tool) -> Command {
    match tool {
        Tool::Wayland => Command::new("wl-copy"),
        Tool::X11 => xclip(&[]),
    }
}

/// The command that prints the clipboard.
fn paste_cmd(tool: Tool) -> Command {
    match tool {
        Tool::Wayland => {
            let mut c = Command::new("wl-paste");
            c.arg("--no-newline");
            c
        }
        Tool::X11 => xclip(&["-o"]),
    }
}

fn xclip(args: &[&str]) -> Command {
    let mut c = Command::new("xclip");
    c.args(["-selection", "clipboard"]).args(args);
    c
}

fn tool_name(tool: Tool) -> &'static str {
    match tool {
        Tool::Wayland => "wl-copy",
        Tool::X11 => "xclip",
    }
}

fn set(tool: Tool, text: &[u8]) -> Result<()> {
    // Both tools fork a process that serves the clipboard; with a pipe on
    // stdout it would hold the pipe open, so stdout goes nowhere.
    let mut child = copy_cmd(tool)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to run {} (is it installed?)", tool_name(tool)))?;
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(text)?;
    if !child.wait()?.success() {
        bail!("{} failed", tool_name(tool));
    }
    Ok(())
}

fn get(tool: Tool) -> Option<Zeroizing<Vec<u8>>> {
    let out = paste_cmd(tool)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| Zeroizing::new(out.stdout))
}

/// Put `secret` on the clipboard and clear it after `clear_after()`
/// seconds.
pub fn copy(secret: &str) -> Result<()> {
    let tool = tool()?;
    set(tool, secret.as_bytes())?;
    let mut cmd = Command::new(std::env::current_exe().context("cannot find ezer-pass itself")?);
    cmd.arg(CLEAR_ARG)
        .arg(clear_after().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut clearer = git::detach(&mut cmd)
        .spawn()
        .context("cannot start the clipboard clearer")?;
    // The secret goes over a pipe, never argv, so `ps` can't see it.
    clearer
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(secret.as_bytes())?;
    Ok(())
}

/// The clearer: read the secret from stdin, wait, and clear the clipboard
/// if it still holds the secret (not something copied since).
pub fn run_clearer(secs: &str) {
    let Ok(tool) = tool() else { return };
    let mut secret = Zeroizing::new(Vec::new());
    if std::io::stdin().read_to_end(&mut secret).is_err() {
        return;
    }
    std::thread::sleep(Duration::from_secs(secs.parse().unwrap_or(45)));
    if get(tool).is_some_and(|now| *now == *secret) {
        let _ = match tool {
            Tool::Wayland => Command::new("wl-copy").arg("--clear").status().map(|_| ()),
            Tool::X11 => set(tool, b"").map_err(std::io::Error::other),
        };
    }
}
