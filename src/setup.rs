//! Keys and setup: unlocking the identity on the terminal, `init` (an
//! identity and who a folder is encrypted to), and `migrate` (a store made
//! by `pass`, moved from gpg to age once).

use crate::commands::Session;
use crate::context::tilde;
use crate::crypto::{self, Keys, WrongPassphrase};
use crate::git;
use crate::store::{self, RECIPIENTS};
use age::secrecy::SecretString;
use age::x25519::Recipient;
use anyhow::{Context, Result, bail};
use colored::Colorize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use zeroize::Zeroizing;

/// Read the identity, asking for its passphrase on the terminal (three
/// tries) when it has one.
pub fn unlock_on_terminal(path: &Path) -> Result<Keys> {
    if !crypto::needs_passphrase(path)? {
        return crypto::unlock(path, None);
    }
    let mut tries = 0;
    loop {
        let passphrase = SecretString::from(rpassword::prompt_password(format!(
            "passphrase for {}: ",
            tilde(path)
        ))?);
        match crypto::unlock(path, Some(&passphrase)) {
            Err(e) if e.is::<WrongPassphrase>() && tries < 2 => {
                eprintln!("{}", "wrong passphrase, try again".red());
                tries += 1;
            }
            result => return result,
        }
    }
}

/// Ask for a new passphrase twice.
fn new_passphrase() -> Result<SecretString> {
    let first = Zeroizing::new(rpassword::prompt_password(
        "passphrase for your new identity: ",
    )?);
    if first.is_empty() {
        bail!("an empty passphrase would leave your key unprotected; nothing made");
    }
    let again = Zeroizing::new(rpassword::prompt_password("the same passphrase again: ")?);
    if first != again {
        bail!("the passphrases don't match; nothing made");
    }
    Ok(SecretString::from(first.to_string()))
}

/// Make an identity if there isn't one yet, leaving the session unlocked
/// with it.
fn ensure_identity(sess: &mut Session) -> Result<()> {
    let path = sess.context.identity()?;
    if path.exists() {
        return Ok(());
    }
    println!("no identity yet; making one at {}", tilde(&path).cyan());
    let passphrase = new_passphrase()?;
    let public = crypto::create_identity(&path, &passphrase)?;
    sess.keys = Some(crypto::unlock(&path, Some(&passphrase))?);
    println!("your public key is {}", public.to_string().green());
    println!(
        "{} back up {} — without it and its passphrase nothing in the store can be read",
        "important:".yellow().bold(),
        tilde(&path)
    );
    Ok(())
}

/// Write a folder's `.age-recipients`.
fn write_recipients(sess: &Session, dir: &str, recipients: &[Recipient]) -> Result<()> {
    let mut text = format!(
        "# Who entries in {} are encrypted to: age public keys, one per line.\n",
        store::display_dir(dir)
    );
    for r in recipients {
        text.push_str(&format!("{r}\n"));
    }
    let file = sess.store.root().join(dir).join(RECIPIENTS);
    sess.store.write_file(&file, text.as_bytes())
}

/// `init [-p folder] [age1...]`: make an identity if there is none, and
/// set who `folder` (default: the whole store) is encrypted to — the keys
/// given, or your own. Entries already there are re-encrypted to match.
pub fn init(sess: &mut Session, args: &[String]) -> Result<()> {
    if sess.store.uses_gpg() {
        bail!("this store is still encrypted with gpg; run `ezer-pass migrate` instead");
    }
    let mut dir = String::new();
    let mut keys = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-p" | "--path" => {
                let folder = args.next().context("usage: init [-p folder] [age1...]")?;
                dir = sess.resolve(folder);
            }
            a if a.starts_with('-') => bail!("unknown option {a}"),
            a => keys.push(
                Recipient::from_str(a)
                    .map_err(|_| anyhow::anyhow!("{a:?} is not an age public key (age1...)"))?,
            ),
        }
    }
    if sess.store.is_entry(&dir) {
        bail!("{} is an entry", store::display(&dir));
    }
    ensure_identity(sess)?;
    if keys.is_empty() {
        keys = sess.unlock()?.recipients();
    }
    let before = sess.store.recipients(&dir).ok().map(|(_, r)| sorted(&r));
    write_recipients(sess, &dir, &keys)?;
    // Entries this file now governs: not under a deeper `.age-recipients`.
    let mut moved = 0;
    if before.as_ref() != Some(&sorted(&keys)) {
        let entries: Vec<String> = sess
            .store
            .walk()
            .into_iter()
            .filter(|(p, is_dir)| !is_dir && (dir.is_empty() || p.starts_with(&format!("{dir}/"))))
            .map(|(p, _)| p)
            .filter(|p| {
                sess.store
                    .recipients(store::parent(p))
                    .is_ok_and(|(owner, _)| owner == dir)
            })
            .collect();
        for path in &entries {
            let content = sess.read(path)?;
            sess.store.write_entry(path, content.as_bytes())?;
            moved += 1;
        }
    }
    let shown = store::display_dir(&dir);
    let changed = if dir.is_empty() {
        ".".to_string()
    } else {
        dir.clone()
    };
    sess.commit(&[&changed], &format!("Set age recipients for {shown}."))?;
    println!(
        "{shown} is encrypted to {} key{}{}",
        keys.len(),
        if keys.len() == 1 { "" } else { "s" },
        match moved {
            0 => String::new(),
            n => format!(
                "; re-encrypted {n} entr{}",
                if n == 1 { "y" } else { "ies" }
            ),
        }
    );
    Ok(())
}

fn sorted(recipients: &[Recipient]) -> Vec<String> {
    let mut v: Vec<String> = recipients.iter().map(|r| r.to_string()).collect();
    v.sort();
    v.dedup();
    v
}

/// Every file under `dir` (skipping `.git`), relative to `root`.
fn files_under(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if entry.file_name() != ".git" {
                files_under(root, &path, out)?;
            }
        } else {
            out.push(path.strip_prefix(root).expect("under root").to_path_buf());
        }
    }
    Ok(())
}

/// Decrypt one gpg file with gpg itself, which asks for the passphrase
/// through pinentry (once: gpg-agent caches it).
fn gpg_decrypt(file: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let out = Command::new("gpg")
        .args(["--quiet", "--yes", "--batch", "--decrypt", "--"])
        .arg(file)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .context("failed to run gpg (is it installed?)")?;
    let data = Zeroizing::new(out.stdout);
    if !out.status.success() {
        bail!("gpg couldn't decrypt it");
    }
    Ok(data)
}

/// `migrate`: decrypt every entry of a `pass` store with gpg, one last
/// time, and encrypt it with age; drop the `.gpg-id` files; commit it all
/// as one change. Nothing is touched unless every entry decrypts.
pub fn migrate(sess: &mut Session) -> Result<()> {
    let root = sess.store.root().to_path_buf();
    if !sess.store.uses_gpg() {
        bail!("this store already uses age");
    }
    if git::is_repo(&root) {
        let status = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["status", "--porcelain"])
            .output()
            .context("failed to run git")?;
        if !status.stdout.is_empty() {
            bail!("the store has uncommitted changes; commit or discard them first");
        }
    }
    let mut files = Vec::new();
    files_under(&root, &root, &mut files)?;
    let entries: Vec<(PathBuf, String)> = files
        .iter()
        .filter_map(|f| {
            let name = f.to_str()?;
            Some((f.clone(), name.strip_suffix(".gpg")?.to_string()))
        })
        .collect();

    ensure_identity(sess)?;
    let made_recipients = sess.store.recipients("").is_err();
    if made_recipients {
        let keys = sess.unlock()?.recipients();
        write_recipients(sess, "", &keys)?;
    }
    // Check every entry has recipients before decrypting anything.
    for (_, path) in &entries {
        sess.store.recipients(store::parent(path))?;
    }

    println!(
        "decrypting {} entr{} with gpg (it may ask for your gpg passphrase once)",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" }
    );
    let mut plain = Vec::with_capacity(entries.len());
    for (i, (file, path)) in entries.iter().enumerate() {
        eprint!(
            "\r\x1b[K{} {}/{} {}",
            "decrypting".dimmed(),
            i + 1,
            entries.len(),
            store::display(path)
        );
        match gpg_decrypt(&root.join(file)) {
            Ok(data) => plain.push(data),
            Err(e) => {
                eprintln!();
                if made_recipients {
                    let _ = std::fs::remove_file(root.join(RECIPIENTS));
                }
                return Err(e.context(format!(
                    "couldn't decrypt {}; nothing was changed",
                    store::display(path)
                )));
            }
        }
    }
    eprint!("\r\x1b[K");

    // Write every age file before removing any gpg one, so a failure part
    // way leaves the store as it was.
    let mut written: Vec<&str> = Vec::new();
    for ((_, path), data) in entries.iter().zip(&plain) {
        if let Err(e) = sess.store.write_entry(path, data) {
            for p in &written {
                let _ = std::fs::remove_file(sess.store.entry_file(p));
            }
            if made_recipients {
                let _ = std::fs::remove_file(root.join(RECIPIENTS));
            }
            return Err(e.context("nothing was changed"));
        }
        written.push(path);
    }
    drop(plain);
    for (file, _) in &entries {
        std::fs::remove_file(root.join(file))?;
    }
    for file in files
        .iter()
        .filter(|f| f.file_name().is_some_and(|n| n == ".gpg-id"))
    {
        std::fs::remove_file(root.join(file))?;
    }
    // pass set git up to diff entries through gpg.
    let attributes = root.join(".gitattributes");
    let text = std::fs::read_to_string(&attributes).unwrap_or_default();
    let kept: String = text
        .lines()
        .filter(|l| !l.contains("diff=gpg"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&attributes, kept)?;
    git::mark_binary(&root)?;
    sess.commit(&["."], "Move the store from gpg to age.")?;
    println!(
        "moved {} entr{} to age",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" }
    );
    println!(
        "{} your identity is {} — back it up, and copy it to any other machine that uses this store",
        "important:".yellow().bold(),
        tilde(&sess.context.identity()?)
    );
    Ok(())
}
