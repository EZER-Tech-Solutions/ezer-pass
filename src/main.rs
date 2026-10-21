//! @file main.rs
//! @author andrewb242
//!
//! This is the main entry point of the ezer-pass program, a password manager written in Rust. It picks the context (named store) to work in, opens its password store, processes command-line arguments, and either runs a REPL (Read-Eval-Print Loop) for interactive use or executes commands based on the provided arguments. The program handles errors gracefully and returns appropriate exit codes.

mod agent;
mod browserpass;
mod clipboard;
mod commands;
mod context;
mod crypto;
mod fileio;
mod git;
mod index;
mod password;
mod repl;
mod setup;
mod store;
mod tui;

use std::io::IsTerminal;
use std::process::ExitCode;

/// The main entry point of the program.
fn main() -> ExitCode {
    // Read the command-line arguments:
    // - Skips the first arg (the program name)
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // A detached copy of ezer-pass that clears the clipboard later.
    if args.first().map(String::as_str) == Some(clipboard::CLEAR_ARG) {
        clipboard::run_clearer(args.get(1).map_or("", String::as_str));
        return ExitCode::SUCCESS;
    }
    // The agent, started by `unlock`.
    if args.first().map(String::as_str) == Some(agent::AGENT_ARG) {
        return match agent::serve() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e:#}");
                ExitCode::FAILURE
            }
        };
    }
    // The browser extension's host works on whichever stores the extension
    // names, not on a context.
    if args.first().map(String::as_str) == Some("browserpass") {
        return match browserpass::run(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("ezer-pass: {e:#}");
                ExitCode::FAILURE
            }
        };
    }

    // `-C <name>` / `--context <name>` picks the context (named store) for
    // this run, ahead of `$EZER_PASS_CONTEXT` and the one last used.
    let named = match take_context_flag(&mut args) {
        Ok(named) => named,
        Err(e) => {
            eprintln!("ezer-pass: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    // Open the context's store. `init` makes one if there is none, and
    // `init` on a context that doesn't exist yet adds it, with its store in
    // `$XDG_DATA_HOME/ezer-pass/<name>`.
    let command = args.first().map(String::as_str);
    let opened = open(named.as_deref(), command);
    let (context, store) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            eprintln!("ezer-pass: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    // A store made by `pass` is unreadable until it is moved to age.
    if store.uses_gpg() && !matches!(command, Some("migrate" | "git" | "help" | "context")) {
        eprintln!(
            "ezer-pass: this store is still encrypted with gpg — run `ezer-pass migrate` to move it to age (gpg asks for your passphrase one last time)"
        );
        return ExitCode::FAILURE;
    }

    // No arguments opens the TUI, `repl` the line-based explorer; anything
    // else is a one-shot command.
    let repl = args.len() == 1 && args[0] == "repl";
    let interactive = args.is_empty() || repl;
    // Initialize the session, which holds:
    // - The store
    // - The interactive flag
    // - The current working directory within the store, which is used for relative paths.
    let mut session = commands::Session::new(context, store, interactive);
    let result = if args.is_empty() {
        if std::io::stdout().is_terminal() && std::io::stdin().is_terminal() {
            tui::run(session)
        } else {
            Err(anyhow::anyhow!(
                "the TUI needs a terminal (use `ezer-pass repl` or one-shot commands)"
            ))
        }
    } else if repl {
        repl::run(&mut session)
    } else {
        // Otherwise, run the command-line interface.
        commands::run(&mut session, &args).map(|_| ())
    };

    // Return an exit code based on the result of the command execution.
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ezer-pass: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Take a leading `-C <name>`, `-C<name>`, `--context <name>` or
/// `--context=<name>` out of `args`.
fn take_context_flag(args: &mut Vec<String>) -> anyhow::Result<Option<String>> {
    let Some(first) = args.first() else {
        return Ok(None);
    };
    let (name, used) = if first == "-C" || first == "--context" {
        let name = args
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("{first} needs a context name"))?;
        (name.clone(), 2)
    } else if let Some(name) = first.strip_prefix("--context=") {
        (name.to_string(), 1)
    } else if let Some(name) = first.strip_prefix("-C") {
        (name.to_string(), 1)
    } else {
        return Ok(None);
    };
    if name.is_empty() {
        anyhow::bail!("{first} needs a context name");
    }
    args.drain(..used);
    Ok(Some(name))
}

/// The context to work in and its store.
fn open(
    named: Option<&str>,
    command: Option<&str>,
) -> anyhow::Result<(context::Context, store::Store)> {
    let init = command == Some("init");
    let context = match context::current(named) {
        Ok(context) => context,
        Err(_) if init && named.is_some() => {
            let name = named.expect("checked");
            let context = context::add(name, None)?;
            println!("added context {name} at {}", context::tilde(&context.path));
            context
        }
        Err(e) => return Err(e),
    };
    let store = match command {
        Some("init") => store::Store::create(context.path.clone())?,
        // `context use default` must work even when this store is gone.
        Some("context") => store::Store::unchecked(context.path.clone()),
        _ => store::Store::open(context.path.clone()).map_err(|e| {
            if context.name == context::DEFAULT || named.is_some() {
                e
            } else {
                e.context(format!(
                    "in context {} (`ezer-pass context use default` goes back)",
                    context.name
                ))
            }
        })?,
    };
    Ok((context, store))
}
