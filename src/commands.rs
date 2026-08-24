use crate::{fileio, pass, store};
use anyhow::{Context, Result, bail};
use colored::Colorize;
use std::path::Path;

pub struct Session {
    pub store: store::Store,
    /// Current store directory ("" = root).
    pub cwd: String,
    pub interactive: bool,
}

pub enum Outcome {
    Continue,
    Exit,
}

impl Session {
    pub fn new(store: store::Store, interactive: bool) -> Self {
        // initialize the session with the store and set the current working directory to root
        Session {
            store,
            cwd: String::new(),
            interactive,
        }
    }

    fn resolve(&self, input: &str) -> String {
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

/// Run a command in the given session, returning an Outcome indicating whether to continue or exit.
pub fn run(sess: &mut Session, argv: &[String]) -> Result<Outcome> {
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
        "show" => {
            expect_args(&positional, "show [-c] <entry>", 1, 1)?;
            let clip = flags.iter().any(|f| *f == "-c" || *f == "--clip");
            cmd_show(sess, positional[0], clip)?
        }
        "insert" => cmd_insert(sess, &flags, &positional)?,
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
                "generate [-n] [-c] [-f] [--url-safe|--hex|--digits|--base-64] <entry> [length]",
                1,
                2,
            )?;
            let path = sess.resolve(positional[0]);
            let charset_flags: Vec<&&str> = flags
                .iter()
                .filter(|f| matches!(**f, "--url-safe" | "--hex" | "--digits" | "--base-64"))
                .collect();
            if charset_flags.len() > 1 {
                bail!("only one of --url-safe, --hex, --digits, --base-64 may be given");
            }
            let charset = charset_flags.first().map(|f| match **f {
                "--url-safe" => "A-Za-z0-9_-",
                "--hex" => "0-9a-f",
                "--digits" => "0-9",
                "--base-64" => "A-Za-z0-9+/",
                _ => unreachable!(),
            });
            if charset.is_some() && flags.iter().any(|f| *f == "-n" || *f == "--no-symbols") {
                bail!("-n/--no-symbols overrides --url-safe/--hex/--digits/--base-64 in pass; drop one");
            }
            let mut pass_args = vec!["generate"];
            pass_args.extend(flags.iter().filter(|f| !charset_flags.contains(f)));
            pass_args.push("--");
            pass_args.push(&path);
            pass_args.extend(positional.get(1));
            match charset {
                Some(set) => {
                    pass::run_interactive_env(&pass_args, &[("PASSWORD_STORE_CHARACTER_SET", set)])?
                }
                None => pass::run_interactive(&pass_args)?,
            }
        }
        "edit" => {
            expect_args(&positional, "edit <entry>", 1, 1)?;
            let path = sess.resolve(positional[0]);
            // pass's cmd_edit takes no options and rejects a `--` separator.
            pass::run_interactive(&["edit", &path])?
        }
        "rm" => {
            expect_args(&positional, "rm [-r] [-f] <path>", 1, 1)?;
            let path = sess.resolve(positional[0]);
            let mut pass_args = vec!["rm"];
            pass_args.extend(&flags);
            pass_args.push("--");
            pass_args.push(&path);
            pass::run_interactive(&pass_args)?
        }
        "mv" | "cp" => {
            expect_args(&positional, "mv|cp [-f] <src> <dest>", 2, 2)?;
            let src = sess.resolve(positional[0]);
            let dest = sess.resolve(positional[1]);
            let mut pass_args = vec![cmd];
            pass_args.extend(&flags);
            pass_args.push("--");
            pass_args.push(&src);
            pass_args.push(&dest);
            pass::run_interactive(&pass_args)?
        }
        "git" => {
            if args.is_empty() {
                bail!("usage: git <git-args>");
            }
            let mut pass_args = vec!["git"];
            pass_args.extend(args.iter().map(String::as_str));
            pass::run_interactive(&pass_args)?
        }
        "find" | "grep" => {
            if args.is_empty() {
                bail!("usage: {cmd} <search-terms>");
            }
            let mut pass_args = vec![cmd];
            pass_args.extend(args.iter().map(String::as_str));
            pass::run_interactive(&pass_args)?
        }
        "help" | "?" => print_help(),
        "exit" | "quit" if sess.interactive => return Ok(Outcome::Exit),
        // Bare directory name acts like `cd` in the REPL.
        _ if sess.interactive && args.is_empty() && sess.store.is_dir(&sess.resolve(cmd)) => {
            sess.cwd = sess.resolve(cmd);
        }
        _ => bail!("unknown command {cmd:?} (try `help`; for anything else use plain `pass`)"),
    }
    Ok(Outcome::Continue)
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
fn cmd_show(sess: &Session, arg: &str, clip: bool) -> Result<()> {
    let path = sess.resolve_entry(arg)?;
    if clip {
        // pass prints its own "Copied ... to clipboard" and handles clearing.
        return pass::run_interactive(&["show", "-c", "--", &path]);
    }
    let content = pass::show(&path)?;
    if fileio::marker_filename(&content).is_some() {
        let entry = fileio::decode_payload(&content)?;
        println!(
            "{} {} ({} bytes) — use {} to write it out",
            "file entry:".yellow().bold(),
            entry.filename,
            entry.data.len(),
            format!("`export {arg}`").green()
        );
    } else {
        print!("{content}");
    }
    Ok(())
}

/// Insert a new entry into the store.
fn cmd_insert(sess: &Session, flags: &[&str], positional: &[&str]) -> Result<()> {
    // `insert --file <os-file> <entry>` is an alias for `import`.
    if flags.contains(&"--file") {
        expect_args(positional, "insert --file [--force] <file> <entry>", 2, 2)?;
        let force = flags.contains(&"--force");
        return cmd_import(sess, positional[0], positional[1], force);
    }
    expect_args(positional, "insert [-m] [-e] [-f] <entry>", 1, 1)?;
    let path = sess.resolve(positional[0]);
    let mut pass_args = vec!["insert"];
    pass_args.extend(flags);
    pass_args.push("--");
    pass_args.push(&path);
    pass::run_interactive(&pass_args)
}

/// Import a file from the OS filesystem into the password store as a base64-encoded entry.
fn cmd_import(sess: &Session, os_file: &str, entry: &str, force: bool) -> Result<()> {
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
    let payload = fileio::encode_payload(&filename, &data);
    pass::insert_multiline(&path, payload.as_bytes())?;
    println!(
        "imported {os_file} ({} bytes) as {}",
        data.len(),
        store::display(&path).green()
    );
    Ok(())
}

/// Export a file entry from the password store back to the OS filesystem.
fn cmd_export(sess: &Session, entry: &str, dest: Option<&str>, force: bool) -> Result<()> {
    let path = sess.resolve_entry(entry)?;
    let content = pass::show(&path)?;
    let file = fileio::decode_payload(&content)
        .with_context(|| format!("{} is not a file entry", store::display(&path)))?;
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
    println!();
    section("passwords (forwarded to pass, paths relative to the current directory)");
    entry(
        "show [-c] <entry>",
        "print an entry, or -c to copy to the clipboard",
    );
    entry("insert [-m] [-e] <entry>", "add an entry (interactive)");
    entry(
        "generate [-n] [--url-safe|--hex|--digits|--base-64] <entry> [len]",
        "generate a password (-n alnum only, or pick a charset)",
    );
    entry("edit <entry>", "edit in $EDITOR");
    entry("rm [-r] <path>", "remove an entry or directory");
    entry("mv <src> <dest>", "rename/move");
    entry("cp <src> <dest>", "copy");
    entry("find <terms>", "list entries matching names");
    entry("grep <pattern>", "search inside decrypted entries");
    entry(
        "git <git-args>",
        "run git in the store (pull offered on startup, push on exit)",
    );
    println!();
    section("files (base64-encoded pass entries)");
    entry(
        "import [--force] <file> <entry>",
        "store a file as an entry (1 MiB soft limit)",
    );
    entry(
        "export [-f] <entry> [dest]",
        "write a file entry back out to disk",
    );
    println!();
    section("version control (pass git)");
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
