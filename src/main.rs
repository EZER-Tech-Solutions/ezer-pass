mod commands;
mod fileio;
mod pass;
mod repl;
mod store;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let store = match store::Store::open() {
        Ok(store) => store,
        Err(e) => {
            eprintln!("ezer-pass: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let interactive = args.is_empty();
    let mut session = commands::Session::new(store, interactive);
    let result = if interactive {
        repl::run(&mut session)
    } else {
        commands::run(&mut session, &args).map(|_| ())
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ezer-pass: {e:#}");
            ExitCode::FAILURE
        }
    }
}
