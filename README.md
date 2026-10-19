# ezer-pass

An interactive wrapper around [`pass`](https://www.passwordstore.org/), the standard
unix password manager. It fixes the two things that make a deeply nested store
painful: typing long entry paths, and storing anything that isn't text.

- **TUI** — a full-screen browser: directory and preview panes, masked
  passwords you reveal on demand, store-wide fuzzy search, and every command one
  keystroke (or `:`) away.
- **Explore mode** — a shell-like session that tracks a current directory inside
  your store: `cd servers`, `ls`, `show web1`, with tab completion and history.
- **File entries** — import any file as a base64-encoded pass entry and export it
  back out, with automatic detection on read.

All secret handling stays in `pass` itself: ezer-pass shells out to the `pass`
binary for every decrypt/encrypt, so GPG, pinentry, git integration, and the
clipboard behave exactly as they do with plain `pass`. Only the directory *names*
(which are not encrypted) are read directly from the store.

## Install

Requires `pass` and a working password store (`pass init <gpg-id>`).

```sh
cargo install --path .
```

## TUI

Run `ezer-pass` with no arguments:

```
 ezer-pass  /servers                                          origin/master ↑1
┌ entries ──────────────────┐┌ /servers/web1 ──────────────────────────────────┐
│ prod/                     ││••••••••••••  space to reveal                    │
│▌web1                      ││user: alice                                      │
│                           ││url: https://example.com                         │
└───────────────────────────┘└─────────────────────────────────────────────────┘
jk move  hl dirs  ⏎ open  space reveal  c copy  / search  : command  ? help  q quit
```

| Key | Action |
|---|---|
| `j` `k` / arrows | move (`PgUp` `PgDn` `Home` `End`) |
| `l` / `→` / `Enter` | open a directory, or decrypt the selected entry |
| `h` / `←` / `Backspace` | parent directory |
| `space` | reveal / hide the password line |
| `c` | copy the password (via `pass show -c`) |
| `/` | fuzzy-search every entry and directory in the store |
| `e` | edit in `$EDITOR` |
| `d` | delete (asks first) |
| `g` `i` `m` `r` `I` `x` | generate, insert, mkdir, rename, import, export — each opens the command line pre-filled |
| `:` | run any explore-mode command (`:git log`, `:find bank`, `:cp a b`, ...) |
| `R` / `?` / `q` | refresh / help / quit |

- Only the first line of an entry (the password) is masked; the rest (`user:`,
  `url:`, notes) is shown once decrypted. A revealed password re-masks after
  `PASSWORD_STORE_CLIP_TIME` (45 s), and plaintext is dropped as soon as the
  selection moves.
- Decryption first runs with gpg's pinentry disabled, so when gpg-agent already
  has your passphrase the screen never flickers. Otherwise the TUI steps aside
  for pinentry, then comes straight back. The same hand-over happens for
  `$EDITOR`, `pass insert` and command output; it all stays on the alternate
  screen, so a freshly generated password doesn't linger in shell scrollback.

## Explore mode

Run `ezer-pass repl` for the line-based explorer:

```
$ ezer-pass repl
ezer-pass — exploring /home/you/.password-store (`help` for commands)
ezer:/ > ls
apple/
servers/
ezer:/ > cd servers
ezer:/servers > ls
prod/
web1
ezer:/servers > show -c web1
Copied servers/web1 to clipboard. Will clear in 45 seconds.
ezer:/servers > cd prod          # tab completion works: cd p<Tab>
ezer:/servers/prod > show db
hunter2
```

- Paths are relative to the current directory; `..`, `.` and store-absolute
  `/apple/id` work like a shell, and `..` can never escape the store.
- Typing a bare directory name (`servers`) is shorthand for `cd servers`.
- `Ctrl-D` or `exit` leaves; command history is kept in
  `~/.local/state/ezer-pass/history` (commands only, never secrets).

### Commands

| Command | Description |
|---|---|
| `ls [path]` | list a store directory |
| `cd [path]` / `pwd` | move around the store |
| `mkdir <path>` | create a directory (pass has none; git tracks it once it holds an entry) |
| `show [-c] <entry>` | print an entry, or `-c` to copy via pass |
| `insert [-m] [-e] <entry>` | add an entry (interactive) |
| `generate [-n] <entry> [len]` | generate a password |
| `edit <entry>` | edit in `$EDITOR` |
| `rm [-r] <path>` | remove an entry or directory |
| `mv <src> <dest>` / `cp <src> <dest>` | rename or copy |
| `find <terms>` / `grep <pattern>` | search names / decrypted contents |
| `import [--force] <file> <entry>` | store a file as a base64 entry |
| `export [-f] <entry> [dest]` | write a file entry back to disk |
| `git <git-args>` | run git in the store (`git log`, `git remote add`, ...) |

Every command also works one-shot from your shell with the same syntax:
`ezer-pass show servers/web1`, `ezer-pass import cert.pem servers/cert`.
Anything not listed (otp, init, ...) is out of scope — use plain `pass`.

## Git sync

If the store is a git repository with a configured upstream, the TUI and the
explore session keep it synced automatically:

- **On startup** it asks `pull from origin/master? [Y/n]` and runs
  `git pull --ff-only` on yes (a diverged store is reported and left for you to
  resolve; a failed pull never blocks the session).
- **On exit** — `q` in the TUI, `exit` or `Ctrl-D` in explore mode — if you have unpushed commits, it asks
  `push 2 commits to origin/master? [Y/n]` and pushes on yes.

Stores without git (or without a remote) are left completely alone, and one-shot
commands never touch git.

## File entries

```
ezer:/ > import backup-codes.pdf apple/backup-codes
imported backup-codes.pdf (48213 bytes) as /apple/backup-codes
ezer:/ > show apple/backup-codes
file entry: backup-codes.pdf (48213 bytes) — use `export apple/backup-codes` to write it out
ezer:/ > export apple/backup-codes ~/Desktop/
exported /apple/backup-codes (48213 bytes) to /home/you/Desktop/backup-codes.pdf
```

A file entry is a normal pass entry whose first line is a marker, followed by the
base64-encoded file — so it stays fully readable by plain `pass` and syncs like
any other entry:

```
!ezer-pass:b64:backup-codes.pdf
JVBERi0xLjcKJcTl8uXrp/Og0MTGCjQgMCBvYmoK...
```

`show` recognizes the marker and reports file metadata instead of dumping base64;
`export` decodes it, defaulting to the original filename. Imports over 1 MiB are
refused without `--force` — a password store is a poor home for large blobs.

## Environment

| Variable | Effect |
|---|---|
| `PASSWORD_STORE_DIR` | store location (default `~/.password-store`), same as pass |
| `PASSWORD_STORE_CLIP_TIME` | seconds before a revealed password re-masks in the TUI (default 45), same as pass's clipboard timeout |
| `XDG_STATE_HOME` | history file location (default `~/.local/state`) |

## Development

```sh
cargo test
```

To try it against a throwaway store instead of your real one:

```sh
export GNUPGHOME=$(mktemp -d) && chmod 700 "$GNUPGHOME"
gpg --batch --passphrase '' --quick-gen-key test@ezer default default never
export PASSWORD_STORE_DIR=$(mktemp -d) && pass init test@ezer
pass generate -n servers/web1 20
cargo run
```
