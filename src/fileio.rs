use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// First line of an entry that stores a base64-encoded file:
/// `!ezer-pass:b64:<original-basename>`
pub const MARKER_PREFIX: &str = "!ezer-pass:b64:";

/// Files above this size are refused unless the user passes --force; GPG and
/// git cope with more, but a password store is a poor home for big blobs.
pub const SIZE_LIMIT: u64 = 1024 * 1024;

pub struct FileEntry {
    pub filename: String,
    pub data: Vec<u8>,
}

/// The original filename, if `content` is a file entry.
pub fn marker_filename(content: &str) -> Option<&str> {
    content.lines().next()?.strip_prefix(MARKER_PREFIX)
}

/// Encode a file entry as a string suitable for storage in the password store.
pub fn encode_payload(filename: &str, data: &[u8]) -> String {
    let b64 = STANDARD.encode(data);
    let mut out = String::with_capacity(
        MARKER_PREFIX.len() + filename.len() + b64.len() + b64.len() / 76 + 2,
    );
    out.push_str(MARKER_PREFIX);
    out.push_str(filename);
    out.push('\n');
    for chunk in b64.as_bytes().chunks(76) {
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ascii"));
        out.push('\n');
    }
    out
}

/// Decode a file entry from its string representation.
pub fn decode_payload(content: &str) -> Result<FileEntry> {
    let mut lines = content.lines();
    let filename = lines
        .next()
        .and_then(|l| l.strip_prefix(MARKER_PREFIX))
        .context("not a file entry (missing marker header)")?
        .to_string();
    let b64: String = lines.map(str::trim).collect();
    let data = STANDARD
        .decode(b64.as_bytes())
        .context("file entry has corrupt base64 content")?;
    if filename.is_empty() {
        bail!("file entry has an empty filename in its marker");
    }
    Ok(FileEntry { filename, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let payload = encode_payload("photo.jpg", &data);
        assert!(payload.starts_with("!ezer-pass:b64:photo.jpg\n"));
        assert!(payload.lines().skip(1).all(|l| l.len() <= 76));
        let entry = decode_payload(&payload).unwrap();
        assert_eq!(entry.filename, "photo.jpg");
        assert_eq!(entry.data, data);
    }

    #[test]
    fn empty_file_roundtrip() {
        let payload = encode_payload("empty.txt", &[]);
        let entry = decode_payload(&payload).unwrap();
        assert_eq!(entry.filename, "empty.txt");
        assert!(entry.data.is_empty());
    }

    #[test]
    fn marker_detection() {
        assert_eq!(marker_filename("!ezer-pass:b64:a.png\nAAAA"), Some("a.png"));
        assert_eq!(marker_filename("hunter2\nurl: example.com"), None);
        assert_eq!(marker_filename(""), None);
    }

    #[test]
    fn plain_entry_rejected() {
        assert!(decode_payload("hunter2\n").is_err());
    }
}
