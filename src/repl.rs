use crate::commands::{self, Outcome, Session};
use crate::store;
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

const COMMANDS: &[&str] = &[
    "ls", "cd", "pwd", "show", "insert", "import", "export", "generate", "edit", "rm", "mv",
    "cp", "find", "grep", "help", "exit", "quit",
];

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
        let os_path = match cmd {
            "import" => arg_index == 1,
            "export" => arg_index == 2,
            "insert" => prior.contains(&"--file") && !word.starts_with('-'),
            _ => false,
        };
        if os_path {
            return self.filename.complete(line, pos, ctx);
        }

        let dirs_only = cmd == "cd" || (arg_index == 1 && !COMMANDS.contains(&cmd));
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
        match prompt.strip_prefix("ezer:").and_then(|r| r.strip_suffix(" > ")) {
            Some(path) => Cow::Owned(format!(
                "{}{}{} ",
                "ezer:".green().bold(),
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

fn history_path() -> Option<PathBuf> {
    let state_dir = match std::env::var_os("XDG_STATE_HOME") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".local/state"),
    };
    let dir = state_dir.join("ezer-pass");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("history"))
}

pub fn run(sess: &mut Session) -> anyhow::Result<()> {
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .auto_add_history(false)
        .build();
    let mut rl: Editor<EzerHelper, FileHistory> = Editor::with_config(config)?;
    rl.set_helper(Some(EzerHelper {
        store: store::Store::open()?,
        cwd: String::new(),
        filename: FilenameCompleter::new(),
    }));
    let history = history_path();
    if let Some(path) = &history {
        let _ = rl.load_history(path); // fine if it doesn't exist yet
    }

    println!(
        "{} — exploring {} (`help` for commands)",
        "ezer-pass".green().bold(),
        sess.store.root().display()
    );

    loop {
        if let Some(helper) = rl.helper_mut() {
            helper.cwd = sess.cwd.clone();
        }
        let prompt = format!("ezer:{} > ", store::display(&sess.cwd));
        match rl.readline(&prompt) {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line);
                let result = commands::tokenize(line).and_then(|argv| {
                    if argv.is_empty() {
                        Ok(Outcome::Continue)
                    } else {
                        commands::run(sess, &argv)
                    }
                });
                match result {
                    Ok(Outcome::Continue) => {}
                    Ok(Outcome::Exit) => break,
                    Err(e) => eprintln!("{} {e:#}", "error:".red().bold()),
                }
            }
            Err(ReadlineError::Interrupted) => continue, // Ctrl-C clears the line
            Err(ReadlineError::Eof) => break,            // Ctrl-D quits
            Err(e) => return Err(e.into()),
        }
    }

    if let Some(path) = &history {
        let _ = rl.save_history(path);
    }
    Ok(())
}
