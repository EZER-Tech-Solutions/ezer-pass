//! @file main.rs
//! @author andrewb242
//!
//! This is the main entry point of the ezer-pass program, a password manager written in Rust. It initializes the password store, processes command-line arguments, and either runs a REPL (Read-Eval-Print Loop) for interactive use or executes commands based on the provided arguments. The program handles errors gracefully and returns appropriate exit codes.

mod commands;
mod fileio;
mod pass;
mod repl;
mod store;
mod tui;

use std::io::IsTerminal;
use std::process::ExitCode;

/// The main entry point of the program.
fn main() -> ExitCode {
    // Read the command-line arguments:
    // - Skips the first arg (the program name)
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Initialize the password store:
    // - Searches initializes the store from the `PASSWORD_STORE_DIR` environment variable.
    // - or defaults to `$HOME/.password-store`.
    let store = match store::Store::open() {
        Ok(store) => store,
        Err(e) => {
            eprintln!("ezer-pass: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    // No arguments opens the TUI, `repl` the line-based explorer; anything
    // else is a one-shot command.
    let repl = args.len() == 1 && args[0] == "repl";
    let interactive = args.is_empty() || repl;
    // Initialize the session, which holds:
    // - The store
    // - The interactive flag
    // - The current working directory within the store, which is used for relative paths.
    let mut session = commands::Session::new(store, interactive);
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
