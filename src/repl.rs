use crate::commands::{self, COMMANDS, Outcome, Session};
use crate::context::Context as StoreContext;
use crate::{git, store};
use colored::Colorize;
use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::FileHistory;
use rustyline::validate::Validator;
use rustyline::{CompletionType, Config, Context, Editor, Helper};
use std::borrow::Cow;
use std::path::PathBuf;

/// What `context` takes first.
pub const CONTEXT_SUBCOMMANDS: &[&str] = &["list", "use", "add", "rm", "rename", "move"];

struct EzerHelper {
    store: store::Store,
    /// Mirror of the session cwd, refreshed before every readline call.
    cwd: String,
    filename: FilenameCompleter,
}

impl EzerHelper {
    /// Complete a store path `word` typed relative to the current directory.
    /// Directories complete with a trailing `/` so completion can continue.
    fn complete_store_path(&self, word: &str, dirs_only: bool) -> Vec<Pair> {
        let (dir_part, prefix) = match word.rsplit_once('/') {
            Some((d, p)) => (format!("{d}/"), p),
            None => (String::new(), word),
        };
        let base = store::resolve(&self.cwd, &dir_part);
        let Ok(nodes) = self.store.list(&base) else {
            return Vec::new();
        };
        nodes
            .into_iter()
            .filter(|n| n.name.starts_with(prefix) && (n.is_dir || !dirs_only))
            .map(|n| {
                let suffix = if n.is_dir { "/" } else { "" };
                Pair {
                    display: format!("{}{suffix}", n.name),
                    replacement: format!("{dir_part}{}{suffix}", n.name),
                }
            })
            .collect()
    }
}

impl Completer for EzerHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let before = &line[..pos];
        let word_start = before
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
        let word = &before[word_start..];
        let prior: Vec<&str> = before[..word_start].split_whitespace().collect();

        // First word: command names.
        let Some(&cmd) = prior.first() else {
            let matches = COMMANDS
                .iter()
                .filter(|c| c.starts_with(word))
                .map(|c| Pair {
                    display: c.to_string(),
                    replacement: format!("{c} "),
                })
                .collect();
            return Ok((word_start, matches));
        };

        // Arguments that name OS files rather than store paths.
        let arg_index = prior.len(); // 1-based position of the word being completed

        if cmd == "context" {
            let words: Vec<String> = match (arg_index, prior.get(1).copied()) {
                (1, _) => CONTEXT_SUBCOMMANDS.iter().map(|s| s.to_string()).collect(),
                (2, Some("use" | "rm" | "rename" | "move")) => crate::context::load()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|c| c.name)
                    .collect(),
                (2, Some("add")) => Vec::new(),
                (3, Some("add" | "move")) => return self.filename.complete(line, pos, ctx),
                _ => Vec::new(),
            };
            let matches = words
                .into_iter()
                .filter(|w| w.starts_with(word))
                .map(|w| Pair {
                    replacement: format!("{w} "),
                    display: w,
                })
                .collect();
            return Ok((word_start, matches));
        }

        let os_path = match cmd {
            "import" => arg_index == 1,
            "export" => arg_index == 2,
            "insert" => prior.contains(&"--file") && !word.starts_with('-'),
            _ => false,
        };
        if os_path {
            return self.filename.complete(line, pos, ctx);
        }

        let dirs_only = cmd == "cd"
            || cmd == "mkdir"
            || cmd == "reindex"
            || (arg_index == 1 && !COMMANDS.contains(&cmd));
        Ok((word_start, self.complete_store_path(word, dirs_only)))
    }
}

impl Highlighter for EzerHelper {
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        default: bool,
    ) -> Cow<'b, str> {
        if !default {
            return Cow::Borrowed(prompt);
        }
        // "ezer:/path > " → color the name and path.
        // "ezer[work]:/path > " → the context in its own color.
        match prompt
            .strip_prefix("ezer")
            .and_then(|r| r.strip_suffix(" > "))
            .and_then(|r| r.split_once(':'))
        {
            Some((tag, path)) => Cow::Owned(format!(
                "{}{}{}{}{} ",
                "ezer".green().bold(),
                tag.yellow().bold(),
                ":".green().bold(),
                path.blue().bold(),
                " >".dimmed()
            )),
            None => Cow::Borrowed(prompt),
        }
    }
}

impl Hinter for EzerHelper {
    type Hint = String;
}

impl Validator for EzerHelper {}
impl Helper for EzerHelper {}

/// Path of a file in ezer-pass's state directory (`$XDG_STATE_HOME/ezer-pass`),
/// creating the directory if necessary. State files hold commands and store
/// paths only, never secrets.
pub fn state_file(name: &str) -> Option<PathBuf> {
    let state_dir = match std::env::var_os("XDG_STATE_HOME") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".local/state"),
    };
    let dir = state_dir.join("ezer-pass");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(name))
}

/// Run the interactive REPL (Read-Eval-Print Loop) for the password manager.
pub fn run(sess: &mut Session) -> anyhow::Result<()> {
    // Build rustyline configuration with:
    // - List completion type (tab-completion shows a list of matches)
    // - No automatic history addition (we add history manually after successful commands)
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .auto_add_history(false)
        .build();
    let mut rl: Editor<EzerHelper, FileHistory> = Editor::with_config(config)?;
    rl.set_helper(Some(EzerHelper {
        store: store::Store::open(sess.store.root().to_path_buf())?,
        cwd: String::new(),
        filename: FilenameCompleter::new(),
    }));

    // load or create the history file, if possible.
    // If the history file cannot be created, continue without history.
    let history = state_file("history");
    if let Some(path) = &history {
        // Load the command history from the history file.
        let _ = rl.load_history(path);
    }

    println!(
        "{}{} — exploring {} (`help` for commands)",
        "ezer-pass".green().bold(),
        context_tag(sess),
        sess.store.root().display()
    );
    offer_pull(&mut rl, sess);

    // Main interactive loop: read a line, tokenize it, and run the command.
    loop {
        // Set the helper's cwd to the session's cwd before each readline call, so completions are relative to the current directory.
        if let Some(helper) = rl.helper_mut() {
            helper.cwd = sess.cwd.clone();
        }

        // Output the prompt with the current working directory, and read a line of input from the user.
        let prompt = format!("ezer{}:{} > ", context_tag(sess), store::display(&sess.cwd));
        match rl.readline(&prompt) {
            Ok(line) => {
                // Trim whitespace from the line and skip empty lines.
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                // Add input to history.
                let _ = rl.add_history_entry(line);

                // Tokenize the input line into arguments and run the command.
                let result = commands::tokenize(line).and_then(|argv| {
                    if argv.is_empty() {
                        Ok(Outcome::Continue)
                    } else {
                        commands::run(sess, &argv)
                    }
                });

                // If the command returns an error, print it to stderr. If the command returns Outcome::Exit, break the loop and exit the REPL.
                match result {
                    Ok(Outcome::Continue) => {}
                    Ok(Outcome::Exit) => break,
                    Ok(Outcome::Switch(target)) => switch(&mut rl, sess, target),
                    Err(e) => eprintln!("{} {e:#}", "error:".red().bold()),
                }
            }

            // Handle Ctrl-C and Ctrl-D gracefully:
            Err(ReadlineError::Interrupted) => continue, // Ctrl-C clears the line
            Err(ReadlineError::Eof) => break,            // Ctrl-D quits
            Err(e) => return Err(e.into()),
        }
    }

    // On exit, offer to push any local commits to the store's upstream.
    offer_push(&mut rl, sess);

    // Save the command history to the history file.
    if let Some(path) = &history {
        let _ = rl.save_history(path);
    }
    Ok(())
}

/// Ask a [Y/n] question: Enter or y/yes means yes; anything else, Ctrl-C, or
/// Ctrl-D means no.
fn confirm(rl: &mut Editor<EzerHelper, FileHistory>, prompt: &str) -> bool {
    matches!(
        rl.readline(prompt),
        Ok(answer) if matches!(answer.trim().to_lowercase().as_str(), "" | "y" | "yes")
    )
}

/// `[work]` outside the default context, so the prompt says which store
/// you are in.
fn context_tag(sess: &Session) -> String {
    if sess.context.name == crate::context::DEFAULT {
        String::new()
    } else {
        format!("[{}]", sess.context.name)
    }
}

/// `context use`: offer to push what this store hasn't, then carry on in
/// `target`, offering to pull it.
fn switch(rl: &mut Editor<EzerHelper, FileHistory>, sess: &mut Session, target: StoreContext) {
    if target.name == sess.context.name {
        println!("already in {}", target.name);
        return;
    }
    if !target.path.is_dir() {
        eprintln!(
            "{} {}'s store isn't at {}",
            "error:".red().bold(),
            target.name,
            crate::context::tilde(&target.path)
        );
        return;
    }
    offer_push(rl, sess);
    if let Err(e) = sess.switch(target) {
        eprintln!("{} {e:#}", "error:".red().bold());
        return;
    }
    if let Some(helper) = rl.helper_mut()
        && let Ok(store) = store::Store::open(sess.store.root().to_path_buf())
    {
        helper.store = store;
    }
    println!(
        "switched to {} — {}",
        sess.context.name.green().bold(),
        sess.store.root().display()
    );
    offer_pull(rl, sess);
}

/// If the store has an upstream, offer to pull any changes from it.
fn offer_pull(rl: &mut Editor<EzerHelper, FileHistory>, sess: &Session) {
    let Some(upstream) = git::upstream(sess.store.root()) else {
        return;
    };
    if confirm(rl, &format!("pull from {upstream}? [Y/n] ")) {
        // --ff-only so a diverged store fails cleanly instead of dropping
        // into an editor for a merge commit; sync failure must not block
        // the session.
        if git::interactive(sess.store.root(), &["pull", "--ff-only"]).is_err() {
            eprintln!(
                "{}",
                "warning: git pull failed — continuing unsynced (resolve with `git ...`)".yellow()
            );
        }
    } else {
        println!("{}", "not pulled (pull later with `git pull`)".dimmed());
    }
}

/// Offer to push any local commits to the store's upstream (on exit, and
/// before switching contexts).
fn offer_push(rl: &mut Editor<EzerHelper, FileHistory>, sess: &Session) {
    let Some(upstream) = git::upstream(sess.store.root()) else {
        return;
    };
    let ahead = git::ahead_count(sess.store.root());
    if ahead == 0 {
        return;
    }
    let prompt = format!(
        "push {ahead} commit{} to {upstream}? [Y/n] ",
        if ahead == 1 { "" } else { "s" }
    );
    if confirm(rl, &prompt) {
        if git::interactive(sess.store.root(), &["push"]).is_err() {
            eprintln!(
                "{}",
                "warning: git push failed — your commits are still local".yellow()
            );
        }
    } else {
        println!("{}", "not pushed (push later with `git push`)".dimmed());
    }
}
