use crate::context;
use crate::crypto::{self, Keys};
use crate::password::{Kind, Spec};
use crate::store::{self, EXT};
use crate::{clipboard, fileio, git, index, setup};
use anyhow::{Context, Result, bail};
use colored::Colorize;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// Every command `run` understands, for completion and the TUI's command line.
pub const COMMANDS: &[&str] = &[
    "ls", "cd", "pwd", "mkdir", "show", "insert", "import", "export", "reindex", "generate",
    "edit", "rm", "mv", "cp", "find", "grep", "git", "init", "migrate", "context", "help", "exit",
    "quit",
];

pub struct Session {
    /// Which named store this is (work, personal, ...).
    pub context: context::Context,
    pub store: store::Store,
    /// Current store directory ("" = root).
    pub cwd: String,
    pub interactive: bool,
    /// Which entries are file entries; see `index`.
    pub files: index::FileIndex,
    /// The identity, once unlocked; kept for the rest of the session.
    pub keys: Option<Keys>,
}

pub enum Outcome {
    Continue,
    Exit,
    /// `context use` in an interactive session: work in this context now.
    Switch(context::Context),
}

impl Session {
    pub fn new(context: context::Context, store: store::Store, interactive: bool) -> Self {
        // initialize the session with the store and set the current working directory to root
        let files = index::FileIndex::load(store.root());
        Session {
            context,
            store,
            cwd: String::new(),
            interactive,
            files,
            keys: None,
        }
    }

    /// Work in `target` instead, keeping the identity unlocked if the two
    /// share it, and remember it as the context to open next time.
    pub fn switch(&mut self, target: context::Context) -> Result<()> {
        let store = store::Store::open(target.path.clone())?;
        let mut next = Session::new(target, store, self.interactive);
        if next.context.identity().ok() == self.context.identity().ok() {
            next.keys = self.keys.take();
        }
        *self = next;
        context::remember(&self.context.name);
        Ok(())
    }

    /// Unlock the identity if it isn't yet, asking for its passphrase on
    /// the terminal.
    pub fn unlock(&mut self) -> Result<&Keys> {
        if self.keys.is_none() {
            self.keys = Some(setup::unlock_on_terminal(&self.context.identity()?)?);
        }
        Ok(self.keys.as_ref().expect("just unlocked"))
    }

    /// Decrypt an entry, unlocking first if needed.
    pub fn read(&mut self, path: &str) -> Result<Zeroizing<String>> {
        self.unlock()?;
        let keys = self.keys.as_ref().expect("unlocked");
        self.store.read_entry(path, keys)
    }

    /// Encrypt and write an entry, then commit it.
    pub fn save(&mut self, path: &str, content: &[u8], message: &str) -> Result<()> {
        self.store.write_entry(path, content)?;
        self.commit(&[&entry_file(path)], message)
    }

    /// Commit a change to `paths` (store-relative), if the store is in git.
    pub fn commit(&self, paths: &[&str], message: &str) -> Result<()> {
        git::commit(self.store.root(), paths, message)
            .context("the change is saved, but committing it to git failed")
    }

    pub fn resolve(&self, input: &str) -> String {
        store::resolve(&self.cwd, input)
    }

    fn resolve_entry(&self, input: &str) -> Result<String> {
        let path = self.resolve(input);
        if self.store.is_entry(&path) {
            Ok(path)
        } else if self.store.is_dir(&path) {
            bail!(
                "{} is a directory (try `ls {input}`)",
                store::display(&path)
            );
        } else {
            bail!("{} is not in the password store", store::display(&path));
        }
    }
}

/// Split a REPL line into arguments, honoring single/double quotes and
/// backslash escapes outside single quotes.
pub fn tokenize(line: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') if c == '\'' => quote = None,
            Some('"') if c == '"' => quote = None,
            Some('"') if c == '\\' => {
                let next = chars.next().context("trailing backslash")?;
                if !matches!(next, '"' | '\\') {
                    cur.push('\\');
                }
                cur.push(next);
            }
            Some(_) => cur.push(c),
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_word = true;
                }
                '\\' => {
                    cur.push(chars.next().context("trailing backslash")?);
                    in_word = true;
                }
                c if c.is_whitespace() => {
                    if in_word {
                        args.push(std::mem::take(&mut cur));
                        in_word = false;
                    }
                }
                c => {
                    cur.push(c);
                    in_word = true;
                }
            },
        }
    }
    if quote.is_some() {
        bail!("unterminated quote");
    }
    if in_word {
        args.push(cur);
    }
    Ok(args)
}

/// Split argv (after the command name) into flags and positionals.
fn split_flags(args: &[String]) -> (Vec<&str>, Vec<&str>) {
    let mut flags = Vec::new();
    let mut positional = Vec::new();
    for a in args {
        if a.starts_with('-') && a.len() > 1 {
            flags.push(a.as_str());
        } else {
            positional.push(a.as_str());
        }
    }
    (flags, positional)
}

fn expect_args(positional: &[&str], usage: &str, min: usize, max: usize) -> Result<()> {
    if positional.len() < min || positional.len() > max {
        bail!("usage: {usage}");
    }
    Ok(())
}

/// An entry's file, relative to the store root.
fn entry_file(path: &str) -> String {
    format!("{path}{EXT}")
}

/// Ask a yes/no question on the terminal; anything but y/yes is no.
fn confirm(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}

/// Whether running `argv` will decrypt something, so the TUI can unlock
/// the identity before handing the terminal over.
pub fn needs_keys(argv: &[String]) -> bool {
    match argv.first().map(String::as_str) {
        Some("show" | "edit" | "export" | "grep" | "reindex") => true,
        Some("generate") => argv.iter().any(|a| a == "-i" || a == "--in-place"),
        _ => false,
    }
}

/// Run a command in the given session, returning an Outcome indicating whether to continue or exit.
pub fn run(sess: &mut Session, argv: &[String]) -> Result<Outcome> {
    let result = dispatch(sess, argv);
    // Persist whatever the command taught the file index, even on failure
    // (a batch may have half-succeeded).
    sess.files.prune(|p| sess.store.is_entry(p));
    sess.files.save(sess.store.root());
    result
}

fn dispatch(sess: &mut Session, argv: &[String]) -> Result<Outcome> {
    let cmd = argv[0].as_str();
    let args = &argv[1..];
    let (flags, positional) = split_flags(args);

    match cmd {
        "ls" | "list" => {
            expect_args(&positional, "ls [path]", 0, 1)?;
            cmd_ls(sess, positional.first().copied().unwrap_or(""))?
        }
        "cd" => {
            expect_args(&positional, "cd [path]", 0, 1)?;
            let target = sess.resolve(positional.first().copied().unwrap_or("/"));
            if !sess.store.is_dir(&target) {
                bail!("{} is not a directory", store::display(&target));
            }
            sess.cwd = target;
            if !sess.interactive {
                eprintln!(
                    "{}",
                    "note: cd only has an effect inside the interactive session (run `ezer-pass` with no arguments)"
                        .dimmed()
                );
            }
        }
        "pwd" => println!("{}", store::display(&sess.cwd).blue().bold()),
        "mkdir" => {
            expect_args(&positional, "mkdir <path>", 1, 1)?;
            let path = sess.resolve(positional[0]);
            sess.store.mkdir(&path)?;
            println!("created {}/", store::display(&path).blue().bold());
        }
        "show" => {
            expect_args(&positional, "show [-c] <entry>", 1, 1)?;
            let clip = flags.iter().any(|f| *f == "-c" || *f == "--clip");
            cmd_show(sess, positional[0], clip)?
        }
        "insert" => cmd_insert(sess, &flags, &positional)?,
        "reindex" => {
            expect_args(&positional, "reindex [path]", 0, 1)?;
            cmd_reindex(sess, positional.first().copied())?
        }
        "import" => {
            expect_args(&positional, "import [--force] <file> <entry>", 2, 2)?;
            let force = flags.iter().any(|f| *f == "-f" || *f == "--force");
            cmd_import(sess, positional[0], positional[1], force)?
        }
        "export" => {
            expect_args(&positional, "export [-f] <entry> [dest]", 1, 2)?;
            let force = flags.iter().any(|f| *f == "-f" || *f == "--force");
            cmd_export(sess, positional[0], positional.get(1).copied(), force)?
        }
        "generate" => {
            expect_args(
                &positional,
                "generate [-n|--url-safe|--digits|--hex] [--no-ambiguous] [--exclude=CHARS] [-c] [-f] [-i] <entry> [length]",
                1,
                2,
            )?;
            cmd_generate(sess, &flags, &positional)?
        }
        "edit" => {
            expect_args(&positional, "edit <entry>", 1, 1)?;
            cmd_edit(sess, positional[0])?
        }
        "rm" => {
            expect_args(&positional, "rm [-r] [-f] <path>", 1, 1)?;
            let path = sess.resolve(positional[0]);
            let recursive = flags.iter().any(|f| matches!(*f, "-r" | "--recursive"));
            let force = flags.iter().any(|f| matches!(*f, "-f" | "--force"));
            cmd_rm(sess, &path, recursive, force)?;
            if !sess.store.is_entry(&path) && !sess.store.is_dir(&path) {
                sess.files.remove(&path);
            }
        }
        "mv" | "cp" => {
            expect_args(&positional, "mv|cp [-f] <src> <dest>", 2, 2)?;
            let force = flags.iter().any(|f| matches!(*f, "-f" | "--force"));
            let src = sess.resolve(positional[0]);
            if let Some(landed) = cmd_move(sess, cmd == "cp", &src, positional[1], force)? {
                sess.files.transfer(&src, &landed, cmd == "cp");
            }
        }
        "git" => {
            if args.is_empty() {
                bail!("usage: git <git-args>");
            }
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            if args == ["init"] {
                git::init(sess.store.root())?;
                println!("the store is now a git repo; every change will be a commit");
            } else {
                git::interactive(sess.store.root(), &args)?
            }
        }
        "find" => {
            if positional.is_empty() {
                bail!("usage: find <terms>");
            }
            cmd_find(sess, &positional)
        }
        "grep" => {
            expect_args(&positional, "grep [-i] <text>", 1, 1)?;
            let ignore_case = flags.iter().any(|f| matches!(*f, "-i" | "--ignore-case"));
            cmd_grep(sess, positional[0], ignore_case)?
        }
        "init" => setup::init(sess, args)?,
        "context" => return cmd_context(sess, &positional),
        "migrate" => {
            expect_args(&positional, "migrate", 0, 0)?;
            setup::migrate(sess)?
        }
        "help" | "?" => print_help(),
        "exit" | "quit" if sess.interactive => return Ok(Outcome::Exit),
        // Bare directory name acts like `cd` in the REPL.
        _ if sess.interactive && args.is_empty() && sess.store.is_dir(&sess.resolve(cmd)) => {
            sess.cwd = sess.resolve(cmd);
        }
        _ => bail!("unknown command {cmd:?} (try `help`)"),
    }
    Ok(Outcome::Continue)
}

const CONTEXT_USAGE: &str = "context [list | use <name> | add <name> [path] | rm <name> | rename <old> <new> | move <name> <path>]";

/// `context`: list, switch, add, remove, rename or move contexts.
fn cmd_context(sess: &mut Session, args: &[&str]) -> Result<Outcome> {
    match args {
        [] | ["list" | "ls"] => {
            let contexts = context::load()?;
            let width = contexts.iter().map(|c| c.name.len()).max().unwrap_or(0);
            for c in &contexts {
                let here = c.name == sess.context.name;
                let name = format!("{:<width$}", c.name);
                println!(
                    "{} {}  {}{}{}",
                    if here { "●".green() } else { " ".into() },
                    if here {
                        name.green().bold()
                    } else {
                        name.bold()
                    },
                    context::tilde(&c.path).dimmed(),
                    if c.identity.is_some() {
                        "  own key".cyan().to_string()
                    } else {
                        String::new()
                    },
                    if c.path.is_dir() {
                        String::new()
                    } else {
                        "  (no store there)".red().to_string()
                    },
                );
            }
        }
        ["use", name] => {
            let target = context::find(name)?;
            if sess.interactive {
                return Ok(Outcome::Switch(target));
            }
            if !target.path.is_dir() {
                bail!("{name}'s store isn't at {}", context::tilde(&target.path));
            }
            context::remember(name);
            println!(
                "ezer-pass now opens {} ({})",
                name.green().bold(),
                context::tilde(&target.path)
            );
        }
        ["add", name] | ["add", name, _] => {
            let path = args.get(2).map(|p| context::user_path(p)).transpose()?;
            let target = context::create(name, path)?;
            println!(
                "added context {} at {}",
                name.green().bold(),
                context::tilde(&target.path)
            );
            init_store(sess, &target)?;
            println!(
                "{}",
                format!("`context use {name}` switches to it; `-C {name}` uses it for one command")
                    .dimmed()
            );
        }
        ["rm", name] => {
            if sess.interactive && *name == sess.context.name {
                bail!("you are in {name}; `context use` another one first");
            }
            let target = context::find(name)?;
            context::remove(name)?;
            println!(
                "removed context {name}; its store at {} is left as it is",
                context::tilde(&target.path)
            );
        }
        ["rename", old, new] => {
            context::find(old)?;
            context::rename(old, new)?;
            if sess.context.name == *old {
                sess.context.name = new.to_string();
            }
            println!("renamed context {old} to {}", new.green().bold());
        }
        ["move", name, path] => {
            let target = context::find(name)?;
            let to = context::user_path(path)?;
            context::move_store(&target, &to)?;
            if sess.context.name == *name {
                sess.store = store::Store::open(to.clone())?;
                sess.context.path = to.clone();
            }
            println!(
                "moved {name}'s store to {}",
                context::tilde(&to).green().bold()
            );
            if git::upstream(&to).is_some() {
                println!(
                    "{}",
                    "its git remote is unchanged; other machines keep their own copy where it is"
                        .dimmed()
                );
            }
        }
        _ => bail!("usage: {CONTEXT_USAGE}"),
    }
    Ok(Outcome::Continue)
}

/// Encrypt a new context's store to your key, as `init` would there,
/// unless it already says who it is encrypted to.
fn init_store(sess: &mut Session, target: &context::Context) -> Result<()> {
    let store = store::Store::open(target.path.clone())?;
    if store.uses_gpg() || store.root().join(store::RECIPIENTS).exists() {
        return Ok(());
    }
    let mut other = Session::new(target.clone(), store, sess.interactive);
    let shared = other.context.identity().ok() == sess.context.identity().ok();
    if shared {
        other.keys = sess.keys.take();
    }
    let result = setup::init(&mut other, &[]);
    if shared {
        sess.keys = other.keys.take();
    }
    result
}

/// List the contents of a store directory, or show a single entry.
fn cmd_ls(sess: &Session, arg: &str) -> Result<()> {
    let path = sess.resolve(arg);
    if sess.store.is_dir(&path) {
        let nodes = sess.store.list(&path)?;
        if nodes.is_empty() {
            println!("{}", "(empty)".dimmed());
        }
        for node in nodes {
            if node.is_dir {
                println!("{}/", node.name.blue().bold());
            } else if sess.files.is_file(&store::resolve(&path, &node.name)) {
                println!("{}", node.name.magenta());
            } else {
                println!("{}", node.name);
            }
        }
    } else if sess.store.is_entry(&path) {
        println!("{}", store::display(&path));
    } else {
        bail!("{} is not in the password store", store::display(&path));
    }
    Ok(())
}

/// Show the contents of a store entry, optionally copying it to the clipboard.
fn cmd_show(sess: &mut Session, arg: &str, clip: bool) -> Result<()> {
    let path = sess.resolve_entry(arg)?;
    let content = sess.read(&path)?;
    if clip {
        if fileio::marker_filename(&content).is_some() {
            bail!("{} is a file entry (use `export`)", store::display(&path));
        }
        clipboard::copy(content.lines().next().unwrap_or(""))?;
        println!(
            "copied {} to the clipboard; it clears in {} seconds",
            store::display(&path).green(),
            clipboard::clear_after()
        );
        return Ok(());
    }
    if fileio::marker_filename(&content).is_some() {
        let entry = fileio::decode_payload(&content)?;
        sess.files.insert(&path, entry.data.len() as u64);
        println!(
            "{} {} ({} bytes) — use {} to write it out",
            "file entry:".yellow().bold(),
            entry.filename,
            entry.data.len(),
            format!("`export {arg}`").green()
        );
    } else {
        sess.files.remove(&path);
        print!("{}", *content);
    }
    Ok(())
}

/// The length a generated password has unless told otherwise.
pub fn default_length() -> u32 {
    std::env::var("PASSWORD_STORE_GENERATED_LENGTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25)
}

/// Generate a password into `path` and commit it. `in_place` replaces only the first line of an existing
/// entry, keeping the rest. Returns the password, for the caller to copy
/// or show.
pub fn generate(
    sess: &mut Session,
    path: &str,
    spec: &Spec,
    length: usize,
    in_place: bool,
) -> Result<Zeroizing<String>> {
    let password = spec.generate(length)?;
    let content = if in_place {
        let old = sess.read(path)?;
        let rest = old.split_once('\n').map_or("", |(_, rest)| rest);
        Zeroizing::new(format!("{}\n{rest}", *password))
    } else {
        Zeroizing::new(format!("{}\n", *password))
    };
    let message = if in_place {
        format!("Replace generated password for {path}.")
    } else {
        format!("Add generated password for {path}.")
    };
    sess.save(path, content.as_bytes(), &message)?;
    // Whatever was there, it now starts with a password.
    sess.files.remove(path);
    Ok(password)
}

fn cmd_generate(sess: &mut Session, flags: &[&str], positional: &[&str]) -> Result<()> {
    let has = |names: &[&str]| flags.iter().any(|f| names.contains(f));
    // An unknown flag (an old `--base-64`) must not quietly give symbols.
    if let Some(f) = flags.iter().find(|f| {
        Kind::from_flag(f).is_none()
            && !f.starts_with("--exclude=")
            && !matches!(
                **f,
                "-c" | "--clip" | "-f" | "--force" | "-i" | "--in-place" | "--no-ambiguous"
            )
    }) {
        bail!("unknown option {f}");
    }
    let kinds: Vec<Kind> = flags.iter().filter_map(|f| Kind::from_flag(f)).collect();
    if kinds.len() > 1 {
        bail!("pick one type: -n, --url-safe, --digits or --hex");
    }
    let spec = Spec {
        kind: kinds.first().copied().unwrap_or(Kind::Symbols),
        no_look_alikes: has(&["--no-ambiguous"]),
        exclude: flags
            .iter()
            .filter_map(|f| f.strip_prefix("--exclude="))
            .collect(),
    };
    let path = sess.resolve(positional[0]);
    let length = match positional.get(1) {
        Some(n) => n
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .with_context(|| format!("length must be a positive number, not {n:?}"))?,
        None => default_length() as usize,
    };
    if sess.store.is_dir(&path) {
        bail!("{} is a folder", store::display(&path));
    }
    let exists = sess.store.is_entry(&path);
    let in_place = has(&["-i", "--in-place"]);
    if in_place && !exists {
        bail!("{} doesn't exist (drop -i)", store::display(&path));
    }
    if exists
        && !in_place
        && !has(&["-f", "--force"])
        && !confirm(&format!(
            "{} exists. Replace it entirely?",
            store::display(&path)
        ))?
    {
        return Ok(());
    }
    let password = generate(sess, &path, &spec, length, in_place)?;
    if has(&["-c", "--clip"]) {
        clipboard::copy(&password)?;
        println!(
            "generated {} and copied it to the clipboard; it clears in {} seconds",
            store::display(&path).green(),
            clipboard::clear_after()
        );
    } else {
        println!(
            "the generated password for {} is:\n{}",
            store::display(&path).green(),
            password.yellow().bold()
        );
    }
    Ok(())
}

/// A temporary file to edit a decrypted entry in: in RAM (`/dev/shm`)
/// when there is one, readable only by you, and wiped on drop.
struct EditFile {
    dir: PathBuf,
    file: PathBuf,
}

impl EditFile {
    fn new(name: &str, content: &[u8]) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let shm = Path::new("/dev/shm");
        let base = if shm.is_dir() {
            shm.to_path_buf()
        } else {
            let tmp = std::env::temp_dir();
            eprintln!(
                "{} no /dev/shm here; the decrypted entry sits in {} while you edit",
                "warning:".yellow().bold(),
                tmp.display()
            );
            tmp
        };
        let mut tag = [0u8; 8];
        getrandom::fill(&mut tag).map_err(|e| anyhow::anyhow!("no randomness from the OS: {e}"))?;
        let tag: String = tag.iter().map(|b| format!("{b:02x}")).collect();
        let dir = base.join(format!("ezer-pass.{tag}"));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
        let edit = EditFile {
            file: dir.join(format!("{name}.txt")),
            dir,
        };
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&edit.file)
            .and_then(|mut f| f.write_all(content))
            .with_context(|| format!("cannot write {}", edit.file.display()))?;
        Ok(edit)
    }
}

impl Drop for EditFile {
    fn drop(&mut self) {
        if let Ok(len) = std::fs::metadata(&self.file).map(|m| m.len()) {
            let _ = std::fs::write(&self.file, vec![0u8; len as usize]);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Edit an entry (or start a new one) in `$VISUAL`/`$EDITOR`.
fn cmd_edit(sess: &mut Session, arg: &str) -> Result<()> {
    let path = sess.resolve(arg);
    if path.is_empty() || sess.store.is_dir(&path) {
        bail!("{} is a folder", store::display(&path));
    }
    let exists = sess.store.is_entry(&path);
    let old = if exists {
        sess.read(&path)?
    } else {
        Zeroizing::new(String::new())
    };
    let name = path.rsplit('/').next().unwrap_or("entry");
    let edit = EditFile::new(name, old.as_bytes())?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    // Through the shell, so an editor with arguments ("code -w") works.
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&edit.file)
        .status()
        .with_context(|| format!("failed to run {editor}"))?;
    if !status.success() {
        bail!("{editor} failed; nothing was saved");
    }
    let mut new = Zeroizing::new(Vec::new());
    std::fs::File::open(&edit.file)
        .and_then(|mut f| f.read_to_end(&mut new))
        .context("cannot read the edited entry back")?;
    drop(edit);
    if exists && new[..] == *old.as_bytes() {
        println!("{} unchanged", store::display(&path));
        return Ok(());
    }
    if !exists && new.is_empty() {
        println!("nothing written; {} not created", store::display(&path));
        return Ok(());
    }
    let editor_name = editor.split_whitespace().next().unwrap_or(&editor);
    let message = if exists {
        format!("Edit password for {path} using {editor_name}.")
    } else {
        format!("Add password for {path} using {editor_name}.")
    };
    sess.save(&path, &new, &message)?;
    let text = String::from_utf8_lossy(&new);
    let size = fileio::decode_payload(&text)
        .ok()
        .map(|f| f.data.len() as u64);
    sess.files.learn(&path, size);
    Ok(())
}

/// Remove an entry, or a folder with `recursive`. Asks first unless
/// `force`.
fn cmd_rm(sess: &mut Session, path: &str, recursive: bool, force: bool) -> Result<()> {
    let store = &sess.store;
    if path.is_empty() {
        bail!("refusing to remove the whole store");
    }
    let is_entry = store.is_entry(path);
    if !is_entry && !store.is_dir(path) {
        bail!("{} is not in the password store", store::display(path));
    }
    if !is_entry && !recursive {
        bail!(
            "{} is a folder (rm -r removes it and everything in it)",
            store::display(path)
        );
    }
    let shown = if is_entry {
        store::display(path)
    } else {
        store::display_dir(path)
    };
    if !force && !confirm(&format!("Remove {shown}?"))? {
        return Ok(());
    }
    let changed = if is_entry {
        let file = entry_file(path);
        std::fs::remove_file(store.root().join(&file))
            .with_context(|| format!("cannot remove {shown}"))?;
        file
    } else {
        std::fs::remove_dir_all(store.root().join(path))
            .with_context(|| format!("cannot remove {shown}"))?;
        path.to_string()
    };
    store.remove_empty_dirs(store::parent(path));
    sess.commit(&[&changed], &format!("Remove {path} from store."))?;
    if sess.interactive || !force {
        println!("removed {shown}");
    }
    Ok(())
}

/// The recipients of `dir`, as comparable strings.
fn recipient_set(store: &store::Store, dir: &str) -> Result<Vec<String>> {
    let (_, recipients) = store.recipients(dir)?;
    let mut set: Vec<String> = recipients.iter().map(|r| r.to_string()).collect();
    set.sort();
    set.dedup();
    Ok(set)
}

/// Move (or copy) an entry or folder. `dest` is where it goes, or a folder
/// to put it in (an existing one, or written with a trailing slash).
/// Entries landing under different recipients are re-encrypted for them.
/// Returns where it landed, or `None` if the user declined to overwrite.
fn cmd_move(
    sess: &mut Session,
    copy: bool,
    src: &str,
    dest_arg: &str,
    force: bool,
) -> Result<Option<String>> {
    let dest = sess.resolve(dest_arg);
    let store = &sess.store;
    let landed = if store.is_dir(&dest) || dest_arg.ends_with('/') {
        let name = src.rsplit('/').next().unwrap_or(src);
        store::resolve(&dest, name)
    } else {
        dest
    };
    let is_entry = store.is_entry(src);
    if src.is_empty() || (!is_entry && !store.is_dir(src)) {
        bail!("{} is not in the password store", store::display(src));
    }
    if landed == src {
        bail!("{} is already there", store::display(src));
    }
    if !is_entry && (landed.is_empty() || landed.starts_with(&format!("{src}/"))) {
        bail!("cannot put {} inside itself", store::display_dir(src));
    }
    // Every entry that moves, as (from, to).
    let pairs: Vec<(String, String)> = if is_entry {
        if store.is_dir(&landed) {
            bail!("{} is a folder", store::display(&landed));
        }
        vec![(src.to_string(), landed.clone())]
    } else {
        if store.is_entry(&landed) {
            bail!("{} is an entry", store::display(&landed));
        }
        store
            .walk()
            .into_iter()
            .filter(|(p, is_dir)| !is_dir && p.starts_with(&format!("{src}/")))
            .map(|(p, _)| {
                let to = format!("{landed}{}", &p[src.len()..]);
                (p, to)
            })
            .collect()
    };
    let clashes = pairs.iter().filter(|(_, to)| store.is_entry(to)).count();
    if clashes > 0 && !force {
        let question = if is_entry {
            format!("{} exists. Overwrite it?", store::display(&landed))
        } else {
            format!(
                "{clashes} entr{} in {} would be overwritten. Go ahead?",
                if clashes == 1 { "y" } else { "ies" },
                store::display_dir(&landed)
            )
        };
        if !confirm(&question)? {
            return Ok(None);
        }
    }
    let before: Vec<Vec<String>> = pairs
        .iter()
        .map(|(from, _)| recipient_set(store, store::parent(from)))
        .collect::<Result<_>>()?;
    // A folder moving somewhere new goes in one piece, taking its own
    // `.age-recipients` (and empty subfolders) along.
    let whole = !is_entry && !store.is_dir(&landed) && !copy;
    if whole {
        let to = store.root().join(&landed);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(store.root().join(src), &to)
            .with_context(|| format!("cannot move {}", store::display_dir(src)))?;
    } else if !is_entry && !store.is_dir(&landed) {
        copy_tree(&store.root().join(src), &store.root().join(&landed))?;
    }
    for ((from, to), before) in pairs.iter().zip(before) {
        let after = recipient_set(&sess.store, store::parent(to))?;
        let from_file = if whole {
            sess.store.entry_file(to)
        } else {
            sess.store.entry_file(from)
        };
        if after != before {
            sess.unlock()?;
            let keys = sess.keys.as_ref().expect("unlocked");
            let ciphertext = std::fs::read(&from_file)?;
            let plain = crypto::decrypt(keys, &ciphertext)
                .with_context(|| format!("cannot decrypt {}", store::display(from)))?;
            sess.store.write_entry(to, &plain)?;
        } else if !whole {
            let data = std::fs::read(&from_file)?;
            sess.store.write_file(&sess.store.entry_file(to), &data)?;
        }
        if !copy && !whole {
            std::fs::remove_file(&from_file)?;
        }
    }
    let (src_path, landed_path) = if is_entry {
        (entry_file(src), entry_file(&landed))
    } else {
        (src.to_string(), landed.clone())
    };
    if !copy {
        if !is_entry && !whole {
            std::fs::remove_dir_all(sess.store.root().join(src))?;
        }
        sess.store.remove_empty_dirs(store::parent(src));
    }
    let message = if copy {
        format!("Copy {src} to {landed}.")
    } else {
        format!("Rename {src} to {landed}.")
    };
    sess.commit(&[&src_path, &landed_path], &message)?;
    Ok(Some(landed))
}

/// Copy a folder and everything in it.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// List entries and folders whose path contains any of `terms`, ignoring
/// case.
fn cmd_find(sess: &Session, terms: &[&str]) {
    let terms: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
    let mut found = 0;
    for (path, is_dir) in sess.store.walk() {
        let lower = path.to_lowercase();
        if !terms.iter().any(|t| lower.contains(t.as_str())) {
            continue;
        }
        found += 1;
        if is_dir {
            println!("{}", store::display_dir(&path).blue().bold());
        } else if sess.files.is_file(&path) {
            println!("{}", store::display(&path).magenta());
        } else {
            println!("{}", store::display(&path));
        }
    }
    if found == 0 {
        println!("{}", "no matches".dimmed());
    }
}

/// Print the lines of every entry containing `text`. Case is ignored when
/// asked, or when `text` is all lower case. File entries are skipped.
fn cmd_grep(sess: &mut Session, text: &str, ignore_case: bool) -> Result<()> {
    let ignore_case = ignore_case || !text.chars().any(char::is_uppercase);
    let needle = if ignore_case {
        text.to_lowercase()
    } else {
        text.to_string()
    };
    sess.unlock()?;
    let entries: Vec<String> = sess
        .store
        .walk()
        .into_iter()
        .filter(|(_, is_dir)| !is_dir)
        .map(|(p, _)| p)
        .collect();
    let (mut hits, mut failed) = (0, 0);
    for path in entries {
        let keys = sess.keys.as_ref().expect("unlocked");
        let Ok(content) = sess.store.read_entry(&path, keys) else {
            failed += 1;
            continue;
        };
        if fileio::marker_filename(&content).is_some() {
            continue;
        }
        let mut header = false;
        for line in content.lines() {
            let hay = if ignore_case {
                line.to_lowercase()
            } else {
                line.to_string()
            };
            let Some(at) = hay.find(&needle) else {
                continue;
            };
            if !header {
                println!("{}", store::display(&path).blue().bold());
                header = true;
                hits += 1;
            }
            // Lowercasing can change byte lengths; only highlight when the
            // match lines up with the original line.
            match line.get(at..at + needle.len()) {
                Some(m) if hay.len() == line.len() => println!(
                    "  {}{}{}",
                    &line[..at],
                    m.red().bold(),
                    &line[at + needle.len()..]
                ),
                _ => println!("  {line}"),
            }
        }
    }
    if hits == 0 {
        println!("{}", "no matches".dimmed());
    }
    if failed > 0 {
        println!(
            "{} {failed} entr{} couldn't be decrypted",
            "warning:".yellow().bold(),
            if failed == 1 { "y" } else { "ies" }
        );
    }
    Ok(())
}

/// Decrypt every entry (in `arg`, or the whole store) to find the file
/// entries, and bring the file index in line. For stores whose files were
/// imported before the index existed, or with another tool.
/// Nothing decrypted is printed.
fn cmd_reindex(sess: &mut Session, arg: Option<&str>) -> Result<()> {
    let dir = arg.map(|a| sess.resolve(a)).unwrap_or_default();
    if !dir.is_empty() && !sess.store.is_dir(&dir) && !sess.store.is_entry(&dir) {
        bail!("{} is not in the password store", store::display(&dir));
    }
    let entries: Vec<String> = sess
        .store
        .walk()
        .into_iter()
        .filter(|(p, is_dir)| {
            !is_dir
                && (dir.is_empty()
                    || *p == dir
                    || p.strip_prefix(&dir).is_some_and(|r| r.starts_with('/')))
        })
        .map(|(p, _)| p)
        .collect();
    let total = entries.len();
    let (mut files, mut found, mut dropped) = (0, 0, 0);
    let mut failed = Vec::new();
    sess.unlock()?;
    for path in &entries {
        let keys = sess.keys.as_ref().expect("unlocked");
        let Ok(content) = sess.store.read_entry(path, keys) else {
            failed.push(path.clone());
            continue;
        };
        let size = match fileio::marker_filename(&content) {
            Some(_) => match fileio::decode_payload(&content) {
                Ok(entry) => Some(entry.data.len() as u64),
                // A corrupt file entry is still a file; keep what's known.
                Err(_) => {
                    files += 1;
                    continue;
                }
            },
            None => None,
        };
        match (size.is_some(), sess.files.is_file(path)) {
            (true, false) => found += 1,
            (false, true) => dropped += 1,
            _ => {}
        }
        if size.is_some() {
            files += 1;
        }
        sess.files.learn(path, size);
    }
    println!(
        "checked {total} entr{}: {files} file entr{} ({found} newly found, {dropped} no longer files)",
        if total == 1 { "y" } else { "ies" },
        if files == 1 { "y" } else { "ies" },
    );
    if !failed.is_empty() {
        let shown: Vec<String> = failed.iter().take(5).map(|p| store::display(p)).collect();
        println!(
            "{} {} couldn't be decrypted and kept their old status: {}{}",
            "warning:".yellow().bold(),
            failed.len(),
            shown.join(", "),
            if failed.len() > 5 { ", …" } else { "" }
        );
    }
    Ok(())
}

/// Insert a new entry into the store.
fn cmd_insert(sess: &mut Session, flags: &[&str], positional: &[&str]) -> Result<()> {
    // `insert --file <os-file> <entry>` is an alias for `import`.
    if flags.contains(&"--file") {
        expect_args(positional, "insert --file [--force] <file> <entry>", 2, 2)?;
        let force = flags.contains(&"--force");
        return cmd_import(sess, positional[0], positional[1], force);
    }
    expect_args(positional, "insert [-m] [-e] [-f] <entry>", 1, 1)?;
    let has = |names: &[&str]| flags.iter().any(|f| names.contains(f));
    let path = sess.resolve(positional[0]);
    let shown = store::display(&path);
    if path.is_empty() || sess.store.is_dir(&path) {
        bail!("{shown} is a folder");
    }
    if sess.store.is_entry(&path)
        && !has(&["-f", "--force"])
        && !confirm(&format!("{shown} exists. Overwrite it?"))?
    {
        return Ok(());
    }
    let content = if has(&["-m", "--multiline"]) {
        println!("Enter the contents of {shown}, then Ctrl-D on a line of its own:");
        let mut text = Zeroizing::new(String::new());
        std::io::stdin().read_to_string(&mut text)?;
        text
    } else if has(&["-e", "--echo"]) {
        print!("Enter password for {shown}: ");
        std::io::stdout().flush()?;
        let mut line = Zeroizing::new(String::new());
        std::io::stdin().lock().read_line(&mut line)?;
        Zeroizing::new(format!("{}\n", line.trim_end_matches(['\r', '\n'])))
    } else {
        let first = Zeroizing::new(rpassword::prompt_password(format!(
            "Enter password for {shown}: "
        ))?);
        let again = Zeroizing::new(rpassword::prompt_password(format!(
            "Retype password for {shown}: "
        ))?);
        if first != again {
            bail!("the passwords don't match; nothing saved");
        }
        Zeroizing::new(format!("{}\n", *first))
    };
    if content.is_empty() {
        bail!("nothing entered; nothing saved");
    }
    sess.save(
        &path,
        content.as_bytes(),
        &format!("Add given password for {path} to store."),
    )?;
    let size = fileio::decode_payload(&content)
        .ok()
        .map(|f| f.data.len() as u64);
    sess.files.learn(&path, size);
    Ok(())
}

/// Import a file from the OS filesystem into the password store as a base64-encoded entry.
fn cmd_import(sess: &mut Session, os_file: &str, entry: &str, force: bool) -> Result<()> {
    let path = sess.resolve(entry);
    if sess.store.is_entry(&path) && !force {
        bail!(
            "{} already exists (pass --force to overwrite)",
            store::display(&path)
        );
    }
    if sess.store.is_dir(&path) {
        bail!("{} is a directory", store::display(&path));
    }
    let meta = std::fs::metadata(os_file).with_context(|| format!("cannot read {os_file}"))?;
    if !meta.is_file() {
        bail!("{os_file} is not a regular file");
    }
    if meta.len() > fileio::SIZE_LIMIT && !force {
        bail!(
            "{os_file} is {} bytes (limit {}); a password store is a poor home for large blobs — pass --force to import anyway",
            meta.len(),
            fileio::SIZE_LIMIT
        );
    }
    let data = std::fs::read(os_file).with_context(|| format!("cannot read {os_file}"))?;
    let filename = Path::new(os_file)
        .file_name()
        .context("path has no filename")?
        .to_string_lossy();
    let payload = Zeroizing::new(fileio::encode_payload(&filename, &data));
    sess.save(
        &path,
        payload.as_bytes(),
        &format!("Add file {filename} as {path}."),
    )?;
    sess.files.insert(&path, data.len() as u64);
    println!(
        "imported {os_file} ({} bytes) as {}",
        data.len(),
        store::display(&path).green()
    );
    Ok(())
}

/// Export a file entry from the password store back to the OS filesystem.
fn cmd_export(sess: &mut Session, entry: &str, dest: Option<&str>, force: bool) -> Result<()> {
    let path = sess.resolve_entry(entry)?;
    let content = sess.read(&path)?;
    if fileio::marker_filename(&content).is_none() {
        sess.files.remove(&path);
    }
    let file = fileio::decode_payload(&content)
        .with_context(|| format!("{} is not a file entry", store::display(&path)))?;
    sess.files.insert(&path, file.data.len() as u64);
    let mut dest = std::path::PathBuf::from(dest.unwrap_or(&file.filename));
    if dest.is_dir() {
        dest.push(&file.filename);
    }
    if dest.exists() && !force {
        bail!("{} already exists (pass -f to overwrite)", dest.display());
    }
    std::fs::write(&dest, &file.data)
        .with_context(|| format!("cannot write {}", dest.display()))?;
    println!(
        "exported {} ({} bytes) to {}",
        store::display(&path),
        file.data.len(),
        dest.display().to_string().green()
    );
    Ok(())
}

/// Print the help text for the command-line interface, with colored formatting.
fn print_help() {
    // Pad before coloring: ANSI escapes would otherwise count toward the width.
    let entry = |usage: &str, desc: &str| println!("  {} {desc}", format!("{usage:<33}").cyan());
    let section = |title: &str| println!("{}", title.yellow().bold());

    section("navigation");
    entry("ls [path]", "list the current (or given) directory");
    entry(
        "cd [path]",
        "change directory (.., /, subdirs); bare dir names also work",
    );
    entry("pwd", "print the current store directory");
    entry(
        "mkdir <path>",
        "create a directory (synced once it holds an entry)",
    );
    println!();
    section("passwords (paths relative to the current directory)");
    entry(
        "show [-c] <entry>",
        "print an entry, or -c to copy its first line",
    );
    entry(
        "insert [-m] [-e] [-f] <entry>",
        "add an entry (-m several lines, -e echo)",
    );
    entry(
        "generate [-c] [-i] [-f] <entry> [len]",
        "generate a password (-c copy it, -i replace only the first line)",
    );
    entry(
        "  -n | --url-safe | --digits | --hex",
        "letters & digits, or for database URLs, PINs, tokens",
    );
    entry(
        "  --no-ambiguous --exclude=CHARS",
        "leave out look-alikes (0O1lI|) or the characters given",
    );
    entry("edit <entry>", "edit in $VISUAL / $EDITOR");
    entry("rm [-r] [-f] <path>", "remove an entry or folder");
    entry("mv [-f] <src> <dest>", "rename/move");
    entry("cp [-f] <src> <dest>", "copy");
    entry("find <terms>", "list entries whose names match");
    entry("grep [-i] <text>", "search inside entries");
    println!();
    section("files (base64-encoded entries)");
    entry(
        "import [--force] <file> <entry>",
        "store a file as an entry (1 MiB soft limit)",
    );
    entry(
        "export [-f] <entry> [dest]",
        "write a file entry back out to disk",
    );
    entry(
        "reindex [path]",
        "find file entries made before ezer-pass tracked them",
    );
    println!();
    section("keys and setup (age)");
    entry(
        "init [-p folder] [age1...]",
        "make your identity if needed, and set who a folder is encrypted to",
    );
    entry(
        "migrate",
        "move a store made by `pass` from gpg to age, once",
    );
    println!();
    section("contexts (named stores: work, personal, ...)");
    entry("context [list]", "list them; ● marks the one you are in");
    entry(
        "context use <name>",
        "switch to one (and open it next time)",
    );
    entry(
        "context add <name> [path]",
        "add one, with a new store or the one at path",
    );
    entry(
        "context rm <name>",
        "forget one (its store is left as it is)",
    );
    entry("context rename <old> <new>", "give one another name");
    entry("context move <name> <path>", "move one's store to path");
    entry(
        "-C <context> <command>",
        "run one command in another context",
    );
    println!();
    section("version control");
    entry(
        "git <git-args>",
        "run git in the store (pull offered on startup, push on exit)",
    );
    println!();
    entry("help", "this text");
    entry("exit | quit | Ctrl-D", "leave");
}

#[cfg(test)]
mod tests {
    use super::tokenize;

    #[test]
    fn plain_words() {
        assert_eq!(
            tokenize("show servers/web1").unwrap(),
            ["show", "servers/web1"]
        );
        assert_eq!(tokenize("  ls  ").unwrap(), ["ls"]);
        assert!(tokenize("").unwrap().is_empty());
    }

    #[test]
    fn quotes_and_escapes() {
        assert_eq!(tokenize("cd 'my dir'").unwrap(), ["cd", "my dir"]);
        assert_eq!(tokenize("cd \"my dir\"").unwrap(), ["cd", "my dir"]);
        assert_eq!(tokenize("cd my\\ dir").unwrap(), ["cd", "my dir"]);
        assert_eq!(tokenize("echo \"a\\\"b\"").unwrap(), ["echo", "a\"b"]);
        assert!(tokenize("cd 'unterminated").is_err());
    }
}
