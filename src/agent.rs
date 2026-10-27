//! The agent: a background copy of ezer-pass that keeps unlocked identities
//! in memory, so a program with no terminal to ask a passphrase on (the
//! browser extension's host, see `browserpass`) can still read the store,
//! and a command run while it holds the identity asks for no passphrase.
//! `ezer-pass unlock` starts it and hands it the passphrase. It decrypts on
//! request, so the keys never leave it, and forgets everything and exits
//! after `idle_timeout()` seconds unused, or on `ezer-pass lock`. It listens
//! on a socket in `$XDG_RUNTIME_DIR` and answers only its own user.

use crate::context::tilde;
use crate::crypto::{self, Keys, WrongPassphrase};
use crate::{git, setup};
use age::secrecy::{ExposeSecret, SecretString};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// The hidden argument that runs ezer-pass as the agent.
pub const AGENT_ARG: &str = "__agent";

/// The largest message either side accepts.
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Nothing to decrypt with: the agent isn't running, or doesn't hold the
/// identity (it timed out, or was locked).
#[derive(Debug)]
pub struct Locked;

impl std::fmt::Display for Locked {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("ezer-pass is locked (run `ezer-pass unlock` in a terminal)")
    }
}

impl std::error::Error for Locked {}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
enum Request {
    Unlock {
        identity: PathBuf,
        passphrase: Zeroizing<String>,
    },
    /// Whether `identity` is unlocked.
    Status {
        identity: PathBuf,
    },
    /// `ciphertext` is base64.
    Decrypt {
        identity: PathBuf,
        ciphertext: String,
    },
    Lock,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case")]
enum Reply {
    Done,
    /// Base64.
    Plaintext {
        data: Zeroizing<String>,
    },
    Locked,
    WrongPassphrase,
    Failed {
        message: String,
    },
}

/// Seconds unused before the agent forgets the keys; 0 keeps them until
/// `ezer-pass lock`.
pub fn idle_timeout() -> u64 {
    std::env::var("EZER_PASS_AGENT_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(900)
}

fn socket_path() -> Result<PathBuf> {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir())
        .or_else(|| Some(PathBuf::from(format!("/run/user/{uid}"))).filter(|dir| dir.is_dir()))
        .context("no $XDG_RUNTIME_DIR to put the agent's socket in")?;
    Ok(dir.join("ezer-pass-agent.sock"))
}

/// Read one message: its length (4 bytes, native byte order), then it.
/// Native messaging frames messages the same way.
pub fn read_frame(input: &mut impl Read) -> Result<Zeroizing<Vec<u8>>> {
    let mut len = [0u8; 4];
    input.read_exact(&mut len)?;
    let len = u32::from_ne_bytes(len) as usize;
    if len > MAX_FRAME {
        bail!("message of {len} bytes is too long");
    }
    let mut buf = Zeroizing::new(vec![0; len]);
    input.read_exact(&mut buf)?;
    Ok(buf)
}

pub fn write_frame(output: &mut impl Write, data: &[u8]) -> Result<()> {
    let len = u32::try_from(data.len()).context("message too long")?;
    output.write_all(&len.to_ne_bytes())?;
    output.write_all(data)?;
    output.flush()?;
    Ok(())
}

/// The same identity file spelled the same way, however it was reached.
fn key_for(identity: &Path) -> PathBuf {
    fs::canonicalize(identity).unwrap_or_else(|_| identity.to_path_buf())
}

fn connect() -> Option<UnixStream> {
    UnixStream::connect(socket_path().ok()?).ok()
}

fn exchange(mut stream: UnixStream, request: &Request) -> Result<Reply> {
    // Unlocking runs scrypt, which takes a moment.
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    write_frame(&mut stream, &Zeroizing::new(serde_json::to_vec(request)?))?;
    let reply = read_frame(&mut stream).context("no answer from the agent")?;
    serde_json::from_slice(&reply).context("the agent's answer is garbled")
}

/// Decrypt `ciphertext` with the agent's copy of `identity`.
pub fn decrypt(identity: &Path, ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let stream = connect().ok_or(Locked)?;
    let request = Request::Decrypt {
        identity: key_for(identity),
        ciphertext: STANDARD.encode(ciphertext),
    };
    match exchange(stream, &request)? {
        Reply::Plaintext { data } => Ok(Zeroizing::new(
            STANDARD
                .decode(data.as_bytes())
                .context("the agent's answer is garbled")?,
        )),
        Reply::Locked => Err(anyhow!(Locked)),
        Reply::Failed { message } => Err(anyhow!(message)),
        _ => bail!("unexpected answer from the agent"),
    }
}

/// Whether a running agent holds `identity` unlocked; asking counts as a use.
pub fn holds(identity: &Path) -> bool {
    let Some(stream) = connect() else {
        return false;
    };
    let status = Request::Status {
        identity: key_for(identity),
    };
    matches!(exchange(stream, &status), Ok(Reply::Done))
}

/// `ezer-pass unlock`: start the agent if it isn't running, and unlock
/// `identity` in it, asking for the passphrase.
pub fn unlock(identity: &Path) -> Result<()> {
    if !crypto::needs_passphrase(identity)? {
        println!(
            "{} has no passphrase, so there is nothing to unlock",
            tilde(identity)
        );
        return Ok(());
    }
    let key = key_for(identity);
    if let Some(stream) = connect() {
        let status = Request::Status {
            identity: key.clone(),
        };
        if matches!(exchange(stream, &status)?, Reply::Done) {
            println!("{} is already unlocked", tilde(identity));
            return Ok(());
        }
    }
    setup::ask_passphrase(identity, |passphrase| {
        let request = Request::Unlock {
            identity: key.clone(),
            passphrase: Zeroizing::new(passphrase.expose_secret().to_owned()),
        };
        match exchange(start()?, &request)? {
            Reply::Done => Ok(()),
            Reply::WrongPassphrase => Err(anyhow!(WrongPassphrase)),
            Reply::Failed { message } => Err(anyhow!(message)),
            _ => bail!("unexpected answer from the agent"),
        }
    })?;
    match idle_timeout() {
        0 => println!("unlocked until `ezer-pass lock`"),
        secs => println!(
            "unlocked; it locks again after {} unused (or on `ezer-pass lock`)",
            minutes(secs)
        ),
    }
    Ok(())
}

fn minutes(secs: u64) -> String {
    match secs {
        s if s % 60 != 0 => format!("{s}s"),
        60 => "1 minute".into(),
        s => format!("{} minutes", s / 60),
    }
}

/// A connection to the agent, starting it first if it isn't running.
fn start() -> Result<UnixStream> {
    if let Some(stream) = connect() {
        return Ok(stream);
    }
    let mut cmd = Command::new(std::env::current_exe().context("cannot find ezer-pass itself")?);
    cmd.arg(AGENT_ARG)
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut agent = git::detach(&mut cmd)
        .spawn()
        .context("cannot start the agent")?;
    // The agent closes its stderr once it is listening, or says why it
    // can't before exiting.
    let mut why = String::new();
    agent
        .stderr
        .take()
        .expect("stderr was piped")
        .read_to_string(&mut why)?;
    if !why.trim().is_empty() {
        bail!("the agent didn't start: {}", why.trim());
    }
    connect().context("the agent didn't start")
}

/// `ezer-pass lock`: make the agent forget every key and exit.
pub fn lock() -> Result<()> {
    match connect() {
        Some(stream) => {
            exchange(stream, &Request::Lock)?;
            println!("locked");
        }
        None => println!("not unlocked"),
    }
    Ok(())
}

struct State {
    keys: HashMap<PathBuf, Keys>,
    last_used: Instant,
}

/// Run as the agent until locked or unused for `idle_timeout()`. An error
/// means it never started listening.
pub fn serve() -> Result<()> {
    // Keep the keys out of core dumps, and other processes from reading
    // them with ptrace.
    // SAFETY: prctl with PR_SET_DUMPABLE takes no pointers.
    unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) };
    let path = socket_path()?;
    if UnixStream::connect(&path).is_ok() {
        // Another agent is already serving.
        return Ok(());
    }
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("cannot listen on {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    // Listening: tell `start` by closing stderr.
    let null = fs::OpenOptions::new().write(true).open("/dev/null")?;
    // SAFETY: both are open descriptors; dup2 replaces fd 2 atomically.
    unsafe { libc::dup2(null.as_raw_fd(), 2) };
    let state = Arc::new(Mutex::new(State {
        keys: HashMap::new(),
        last_used: Instant::now(),
    }));
    let timeout = idle_timeout();
    if timeout > 0 {
        let state = Arc::clone(&state);
        let path = path.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let idle = state.lock().expect("not poisoned").last_used.elapsed();
                if idle >= Duration::from_secs(timeout) {
                    let _ = fs::remove_file(&path);
                    std::process::exit(0);
                }
            }
        });
    }
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if peer_uid(&stream) != Some(uid) {
            continue;
        }
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        let request = read_frame(&mut stream)
            .and_then(|buf| serde_json::from_slice(&buf).context("garbled request"));
        let lock = matches!(request, Ok(Request::Lock));
        let reply = match request {
            Ok(request) => answer(request, &mut state.lock().expect("not poisoned")),
            Err(e) => Reply::Failed {
                message: format!("{e:#}"),
            },
        };
        if let Ok(reply) = serde_json::to_vec(&reply) {
            let _ = write_frame(&mut stream, &Zeroizing::new(reply));
        }
        if lock {
            let _ = fs::remove_file(&path);
            return Ok(());
        }
    }
    Ok(())
}

fn answer(request: Request, state: &mut State) -> Reply {
    match request {
        Request::Lock => Reply::Done,
        Request::Status { identity } => {
            if state.keys.contains_key(&identity) {
                state.last_used = Instant::now();
                Reply::Done
            } else {
                Reply::Locked
            }
        }
        Request::Unlock {
            identity,
            passphrase,
        } => {
            let passphrase = SecretString::from(passphrase.as_str().to_owned());
            match crypto::unlock(&identity, Some(&passphrase)) {
                Ok(keys) => {
                    state.keys.insert(identity, keys);
                    state.last_used = Instant::now();
                    Reply::Done
                }
                Err(e) if e.is::<WrongPassphrase>() => Reply::WrongPassphrase,
                Err(e) => Reply::Failed {
                    message: format!("{e:#}"),
                },
            }
        }
        Request::Decrypt {
            identity,
            ciphertext,
        } => {
            let Some(keys) = state.keys.get(&identity) else {
                return Reply::Locked;
            };
            state.last_used = Instant::now();
            let plain = STANDARD
                .decode(ciphertext)
                .context("garbled request")
                .and_then(|ciphertext| crypto::decrypt(keys, &ciphertext));
            match plain {
                Ok(plain) => Reply::Plaintext {
                    data: Zeroizing::new(STANDARD.encode(&plain[..])),
                },
                Err(e) => Reply::Failed {
                    message: format!("{e:#}"),
                },
            }
        }
    }
}

/// The user id of the process at the other end of `stream`.
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred and len are valid for getsockopt to write, and len is
    // cred's size.
    let r = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (r == 0).then_some(cred.uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{\"op\":\"lock\"}").unwrap();
        assert_eq!(buf.len(), 4 + 13);
        let read = read_frame(&mut &buf[..]).unwrap();
        assert_eq!(&read[..], b"{\"op\":\"lock\"}");
        let mut huge = (MAX_FRAME as u32 + 1).to_ne_bytes().to_vec();
        huge.extend_from_slice(b"x");
        assert!(read_frame(&mut &huge[..]).is_err());
    }

    #[test]
    fn decrypts_only_unlocked_identities() {
        let keys = Keys::generate();
        let ciphertext = crypto::encrypt(&keys.recipients(), b"hunter2\n").unwrap();
        let mut state = State {
            keys: HashMap::new(),
            last_used: Instant::now(),
        };
        let request = || Request::Decrypt {
            identity: PathBuf::from("/id"),
            ciphertext: STANDARD.encode(&ciphertext),
        };
        assert!(matches!(answer(request(), &mut state), Reply::Locked));
        state.keys.insert(PathBuf::from("/id"), keys);
        match answer(request(), &mut state) {
            Reply::Plaintext { data } => {
                assert_eq!(STANDARD.decode(data.as_bytes()).unwrap(), b"hunter2\n")
            }
            _ => panic!("expected plaintext"),
        }
    }
}
