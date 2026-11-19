//! Asking for a secret on the terminal: a label on one line, then the
//! input on the next, drawn as bullets so you can see
//! what you've typed without anything being shown. Read from `/dev/tty`,
//! so it works with stdin or stdout piped.

use anyhow::{Context, Result, anyhow, bail};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::sync::OnceLock;
use zeroize::{Zeroize, Zeroizing};

fn color() -> bool {
    static COLOR: OnceLock<bool> = OnceLock::new();
    *COLOR.get_or_init(|| std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()))
}

/// `text` in the SGR style `code`, unless `NO_COLOR` is set.
fn paint(code: &str, text: &str) -> String {
    if color() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

const ACCENT: &str = "1;91";
const SUBJECT: &str = "36";

/// Ask for a secret: `label`, then `subject` (a path) in its own color.
pub fn hidden(label: &str, subject: Option<&str>) -> Result<Zeroizing<String>> {
    let mut tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("no terminal to ask on")?;
    let mut head = paint("1", label);
    if let Some(s) = subject {
        head.push(' ');
        head.push_str(&paint(SUBJECT, s));
    }
    writeln!(tty, "{head}")?;

    let width = bullets_width(&tty);
    let mut field = Field::default();
    let result = {
        let _raw = Raw::enter(&tty)?;
        draw(&mut tty, &field, width)?;
        loop {
            let mut byte = [0u8];
            if tty.read(&mut byte)? == 0 {
                break Err(anyhow!("the terminal closed"));
            }
            match field.feed(byte[0]) {
                Step::Done => break Ok(()),
                Step::Cancel => break Err(anyhow!("cancelled")),
                Step::Changed => draw(&mut tty, &field, width)?,
                Step::Same => {}
            }
        }
    };
    writeln!(tty)?;
    result?;
    field.into_string()
}

/// Redraw the input line.
fn draw(tty: &mut File, field: &Field, width: usize) -> io::Result<()> {
    let n = field.chars();
    let dots = if n > width {
        "•".repeat(width - 1) + "…"
    } else {
        "•".repeat(n)
    };
    write!(
        tty,
        "\r\x1b[2K  {} {}",
        paint(ACCENT, "›"),
        paint("2", &dots)
    )?;
    tty.flush()
}

/// How many bullets fit on the input line without wrapping.
fn bullets_width(tty: &File) -> usize {
    let mut size = MaybeUninit::<libc::winsize>::zeroed();
    // SAFETY: TIOCGWINSZ fills a winsize; on failure it stays zeroed.
    let cols = unsafe {
        libc::ioctl(tty.as_raw_fd(), libc::TIOCGWINSZ, size.as_mut_ptr());
        size.assume_init().ws_col as usize
    };
    match cols {
        0 => 40,
        c => c.saturating_sub(6).clamp(8, 48),
    }
}

/// The terminal with echo and line editing off, put back when dropped.
struct Raw {
    fd: i32,
    saved: libc::termios,
}

impl Raw {
    fn enter(tty: &File) -> Result<Raw> {
        let fd = tty.as_raw_fd();
        let mut t = MaybeUninit::<libc::termios>::uninit();
        // SAFETY: tcgetattr fills the termios when it returns 0.
        let saved = unsafe {
            if libc::tcgetattr(fd, t.as_mut_ptr()) != 0 {
                bail!("can't set up the terminal: {}", io::Error::last_os_error());
            }
            t.assume_init()
        };
        let mut raw = saved;
        // No echo, a byte at a time, and Ctrl-C read as a key so the
        // terminal is always put back.
        raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG | libc::IEXTEN);
        raw.c_iflag &= !(libc::IXON | libc::ICRNL);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: fd is an open terminal and raw a valid termios.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            bail!("can't set up the terminal: {}", io::Error::last_os_error());
        }
        Ok(Raw { fd, saved })
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        // SAFETY: putting back the settings read in `enter`.
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved) };
    }
}

#[derive(Debug, PartialEq)]
enum Step {
    Done,
    Cancel,
    Changed,
    Same,
}

#[derive(Default, PartialEq)]
enum Escape {
    #[default]
    None,
    /// Just read ESC.
    Start,
    /// Inside `ESC [` or `ESC O`: skipped up to its final byte.
    Sequence,
}

/// What's been typed, as bytes until Enter.
struct Field {
    bytes: Zeroizing<Vec<u8>>,
    escape: Escape,
}

impl Default for Field {
    fn default() -> Self {
        // Room up front, so growing doesn't leave copies behind.
        Field {
            bytes: Zeroizing::new(Vec::with_capacity(1024)),
            escape: Escape::None,
        }
    }
}

impl Field {
    fn feed(&mut self, b: u8) -> Step {
        match self.escape {
            Escape::Start => {
                self.escape = if b == b'[' || b == b'O' {
                    Escape::Sequence
                } else {
                    Escape::None
                };
                return Step::Same;
            }
            Escape::Sequence => {
                if (0x40..=0x7e).contains(&b) {
                    self.escape = Escape::None;
                }
                return Step::Same;
            }
            Escape::None => {}
        }
        match b {
            b'\r' | b'\n' => Step::Done,
            // Ctrl-C, and Ctrl-D with nothing typed.
            0x03 => Step::Cancel,
            0x04 if self.bytes.is_empty() => Step::Cancel,
            // Backspace: one character, however many bytes it took.
            0x7f | 0x08 => {
                if self.bytes.is_empty() {
                    return Step::Same;
                }
                while let Some(b) = self.bytes.pop() {
                    if b & 0xc0 != 0x80 {
                        break;
                    }
                }
                Step::Changed
            }
            // Ctrl-U and Ctrl-W: start over.
            0x15 | 0x17 => {
                self.bytes.zeroize();
                Step::Changed
            }
            0x1b => {
                self.escape = Escape::Start;
                Step::Same
            }
            b if b < 0x20 => Step::Same,
            b => {
                self.bytes.push(b);
                Step::Changed
            }
        }
    }

    fn chars(&self) -> usize {
        self.bytes.iter().filter(|&&b| b & 0xc0 != 0x80).count()
    }

    fn into_string(mut self) -> Result<Zeroizing<String>> {
        let bytes = std::mem::take(&mut *self.bytes);
        match String::from_utf8(bytes) {
            Ok(s) => Ok(Zeroizing::new(s)),
            Err(e) => {
                e.into_bytes().zeroize();
                bail!("what was typed isn't valid UTF-8")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(input: &[u8]) -> (Field, Step) {
        let mut f = Field::default();
        let mut last = Step::Same;
        for &b in input {
            last = f.feed(b);
            if matches!(last, Step::Done | Step::Cancel) {
                break;
            }
        }
        (f, last)
    }

    #[test]
    fn enter_finishes() {
        let (f, step) = typed(b"hunter2\r");
        assert_eq!(step, Step::Done);
        assert_eq!(f.into_string().unwrap().as_str(), "hunter2");
    }

    #[test]
    fn backspace_takes_whole_characters() {
        let (f, _) = typed("aé€\x7f\x7f\r".as_bytes());
        assert_eq!(f.chars(), 1);
        assert_eq!(f.into_string().unwrap().as_str(), "a");
    }

    #[test]
    fn ctrl_u_clears_and_arrows_are_ignored() {
        let (f, _) = typed(b"wrong\x15ri\x1b[Dght\x1bOA\r");
        assert_eq!(f.into_string().unwrap().as_str(), "right");
    }

    #[test]
    fn ctrl_c_and_empty_ctrl_d_cancel() {
        assert_eq!(typed(b"abc\x03").1, Step::Cancel);
        assert_eq!(typed(b"\x04").1, Step::Cancel);
        assert_eq!(typed(b"a\x04\r").1, Step::Done);
    }
}
