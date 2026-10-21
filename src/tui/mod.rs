//! Full-screen browser for the store: a directory pane, a preview pane that
//! decrypts on demand, store-wide fuzzy search, and a `:` command line that
//! accepts every REPL command.

mod app;
mod fuzzy;
mod generate;
mod history;
mod palette;
mod preview;
mod sync;
mod theme;
mod ui;

use crate::commands::{self, Session};
use crate::{clipboard, fileio, git};
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
    theme::load();
    let mut terminal = ratatui::try_init()?;
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    if let Some(warning) = &app.exit_warning {
        eprintln!("{}", warning.yellow());
    }
    result
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.quit {
        app.tick();
        if let Some(effect) = app.next.take() {
            perform(terminal, app, effect)?;
        }
        if ui::preview_fits(terminal.size()?.width) {
            app.load_preview();
        } else {
            app.preview = None;
        }
        terminal.draw(|f| ui::draw(f, app))?;
        // Wake periodically so a revealed password re-masks on time, and
        // often while a spinner is turning.
        let wait = if app.busy() { 80 } else { 250 };
        if !event::poll(Duration::from_millis(wait))? {
            continue;
        }
        // Resize and other events just fall through to a redraw.
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let effect = app.on_key(key);
            perform(terminal, app, effect)?;
        }
    }
    Ok(())
}

/// Carry out an effect, once the identity is unlocked if it needs to be.
fn perform(terminal: &mut DefaultTerminal, app: &mut App, effect: Effect) -> Result<()> {
    match app.gate(effect) {
        Effect::None => {}
        Effect::Unlock => {
            // Show "unlocking" while the key is stretched (about a second).
            terminal.draw(|f| ui::draw(f, app))?;
            let next = app.try_unlock();
            perform(terminal, app, next)?;
        }
        Effect::Decrypt(path) => {
            let keys = app.sess.keys.as_ref().expect("gated");
            let result = app
                .sess
                .store
                .read_entry(&path, keys)
                .map(|content| content.to_string());
            app.finish_decrypt(path, result);
        }
        Effect::Copy(path) => {
            let keys = app.sess.keys.as_ref().expect("gated");
            let result = app.sess.store.read_entry(&path, keys).and_then(|content| {
                if fileio::marker_filename(&content).is_some() {
                    anyhow::bail!("{} is a file entry", crate::store::display(&path));
                }
                clipboard::copy(content.lines().next().unwrap_or(""))
            });
            app.finish_copy(&path, result);
        }
        Effect::Run(batch) => {
            let (done, result) = suspended(terminal, || {
                let mut done = 0;
                let mut result = Ok(());
                for job in &batch.jobs {
                    println!("{} {}", ":".green().bold(), job.argv.join(" "));
                    if let Err(e) = commands::run(&mut app.sess, &job.argv) {
                        // The TUI's footer shows the error; only git's own
                        // stderr needs holding on screen to be read.
                        if job.argv[0] == "git" {
                            eprintln!("{} {e:#}", "error:".red().bold());
                            wait_for_enter();
                        }
                        result = Err(e);
                        break;
                    }
                    done += 1;
                }
                if matches!(batch.pause, Pause::Always) && result.is_ok() {
                    wait_for_enter();
                }
                (done, result)
            })?;
            app.finish_run(&batch, done, result);
        }
        Effect::Generate {
            path,
            spec,
            length,
            in_place,
            login,
        } => match path {
            Some(path) => {
                let result = commands::generate(
                    &mut app.sess,
                    &path,
                    &spec,
                    length as usize,
                    in_place,
                    login.as_deref(),
                )
                .and_then(|password| clipboard::copy(&password));
                let next = app.finish_generate(path, result);
                perform(terminal, app, next)?;
            }
            None => {
                let result = spec
                    .generate(length as usize)
                    .and_then(|password| clipboard::copy(&password));
                app.finish_generate_unsaved(result);
            }
        },
        Effect::Pull => {
            let result = suspended(terminal, || {
                pause_on_err(git::interactive(
                    app.sess.store.root(),
                    sync::Op::Pull.args(),
                ))
            })?;
            app.finish_sync("pulled", result);
        }
        Effect::Push => {
            let result = suspended(terminal, || {
                pause_on_err(git::interactive(
                    app.sess.store.root(),
                    sync::Op::Push.args(),
                ))
            })?;
            app.after_push(result.err());
        }
    }
    Ok(())
}

/// Hand the terminal to `f` ($EDITOR, prompts, command output) and take
/// it back afterwards. Output stays on the alternate screen, so anything
/// printed — `generate`'s new password included — is gone when the TUI
/// redraws instead of lingering in shell scrollback.
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

/// On failure, print the error and hold the screen so git's own stderr
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
