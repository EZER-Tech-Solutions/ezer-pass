//! Full-screen browser for the store: a directory pane, a preview pane that
//! decrypts on demand, store-wide fuzzy search, and a `:` command line that
//! accepts every REPL command.

mod app;
mod fuzzy;
mod ui;

use crate::commands::{self, Session};
use crate::pass;
use anyhow::Result;
use app::{App, Effect, Pause};
use colored::Colorize;
use ratatui::DefaultTerminal;
use ratatui::crossterm::cursor::MoveTo;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, disable_raw_mode, enable_raw_mode};
use std::io::{self, Write};
use std::time::Duration;

pub fn run(sess: Session) -> Result<()> {
    let mut app = App::new(sess);
    let mut terminal = ratatui::try_init()?;
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    if let Ok(Some(warning)) = &result {
        eprintln!("{}", warning.yellow());
    }
    result.map(|_| ())
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<Option<String>> {
    let mut exit_warning = None;
    while !app.quit {
        app.tick();
        terminal.draw(|f| ui::draw(f, app))?;
        // Wake periodically so a revealed password re-masks on time.
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        // Resize and other events just fall through to a redraw.
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let effect = app.on_key(key);
            exit_warning = perform(terminal, app, effect)?;
        }
    }
    Ok(exit_warning)
}

/// Carry out an effect. Returns a warning to print once the TUI has closed.
fn perform(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    effect: Effect,
) -> Result<Option<String>> {
    let mut exit_warning = None;
    match effect {
        Effect::None => {}
        Effect::Decrypt { path, reveal } => {
            // Try without a passphrase prompt first: if gpg-agent has it
            // cached the screen never flickers.
            let result = match pass::capture_no_prompt(&["show", "--", &path]) {
                Ok(content) => Ok(content),
                Err(_) => suspended(terminal, || pause_on_err(pass::show(&path)))?,
            };
            app.finish_decrypt(path, result, reveal);
        }
        Effect::Copy(path) => {
            let args = ["show", "-c", "--", path.as_str()];
            let result = match pass::run_no_prompt(&args) {
                Ok(()) => Ok(()),
                Err(_) => suspended(terminal, || pause_on_err(pass::run_interactive(&args)))?,
            };
            app.finish_copy(&path, result);
        }
        Effect::Run { argv, pause } => {
            let result = suspended(terminal, || {
                println!("{} {}", ":".green().bold(), argv.join(" "));
                let result = commands::run(&mut app.sess, &argv).map(|_| ());
                if let Err(e) = &result {
                    eprintln!("{} {e:#}", "error:".red().bold());
                }
                if matches!(pause, Pause::Always) || result.is_err() {
                    wait_for_enter();
                }
                result
            })?;
            app.finish_run(result);
        }
        Effect::Pull => {
            let result = suspended(terminal, || {
                pause_on_err(pass::run_interactive(&["git", "pull", "--ff-only"]))
            })?;
            app.finish_sync("pulled", result);
        }
        Effect::Push => {
            let result = suspended(terminal, || {
                pause_on_err(pass::run_interactive(&["git", "push"]))
            })?;
            if let Err(e) = result {
                exit_warning = Some(format!("warning: {e:#} — your commits are still local"));
            }
            app.quit = true;
        }
    }
    Ok(exit_warning)
}

/// Hand the terminal to `f` (pinentry, $EDITOR, pass prompts, command
/// output) and take it back afterwards. Output stays on the alternate
/// screen, so anything printed — `generate`'s new password included — is
/// gone when the TUI redraws instead of lingering in shell scrollback.
fn suspended<T>(terminal: &mut DefaultTerminal, f: impl FnOnce() -> T) -> Result<T> {
    terminal.clear()?;
    terminal.show_cursor()?;
    disable_raw_mode()?;
    execute!(io::stdout(), MoveTo(0, 0))?;
    let out = f();
    // Full-screen children ($EDITOR, pinentry-curses) leave the alternate
    // screen when they exit; re-enter it before redrawing.
    execute!(io::stdout(), EnterAlternateScreen)?;
    enable_raw_mode()?;
    terminal.clear()?;
    Ok(out)
}

/// On failure, print the error and hold the screen so gpg/git's own stderr
/// can be read before the TUI comes back.
fn pause_on_err<T>(result: Result<T>) -> Result<T> {
    if let Err(e) = &result {
        eprintln!("{} {e:#}", "error:".red().bold());
        wait_for_enter();
    }
    result
}

fn wait_for_enter() {
    print!("\n{}", "press Enter to return to ezer-pass".dimmed());
    let _ = io::stdout().flush();
    let _ = io::stdin().read_line(&mut String::new());
}
