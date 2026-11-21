//! One-time codes (TOTP, RFC 6238). An entry holds its secret as an
//! `otpauth://totp/...` line, the format pass-otp and authenticator apps'
//! QR codes use, anywhere in the entry.

use anyhow::{Result, bail};
use hmac::{Hmac, Mac};
use zeroize::Zeroizing;

/// The entry's `otpauth://` line, if it has one.
pub fn find(content: &str) -> Option<&str> {
    content
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("otpauth://"))
}

/// The current code of the `otpauth://` line in `content`, and how many
/// seconds it has left.
pub fn current(content: &str) -> Result<(Zeroizing<String>, u64)> {
    let Some(uri) = find(content) else {
        bail!("it has no otpauth:// line (add one with `otp add`)");
    };
    let totp = Totp::parse(uri)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    Ok((totp.code(now), totp.period - now % totp.period))
}

/// What was typed into `otp add`, as an `otpauth://` line: a URI as is
/// (once it parses), or a bare base32 secret wrapped in one, labelled
/// `name`.
pub fn to_uri(input: &str, name: &str) -> Result<Zeroizing<String>> {
    let input = input.trim();
    let uri = if input.starts_with("otpauth://") {
        Zeroizing::new(input.to_string())
    } else {
        let secret: String = input
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| c.to_ascii_uppercase())
            .collect();
        let label: String = name
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                b => format!("%{b:02X}"),
            })
            .collect();
        Zeroizing::new(format!("otpauth://totp/{label}?secret={secret}"))
    };
    Totp::parse(&uri)?;
    Ok(uri)
}

#[derive(Clone, Copy)]
enum Algorithm {
    Sha1,
    Sha256,
    Sha512,
}

struct Totp {
    secret: Zeroizing<Vec<u8>>,
    digits: u32,
    period: u64,
    algorithm: Algorithm,
}

impl Totp {
    fn parse(uri: &str) -> Result<Self> {
        let Some(rest) = uri.strip_prefix("otpauth://") else {
            bail!("not an otpauth:// URI");
        };
        if !rest.to_ascii_lowercase().starts_with("totp/") {
            bail!("only time-based (totp) codes are supported");
        }
        let query = rest.split_once('?').map_or("", |(_, q)| q);
        let mut totp = Totp {
            secret: Zeroizing::new(Vec::new()),
            digits: 6,
            period: 30,
            algorithm: Algorithm::Sha1,
        };
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            match key.to_ascii_lowercase().as_str() {
                "secret" => totp.secret = base32(value)?,
                "digits" => match value.parse() {
                    Ok(d @ 6..=8) => totp.digits = d,
                    _ => bail!("digits must be 6, 7 or 8"),
                },
                "period" => match value.parse() {
                    Ok(p @ 1..) => totp.period = p,
                    _ => bail!("period must be a number of seconds"),
                },
                "algorithm" => {
                    totp.algorithm = match value.to_ascii_uppercase().as_str() {
                        "SHA1" => Algorithm::Sha1,
                        "SHA256" => Algorithm::Sha256,
                        "SHA512" => Algorithm::Sha512,
                        _ => bail!("unknown algorithm {value:?}"),
                    }
                }
                _ => {}
            }
        }
        if totp.secret.is_empty() {
            bail!("the otpauth:// URI has no secret");
        }
        Ok(totp)
    }

    /// The code at `unix` seconds.
    fn code(&self, unix: u64) -> Zeroizing<String> {
        let counter = (unix / self.period).to_be_bytes();
        macro_rules! hmac {
            ($digest:ty) => {{
                let mut mac =
                    Hmac::<$digest>::new_from_slice(&self.secret).expect("any key length");
                mac.update(&counter);
                Zeroizing::new(mac.finalize().into_bytes().to_vec())
            }};
        }
        let hash = match self.algorithm {
            Algorithm::Sha1 => hmac!(sha1::Sha1),
            Algorithm::Sha256 => hmac!(sha2::Sha256),
            Algorithm::Sha512 => hmac!(sha2::Sha512),
        };
        // RFC 4226's dynamic truncation.
        let at = (hash[hash.len() - 1] & 0x0f) as usize;
        let bits = u32::from_be_bytes(hash[at..at + 4].try_into().expect("4 bytes")) & 0x7fff_ffff;
        let code = bits % 10u32.pow(self.digits);
        Zeroizing::new(format!("{code:0width$}", width = self.digits as usize))
    }
}

/// RFC 4648 base32, ignoring case, spaces and padding.
fn base32(text: &str) -> Result<Zeroizing<Vec<u8>>> {
    let mut out = Zeroizing::new(Vec::new());
    let (mut buffer, mut bits) = (0u64, 0);
    for c in text.bytes().filter(|c| !matches!(c, b' ' | b'=' | b'-')) {
        let value = match c.to_ascii_uppercase() {
            c @ b'A'..=b'Z' => c - b'A',
            c @ b'2'..=b'7' => c - b'2' + 26,
            _ => bail!("the secret isn't base32"),
        };
        buffer = buffer << 5 | value as u64;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238's test vectors (base32 of its ASCII seeds).
    #[test]
    fn rfc6238() {
        let cases = [
            ("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", "SHA1"),
            (
                "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZA",
                "SHA256",
            ),
            (
                "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNA",
                "SHA512",
            ),
        ];
        let expected = [
            (59, ["94287082", "46119246", "90693936"]),
            (1111111109, ["07081804", "68084774", "25091201"]),
            (20000000000, ["65353130", "77737706", "47863826"]),
        ];
        for (i, (secret, algorithm)) in cases.iter().enumerate() {
            let uri = format!("otpauth://totp/x?secret={secret}&digits=8&algorithm={algorithm}");
            let totp = Totp::parse(&uri).unwrap();
            for (time, codes) in expected {
                assert_eq!(*totp.code(time), codes[i], "{algorithm} at {time}");
            }
        }
    }

    #[test]
    fn found_in_an_entry() {
        let entry = "hunter2\nlogin: me\notpauth://totp/x?secret=GEZDGNBV\n";
        assert_eq!(find(entry), Some("otpauth://totp/x?secret=GEZDGNBV"));
        assert!(current(entry).unwrap().0.len() == 6);
        assert!(current("hunter2\n").is_err());
    }

    #[test]
    fn bare_secrets_become_uris() {
        assert_eq!(
            *to_uri("gezd gnbv", "my site").unwrap(),
            "otpauth://totp/my%20site?secret=GEZDGNBV"
        );
        assert!(to_uri("not base32!", "x").is_err());
        assert!(to_uri("otpauth://hotp/x?secret=GEZDGNBV", "x").is_err());
    }
}
