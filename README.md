# ezer-pass

An interactive wrapper around [`pass`](https://www.passwordstore.org/), the standard
unix password manager. It fixes the two things that make a deeply nested store
painful: typing long entry paths, and storing anything that isn't text.

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

## Explore mode

Run `ezer-pass` with no arguments:

```
$ ezer-pass
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
| `show [-c] <entry>` | print an entry, or `-c` to copy via pass |
| `insert [-m] [-e] <entry>` | add an entry (interactive) |
| `generate [-n] <entry> [len]` | generate a password |
| `edit <entry>` | edit in `$EDITOR` |
| `rm [-r] <path>` | remove an entry or directory |
| `mv <src> <dest>` / `cp <src> <dest>` | rename or copy |
| `find <terms>` / `grep <pattern>` | search names / decrypted contents |
| `import [--force] <file> <entry>` | store a file as a base64 entry |
| `export [-f] <entry> [dest]` | write a file entry back to disk |

Every command also works one-shot from your shell with the same syntax:
`ezer-pass show servers/web1`, `ezer-pass import cert.pem servers/cert`.
Anything not listed (git, otp, init, ...) is out of scope — use plain `pass`.

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
