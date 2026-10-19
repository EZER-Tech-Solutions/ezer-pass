//! `git pull`/`git push` in the background, so the TUI can show a progress
//! popup instead of handing over the terminal.
//!
//! git runs detached from the terminal (`git::detach`): an ssh passphrase
//! or host-key prompt fails instead of drawing over the TUI, and the popup
//! offers to retry with the terminal handed over, where prompts work.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Instant;

/// Lines of git output kept for the popup.
const KEEP: usize = 50;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Pull,
    Push,
}

impl Op {
    pub fn verb(self) -> &'static str {
        match self {
            Op::Pull => "pull",
            Op::Push => "push",
        }
    }

    /// The git arguments, also used for the in-terminal retry. `--ff-only`
    /// so a diverged store fails cleanly instead of wanting a merge commit.
    pub fn args(self) -> &'static [&'static str] {
        match self {
            Op::Pull => &["pull", "--ff-only"],
            Op::Push => &["push"],
        }
    }
}

enum Msg {
    Line(String),
    Done(Result<(), String>),
}

pub struct Sync {
    pub op: Op,
    pub started: Instant,
    /// git's output, progress lines updated in place.
    pub output: Vec<String>,
    /// Set once git has exited unsuccessfully.
    pub failed: bool,
    rx: Receiver<Msg>,
}

impl Sync {
    pub fn start(op: Op, store_root: &Path) -> Self {
        let (tx, rx) = mpsc::channel();
        let root = store_root.to_path_buf();
        thread::spawn(move || {
            let result = run(op, &root, &tx);
            let _ = tx.send(Msg::Done(result));
        });
        Sync {
            op,
            started: Instant::now(),
            output: Vec::new(),
            failed: false,
            rx,
        }
    }

    /// Take in whatever git has sent. `Some(succeeded)` once it is done.
    pub fn poll(&mut self) -> Option<bool> {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Line(line) => self.push_line(line),
                Msg::Done(result) => {
                    if let Err(e) = result {
                        // git's own output already says what went wrong.
                        if self.output.is_empty() {
                            self.push_line(e);
                        }
                        self.failed = true;
                    }
                    return Some(!self.failed);
                }
            }
        }
        None
    }

    fn push_line(&mut self, line: String) {
        // git redraws progress ("Receiving objects:  45% (9/20)") in place.
        let label = |l: &str| l.split_once(':').map(|(k, _)| k.to_string());
        if let Some(last) = self.output.last_mut()
            && last.contains('%')
            && label(last).is_some()
            && label(last) == label(&line)
        {
            *last = line;
            return;
        }
        self.output.push(line);
        if self.output.len() > KEEP {
            self.output.remove(0);
        }
    }
}

fn run(op: Op, store_root: &Path, tx: &Sender<Msg>) -> Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(store_root)
        .args(op.args())
        .arg("--progress")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::git::detach(&mut cmd)
        .spawn()
        .map_err(|e| format!("failed to run `git`: {e}"))?;
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|r| {
        let tx = tx.clone();
        thread::spawn(move || forward(r, &tx))
    })
    .collect();
    let status = child
        .wait()
        .map_err(|e| format!("failed to wait for git: {e}"))?;
    for r in readers {
        let _ = r.join();
    }
    if status.success() {
        Ok(())
    } else {
        Err(format!("git {} failed", op.verb()))
    }
}

/// Send each line of `r` to the popup. Progress output ends its updates
/// with `\r`, so that splits lines too.
fn forward(mut r: Box<dyn Read + Send>, tx: &Sender<Msg>) {
    let mut buf = [0u8; 4096];
    let mut line = Vec::new();
    let send = |line: &mut Vec<u8>| {
        let text = String::from_utf8_lossy(line).trim_end().to_string();
        line.clear();
        if !text.trim().is_empty() {
            let _ = tx.send(Msg::Line(text));
        }
    };
    while let Ok(n) = r.read(&mut buf) {
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if b == b'\n' || b == b'\r' {
                send(&mut line);
            } else {
                line.push(b);
            }
        }
    }
    send(&mut line);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sync() -> Sync {
        let (_tx, rx) = mpsc::channel();
        Sync {
            op: Op::Pull,
            started: Instant::now(),
            output: Vec::new(),
            failed: false,
            rx,
        }
    }

    #[test]
    fn progress_updates_in_place() {
        let mut s = sync();
        s.push_line("From ../remote".into());
        s.push_line("Receiving objects:  10% (1/10)".into());
        s.push_line("Receiving objects:  50% (5/10)".into());
        s.push_line("Receiving objects: 100% (10/10), done.".into());
        s.push_line("Resolving deltas: 100% (2/2)".into());
        assert_eq!(
            s.output,
            [
                "From ../remote",
                "Receiving objects: 100% (10/10), done.",
                "Resolving deltas: 100% (2/2)",
            ]
        );
    }
}
