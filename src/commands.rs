use crate::{fileio, pass, store};
use anyhow::{bail, Context, Result};
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
            bail!("{} is a directory (try `ls {input}`)", store::display(&path));
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
                eprintln!("note: cd only has an effect inside the interactive session (run `ezer-pass` with no arguments)");
            }
        }
        "pwd" => println!("{}", store::display(&sess.cwd)),
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
            expect_args(&positional, "generate [-n] [-c] [-f] <entry> [length]", 1, 2)?;
            let path = sess.resolve(positional[0]);
            let mut pass_args = vec!["generate"];
            pass_args.extend(&flags);
            pass_args.push("--");
            pass_args.push(&path);
            pass_args.extend(positional.get(1));
            pass::run_interactive(&pass_args)?
        }
        "edit" => {
            expect_args(&positional, "edit <entry>", 1, 1)?;
            let path = sess.resolve(positional[0]);
            pass::run_interactive(&["edit", "--", &path])?
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
            "{} {} ({} bytes) — use `export {arg}` to write it out",
            "file entry:".yellow(),
            entry.filename,
            entry.data.len()
        );
    } else {
        print!("{content}");
    }
    Ok(())
}

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

fn cmd_export(sess: &Session, entry: &str, dest: Option<&str>, force: bool) -> Result<()> {
    let path = sess.resolve_entry(entry)?;
    let content = pass::show(&path)?;
    let file = fileio::decode_payload(&content).with_context(|| {
        format!("{} is not a file entry", store::display(&path))
    })?;
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

fn print_help() {
    let help = "\
navigation
  ls [path]                    list the current (or given) directory
  cd [path]                    change directory (.., /, subdirs); bare dir names also work
  pwd                          print the current store directory

passwords (forwarded to pass, paths relative to the current directory)
  show [-c] <entry>            print an entry, or -c to copy to the clipboard
  insert [-m] [-e] <entry>     add an entry (interactive)
  generate [-n] <entry> [len]  generate a new password
  edit <entry>                 edit in $EDITOR
  rm [-r] <path>               remove an entry or directory
  mv <src> <dest>              rename/move
  cp <src> <dest>              copy
  find <terms>                 list entries matching names
  grep <pattern>               search inside decrypted entries

files (base64-encoded pass entries)
  import [--force] <file> <entry>   store a file as an entry (1 MiB soft limit)
  export [-f] <entry> [dest]        write a file entry back out to disk

  help                         this text
  exit | quit | Ctrl-D         leave";
    println!("{help}");
}

#[cfg(test)]
mod tests {
    use super::tokenize;

    #[test]
    fn plain_words() {
        assert_eq!(tokenize("show servers/web1").unwrap(), ["show", "servers/web1"]);
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
