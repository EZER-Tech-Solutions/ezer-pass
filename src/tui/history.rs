//! The `u` history view: the store's git log, what each commit changed,
//! and undoing a commit or rolling the store back to one. Every pass
//! operation is a commit, so this reaches changes from any session or
//! machine, not just this one.

use crate::pass::{self, Commit, FileChange};
use anyhow::Result;
use ratatui::widgets::ListState;
use std::collections::HashSet;
use std::path::Path;

/// How far back the view reaches.
const LIMIT: usize = 500;

/// Commits ezer-pass makes for its file index, hidden from the view: the
/// commit they follow tells the story.
const INDEX_SUBJECT: &str = "Update ezer-pass file index.";

pub struct History {
    pub commits: Vec<Commit>,
    pub unpushed: HashSet<String>,
    pub list: ListState,
    /// What the highlighted commit changed.
    pub changes: Vec<FileChange>,
    /// Asking whether to roll back to the highlighted commit.
    pub confirm: bool,
    /// A store path the last undo or rollback brought back, highlighted in
    /// the tree on leaving.
    pub focus: Option<String>,
}

impl History {
    pub fn load(root: &Path) -> Result<Self> {
        let mut history = History {
            commits: Vec::new(),
            unpushed: HashSet::new(),
            list: ListState::default(),
            changes: Vec::new(),
            confirm: false,
            focus: None,
        };
        history.reload(root)?;
        Ok(history)
    }

    /// Re-read the log, highlighting the newest commit.
    pub fn reload(&mut self, root: &Path) -> Result<()> {
        self.commits.clear();
        for commit in pass::git_log(root, LIMIT)? {
            // Dropping a row leaves the graph's lanes lined up above and
            // below it; only its connector lines need keeping.
            if commit.subject == INDEX_SUBJECT {
                if let Some(prev) = self.commits.last_mut() {
                    prev.after.extend(commit.after);
                }
                continue;
            }
            self.commits.push(commit);
        }
        self.unpushed = pass::git_unpushed(root);
        self.select(0, root);
        Ok(())
    }

    pub fn selected(&self) -> Option<&Commit> {
        self.list.selected().and_then(|i| self.commits.get(i))
    }

    pub fn move_by(&mut self, delta: isize, root: &Path) {
        let i = self.list.selected().unwrap_or(0);
        self.select(i.saturating_add_signed(delta), root);
    }

    pub fn select(&mut self, index: usize, root: &Path) {
        if self.commits.is_empty() {
            self.list.select(None);
            self.changes.clear();
            return;
        }
        let index = index.min(self.commits.len() - 1);
        self.list.select(Some(index));
        self.changes = pass::git_changes(root, &self.commits[index]).unwrap_or_default();
    }

    /// The store paths (".gpg" dropped) the highlighted commit touched.
    pub fn touched(&self) -> Vec<String> {
        self.changes
            .iter()
            .flat_map(|c| std::iter::once(&c.path).chain(c.to.as_ref()))
            .filter_map(|p| store_path(p))
            .collect()
    }
}

/// The store path of a file in the repo, or `None` for pass's own files
/// (`.gpg-id`, `.gitattributes`, the file index).
pub fn store_path(file: &str) -> Option<String> {
    file.strip_suffix(".gpg").map(str::to_string)
}

/// The message for undoing a commit: undoing an undo is a redo, and back.
pub fn undo_message(subject: &str) -> String {
    if let Some(rest) = subject.strip_prefix("Undo: ") {
        format!("Redo: {rest}")
    } else if let Some(rest) = subject.strip_prefix("Redo: ") {
        format!("Undo: {rest}")
    } else {
        format!("Undo: {subject}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_messages_alternate() {
        let undo = undo_message("Remove a from store.");
        assert_eq!(undo, "Undo: Remove a from store.");
        let redo = undo_message(&undo);
        assert_eq!(redo, "Redo: Remove a from store.");
        assert_eq!(undo_message(&redo), undo);
    }

    #[test]
    fn store_paths() {
        assert_eq!(store_path("a/b.gpg").as_deref(), Some("a/b"));
        assert_eq!(store_path(".gpg-id"), None);
        assert_eq!(store_path(".files"), None);
    }
}
