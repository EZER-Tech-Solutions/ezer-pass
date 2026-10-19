//! age encryption for the store. Entries are encrypted to the recipients
//! (public keys) listed in the nearest `.age-recipients`; reading them takes
//! your identity (private key), kept in its own file under a passphrase and
//! unlocked once per session.

use age::secrecy::{ExposeSecret, SecretString};
use age::x25519;
use anyhow::{Context, Result, anyhow, bail};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::str::FromStr;
use zeroize::Zeroizing;

/// Where the identity lives: `$EZER_PASS_IDENTITY`, else
/// `$XDG_CONFIG_HOME/ezer-pass/identity.age`.
pub fn identity_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("EZER_PASS_IDENTITY") {
        return Ok(PathBuf::from(path));
    }
    let config = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    Ok(config.join("ezer-pass").join("identity.age"))
}

/// The passphrase didn't open the identity.
#[derive(Debug)]
pub struct WrongPassphrase;

impl std::fmt::Display for WrongPassphrase {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("wrong passphrase")
    }
}

impl std::error::Error for WrongPassphrase {}

/// The unlocked identity, held for the rest of the session.
pub struct Keys {
    identities: Vec<x25519::Identity>,
}

impl Keys {
    /// A fresh throwaway identity, for tests.
    #[cfg(test)]
    pub fn generate() -> Self {
        Keys {
            identities: vec![x25519::Identity::generate()],
        }
    }

    /// The public keys that go with the identity.
    pub fn recipients(&self) -> Vec<x25519::Recipient> {
        self.identities.iter().map(|i| i.to_public()).collect()
    }
}

/// An age file, armored or not (a plain identity file is neither).
fn is_age_file(data: &[u8]) -> bool {
    data.starts_with(b"age-encryption.org/")
        || data.starts_with(b"-----BEGIN AGE ENCRYPTED FILE-----")
}

fn read_identity_file() -> Result<(PathBuf, Zeroizing<Vec<u8>>)> {
    let path = identity_path()?;
    match fs::read(&path) {
        Ok(data) => Ok((path, Zeroizing::new(data))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "no identity at {} (run `ezer-pass init` to make one, or set EZER_PASS_IDENTITY)",
            path.display()
        ),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Whether unlocking takes a passphrase: false for a plain key file.
pub fn needs_passphrase() -> Result<bool> {
    Ok(is_age_file(&read_identity_file()?.1))
}

/// Read the identity, decrypting it with `passphrase` if it has one.
pub fn unlock(passphrase: Option<&SecretString>) -> Result<Keys> {
    let (path, data) = read_identity_file()?;
    let text = if is_age_file(&data) {
        let passphrase = passphrase.context("the identity needs its passphrase")?;
        let identity = age::scrypt::Identity::new(passphrase.clone());
        decrypt_with(&identity, &data).map_err(|e| match e.downcast_ref() {
            Some(age::DecryptError::DecryptionFailed | age::DecryptError::NoMatchingKeys) => {
                anyhow!(WrongPassphrase)
            }
            _ => e.context(format!("cannot decrypt {}", path.display())),
        })?
    } else {
        data
    };
    let text = std::str::from_utf8(&text).context("the identity file is not text")?;
    let mut identities = Vec::new();
    for (n, line) in text.lines().enumerate().map(|(n, l)| (n + 1, l.trim())) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let identity = x25519::Identity::from_str(line).map_err(|_| {
            anyhow!(
                "{} line {n}: not an age secret key (AGE-SECRET-KEY-1...)",
                path.display()
            )
        })?;
        identities.push(identity);
    }
    if identities.is_empty() {
        bail!("{} holds no keys", path.display());
    }
    Ok(Keys { identities })
}

/// Make a new identity under `passphrase`, returning its public key.
/// Refuses to replace one that exists.
pub fn create_identity(passphrase: &SecretString) -> Result<x25519::Recipient> {
    let path = identity_path()?;
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    let key = x25519::Identity::generate();
    let public = key.to_public();
    let plain = Zeroizing::new(format!(
        "# ezer-pass identity\n# public key: {public}\n{}\n",
        key.to_string().expose_secret()
    ));
    let mut out = Vec::new();
    let armored = age::armor::ArmoredWriter::wrap_output(&mut out, age::armor::Format::AsciiArmor)?;
    let mut writer =
        age::Encryptor::with_user_passphrase(passphrase.clone()).wrap_output(armored)?;
    writer.write_all(plain.as_bytes())?;
    writer.finish()?.finish()?;
    if let Some(dir) = path.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut f| f.write_all(&out))
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(public)
}

/// Parse a `.age-recipients` file: one public key per line, `#` comments.
pub fn parse_recipients(text: &str) -> Result<Vec<x25519::Recipient>> {
    let mut recipients = Vec::new();
    for (n, line) in text.lines().enumerate().map(|(n, l)| (n + 1, l.trim())) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let recipient = x25519::Recipient::from_str(line)
            .map_err(|_| anyhow!("line {n}: {line:?} is not an age public key (age1...)"))?;
        recipients.push(recipient);
    }
    Ok(recipients)
}

pub fn encrypt(recipients: &[x25519::Recipient], plaintext: &[u8]) -> Result<Vec<u8>> {
    let encryptor =
        age::Encryptor::with_recipients(recipients.iter().map(|r| r as &dyn age::Recipient))?;
    let mut out = Vec::with_capacity(plaintext.len() + 256);
    let mut writer = encryptor.wrap_output(&mut out)?;
    writer.write_all(plaintext)?;
    writer.finish()?;
    Ok(out)
}

pub fn decrypt(keys: &Keys, ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let identities = keys.identities.iter().map(|i| i as &dyn age::Identity);
    decrypt_any(identities, ciphertext).map_err(|e| match e.downcast_ref() {
        Some(age::DecryptError::NoMatchingKeys) => anyhow!("not encrypted to your identity"),
        _ => e,
    })
}

fn decrypt_with(identity: &dyn age::Identity, ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    decrypt_any(std::iter::once(identity), ciphertext)
}

fn decrypt_any<'a>(
    identities: impl Iterator<Item = &'a dyn age::Identity>,
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let decryptor = age::Decryptor::new_buffered(age::armor::ArmoredReader::new(ciphertext))?;
    let mut reader = decryptor.decrypt(identities)?;
    let mut plain = Zeroizing::new(Vec::new());
    reader.read_to_end(&mut plain)?;
    Ok(plain)
}

/// A password of `length` characters drawn uniformly from `chars` (ASCII).
pub fn random_password(chars: &str, length: usize) -> Result<Zeroizing<String>> {
    let chars = chars.as_bytes();
    // Bytes at or past the last whole multiple of the set's size would
    // favour its first characters, so they are thrown away.
    let limit = 256 - 256 % chars.len();
    let mut out = Zeroizing::new(String::with_capacity(length));
    let mut buf = Zeroizing::new([0u8; 64]);
    while out.len() < length {
        getrandom::fill(&mut buf[..]).map_err(|e| anyhow!("no randomness from the OS: {e}"))?;
        for &b in buf.iter() {
            if (b as usize) < limit && out.len() < length {
                out.push(chars[b as usize % chars.len()] as char);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let keys = Keys {
            identities: vec![x25519::Identity::generate()],
        };
        let ciphertext = encrypt(&keys.recipients(), b"hunter2\nuser: me\n").unwrap();
        assert_eq!(
            &decrypt(&keys, &ciphertext).unwrap()[..],
            b"hunter2\nuser: me\n"
        );
        let other = Keys {
            identities: vec![x25519::Identity::generate()],
        };
        let err = decrypt(&other, &ciphertext).unwrap_err().to_string();
        assert_eq!(err, "not encrypted to your identity");
    }

    #[test]
    fn recipients_file() {
        let key = x25519::Identity::generate().to_public();
        let text = format!("# me\n\n  {key}  \n");
        assert_eq!(parse_recipients(&text).unwrap().len(), 1);
        assert!(parse_recipients("ssh-rsa AAAA").is_err());
    }

    #[test]
    fn passwords_use_only_the_set() {
        let pw = random_password("ab", 200).unwrap();
        assert_eq!(pw.len(), 200);
        assert!(pw.chars().all(|c| c == 'a' || c == 'b'));
        assert!(pw.contains('a') && pw.contains('b'));
        assert_eq!(random_password("0123456789", 0).unwrap().len(), 0);
    }
}
