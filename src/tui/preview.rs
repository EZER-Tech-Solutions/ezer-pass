//! The pane beside the tree on wide terminals: the highlighted row's
//! details, loaded when the highlight moves (or the row changes on disk).

use super::history::INDEX_SUBJECT;
use crate::commands::Session;
use crate::{fileio, git, store};
use std::time::SystemTime;
use zeroize::Zeroizing;

/// How many commits the history section reaches back.
const HISTORY: usize = 20;

/// What a preview was loaded for; a different key means loading again.
#[derive(PartialEq)]
struct Key {
    context: String,
    path: String,
    modified: Option<SystemTime>,
    unlocked: bool,
}

pub enum Body {
    /// Who the folder's entries are encrypted to, and the folder whose
    /// `.age-recipients` says so.
    Folder(Result<(String, Vec<Recipient>), String>),
    /// A text entry's lines after the password, which is never kept.
    /// `None` while the identity is locked.
    Entry(Option<Result<Zeroizing<String>, String>>),
    /// File entries aren't decrypted: they can be large, and the content
    /// isn't readable anyway.
    File,
}

pub struct Recipient {
    pub key: String,
    /// The unlocked identity is this recipient.
    pub you: bool,
}

pub struct Preview {
    key: Key,
    pub body: Body,
    /// Commits that touched the row, as (commit time, subject).
    pub history: Vec<(u64, String)>,
}

impl Preview {
    /// The preview for `path`, reusing `old` if nothing it shows changed.
    pub fn load(old: Option<Preview>, sess: &Session, path: &str, is_dir: bool) -> Preview {
        let key = Key {
            context: sess.context.name.clone(),
            path: path.to_string(),
            modified: if is_dir {
                None
            } else {
                sess.store.modified(path)
            },
            unlocked: sess.keys.is_some(),
        };
        if let Some(old) = old
            && old.key == key
        {
            return old;
        }
        let root = sess.store.root();
        let (body, pathspec) = if is_dir {
            (
                Body::Folder(recipients(sess, path)),
                if path.is_empty() {
                    ".".to_string()
                } else {
                    path.to_string()
                },
            )
        } else {
            (entry(sess, path), format!("{path}{}", store::EXT))
        };
        let history = git::path_log(root, &pathspec, !is_dir, HISTORY)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, subject)| subject != INDEX_SUBJECT)
            .collect();
        Preview { key, body, history }
    }

    pub fn path(&self) -> &str {
        &self.key.path
    }
}

fn recipients(sess: &Session, dir: &str) -> Result<(String, Vec<Recipient>), String> {
    let mine: Vec<String> = sess
        .keys
        .as_ref()
        .map(|k| k.recipients().iter().map(|r| r.to_string()).collect())
        .unwrap_or_default();
    let (from, list) = sess.store.recipients(dir).map_err(|e| format!("{e:#}"))?;
    let list = list
        .iter()
        .map(|r| {
            let key = r.to_string();
            Recipient {
                you: mine.contains(&key),
                key,
            }
        })
        .collect();
    Ok((from, list))
}

fn entry(sess: &Session, path: &str) -> Body {
    if sess.files.is_file(path) {
        return Body::File;
    }
    let Some(keys) = &sess.keys else {
        return Body::Entry(None);
    };
    match sess.store.read_entry(path, keys) {
        // Not in the index yet, but a file entry all the same.
        Ok(content) if fileio::marker_filename(&content).is_some() => Body::File,
        Ok(content) => {
            let rest = content.split_once('\n').map_or("", |(_, rest)| rest);
            Body::Entry(Some(Ok(Zeroizing::new(rest.to_string()))))
        }
        Err(e) => Body::Entry(Some(Err(format!("{e:#}")))),
    }
}
