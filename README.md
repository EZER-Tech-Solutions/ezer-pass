# ezer-pass

A password manager for the terminal, in the spirit of
[`pass`](https://www.passwordstore.org/): one encrypted file per entry, in a
folder tree you can sync with git. Entries are encrypted with
[age](https://age-encryption.org/) and decrypted in-process, so there is no gpg,
no pinentry and no agent, and searching the whole store takes moments. It also
fixes the two things that make a deeply nested store painful: typing long entry
paths, and storing anything that isn't text.

- **TUI** — a full-screen browser: a collapsible tree with fuzzy filtering,
  cut/copy/paste and in-place rename, a recent-entries pane for one-key copies,
  and every command one keystroke (or `:`) away.
- **Explore mode** — a shell-like session that tracks a current directory inside
  your store: `cd servers`, `ls`, `show web1`, with tab completion and history.
- **File entries** — import any file as a base64-encoded entry and export it
  back out, with automatic detection on read.

Entry *contents* are encrypted; folder and entry *names* are not, just as with
`pass`.

## Install

```sh
cargo install --path .
ezer-pass init        # makes your identity (asks for a passphrase) and the store
```

Copying to the clipboard needs `wl-copy`/`wl-paste` (Wayland) or `xclip` (X11);
sync and history need `git`.

Coming from `pass`? See [Moving from pass](#moving-from-pass).

## TUI

Run `ezer-pass` with no arguments:

```
╭ ezer-pass  ──────────────────────────────────────────── ~/.password-store ╮
│   ▼ servers/                                                              │
│▌  ├─▶ prod/                                                               │
│▌✂ ├─• web1                                                                │
│   └─▪ tls-cert                                                            │
│   ▶ personal/                                                             │
╰────────────────────────────────────────────────────────────────────── 3/5 ╯
╭ recent ─────────────────────────╮╭ info ──────────────────────────────────╮
│ 1 /servers/web1       ▰▰▰▱▱ 32s ││ /servers/web1                          │
│ 2 /apple/id                     ││ password · modified 3 days ago         │
│                                 ││ buffer  ✂ cut web1   p paste           │
│                                 ││ marks   ▌1 marked   x cut y copy d …   │
│                                 ││ git     origin/master  ↑2 to push      │
╰─────────────────────────────────╯╰────────────────────────────────────────╯
[⏎] copy  [v] view  [e] edit  [r] rename  [d] delete  [␣] mark  [/] filter  [?] help
```

The top pane is a collapsible tree of the store. The bottom pane shows your
recently used entries (with a countdown while one is on the clipboard), details
of the highlighted row, the cut/copy buffer, marks, and git sync state. The
footer shows only the keys that do something right now: they change with the
highlighted row (password, file, folder), and while items are marked or cut.

| Key | Action |
|---|---|
| `j` `k` / arrows | move (`PgUp` `PgDn` `Home` `End`) |
| `l` / `→`, `h` / `←` | expand / collapse a folder, step in / jump to parent |
| `Enter` | copy the password, export a file entry, or toggle a folder |
| `H` / `Backspace` | collapse the folder the cursor is in, landing on it |
| `-` | collapse everything |
| `v` | view the entry in a popup; `space` reveals the password |
| `c`, `1`-`5` | copy the highlighted entry, or a recent one |
| `/` | fuzzy-filter the tree in place; `Esc` clears |
| `space` | mark / unmark (moves down) |
| `x` / `y` / `p` | cut / copy the marked (or highlighted) items, paste into the highlighted folder |
| `r` | rename in place (`../x` and `/abs/path` move it) |
| `d` | delete the marked (or highlighted) items, after asking |
| `u` | history: undo any past change, or roll the store back to a point |
| `e` | edit in `$VISUAL` / `$EDITOR` |
| `g` | generate a password in the highlighted folder: pick name, type (symbols, letters & digits, url-safe, hex, digits, base-64) and length; it goes straight to the clipboard and opens masked, never printed. Type and length are remembered; on an existing entry it replaces only the password line |
| `i` `m` `I` `E` | insert, mkdir, import, export — opens the command line, pre-filled, in the highlighted folder |
| `:` | command palette: every command with its usage, narrowing as you type; then entries and folders for its path. `↑↓` pick, `Tab` completes, `⏎` runs; `Ctrl-P`/`Ctrl-N` walk history |
| `Esc` | clear the filter, then marks, then the buffer |
| `R` / `?` / `q` | refresh / help / quit |

- Pasting moves or copies, re-encrypting entries that land in a folder with
  different recipients, and every change is a git commit. Overwriting an
  existing entry asks first; copying into the same folder makes `name-copy`.
- Passwords are never on screen unless you open `v` and press `space`; a
  revealed password re-masks after `PASSWORD_STORE_CLIP_TIME` (45 s), and the
  plaintext is dropped when the popup closes. The rest of the entry (`user:`,
  `url:`, notes) is shown in the popup.
- Recent entries persist in `$XDG_STATE_HOME/ezer-pass/recent` (names only).
- The first time something needs decrypting, an unlock popup asks for your
  identity's passphrase; after that the session stays unlocked until you quit.
- The TUI steps aside for `$EDITOR`, `insert` prompts and command output, then
  comes straight back; it all stays on the alternate screen, so a freshly
  generated password doesn't linger in shell scrollback.

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
passphrase for ~/.config/ezer-pass/identity.age:
copied /servers/web1 to the clipboard; it clears in 45 seconds
ezer:/servers > cd prod          # tab completion works: cd p<Tab>
ezer:/servers/prod > show db
hunter2
```

- Paths are relative to the current directory; `..`, `.` and store-absolute
  `/apple/id` work like a shell, and `..` can never escape the store.
- Typing a bare directory name (`servers`) is shorthand for `cd servers`.
- The passphrase is asked once per session, the first time something is
  decrypted.
- `Ctrl-D` or `exit` leaves; command history is kept in
  `~/.local/state/ezer-pass/history` (commands only, never secrets).

### Commands

| Command | Description |
|---|---|
| `ls [path]` | list a store directory |
| `cd [path]` / `pwd` | move around the store |
| `mkdir <path>` | create a directory (git tracks it once it holds an entry) |
| `show [-c] <entry>` | print an entry, or `-c` to copy its first line (cleared after 45 s) |
| `insert [-m] [-e] [-f] <entry>` | add an entry: asks twice without echo; `-m` several lines, `-e` echo |
| `generate [-n] [-c] [-i] [-f] [--url-safe\|--hex\|--digits\|--base-64] <entry> [len]` | generate a password; `-c` copies instead of printing, `-i` replaces only the first line |
| `edit <entry>` | edit in `$VISUAL` / `$EDITOR`, via a private file in `/dev/shm` that is wiped afterwards |
| `rm [-r] [-f] <path>` | remove an entry or directory |
| `mv [-f] <src> <dest>` / `cp [-f] <src> <dest>` | rename or copy, re-encrypting for the new folder's recipients if they differ |
| `find <terms>` | list entries whose names contain any term |
| `grep [-i] <text>` | search inside entries (ignores case unless `text` has capitals) |
| `import [--force] <file> <entry>` | store a file as a base64 entry |
| `export [-f] <entry> [dest]` | write a file entry back to disk |
| `reindex [path]` | find file entries the index doesn't know about (older stores) |
| `init [-p folder] [age1...]` | make your identity if needed; set who a folder is encrypted to |
| `migrate` | move a `pass` store from gpg to age, once |
| `git <git-args>` | run git in the store (`git init`, `git log`, `git remote add`, ...) |

Every command also works one-shot from your shell with the same syntax:
`ezer-pass show servers/web1`, `ezer-pass import cert.pem servers/cert`.

## Keys and encryption

Each entry is an age file (`name.age`) encrypted to the *recipients* (age public
keys) listed in the nearest `.age-recipients`: the one in its folder, or the
closest one above it. Your *identity*, the private key that decrypts them, lives
in `~/.config/ezer-pass/identity.age`, encrypted with your passphrase.

- `ezer-pass init` makes the identity (asking for a passphrase twice) and writes
  the store's `.age-recipients` with your public key.
- `init -p team age1… age1…` gives a folder its own recipients, for sharing
  with others, and re-encrypts what is already there. Entries moved or copied
  into it are re-encrypted for them too.
- The passphrase is asked once per TUI or explore session and the unlocked key
  is kept in memory only. A plain, unencrypted identity file (as made by
  `age-keygen`) also works and is never asked about.
- Back the identity up: without it and its passphrase nothing in the store can
  be read. To use the store on another machine, copy the identity there and
  clone the store.
- The files are standard age, so the `age` command line reads them too:
  `age -d -i ~/.config/ezer-pass/identity.age ~/.password-store/site.age`.

## Moving from pass

ezer-pass reads a store made by `pass` only to move it to age:

```
$ ezer-pass migrate
no identity yet; making one at ~/.config/ezer-pass/identity.age
passphrase for your new identity:
the same passphrase again:
your public key is age1rn2pkl4k7y6ejkqp43q8asvsssjkxlw72ej9l9xvr9aylnzv64xshgkcr3
decrypting 214 entries with gpg (it may ask for your gpg passphrase once)
moved 214 entries to age
```

It decrypts every entry with gpg one last time, encrypts it with age, removes
the `.gpg` files and `.gpg-id`, and commits the whole move as one change. If any
entry fails to decrypt, nothing is touched. Until a store is migrated, ezer-pass
refuses to work on it, other than `migrate` and `git`.

Afterwards history still shows every commit from before the move, but undoing
one of those, or rolling back past the move, is refused, since it would bring
back gpg files. On your other machines, copy the identity over and pull.

## Git sync

If the store is a git repository with a configured upstream, the TUI and the
explore session keep it synced automatically:

- **On startup** it asks `pull from origin/master? [Y/n]` and runs
  `git pull --ff-only` on yes (a diverged store is reported and left for you to
  resolve; a failed pull never blocks the session).
- **On exit** — `q` in the TUI, `exit` or `Ctrl-D` in explore mode — if you have unpushed commits, it asks
  `push 2 commits to origin/master? [Y/n]` and pushes on yes.

In the TUI, pulls and pushes run in the background behind a progress popup.
git gets no terminal there, so an ssh passphrase or host-key prompt fails
instead of drawing over the screen (an `SSH_ASKPASS` helper still works); the
popup then shows git's output and `r` retries with the terminal handed over,
where prompts work. A failed push on exit offers `q` to quit anyway (your
commits stay local) or `esc` to go back.

Stores without git (or without a remote) are left completely alone, and one-shot
commands never touch git.

## History and undo

Every change is a git commit, so `u` in the TUI opens the store's
history — from any session or machine, not just this one — with a graph of
how it branched and merged across machines:

```
╭ history ───────────────────────────────────────────────────── ↑ 1 not pushed ╮
│▌●   ↑ 3adf352 Remove personal/bank from store.                       just now│
│ ●     65660df Merge branch 'master' of github.com:me/pass         2 hours ago│
│ │╲                                                                           │
│ │ ●   b6b82d5 Add given password for laptop/key to store.         2 hours ago│
│ ● │   4001965 Rename work/a to work/b.                             3 days ago│
│ │╱                                                                           │
│ ●     8be2f54 Undo: Remove old/key from store.                     1 week ago│
╰───────────────────────────────────────────────────────────────────────── 1/5 ╯
╭ changes ──────────────────────────────────────── 3adf352 · andrew · just now ╮
│ − /personal/bank                                                             │
╰──────────────────────────────────────────────────────────── ↑ not pushed yet ╯
[j k] move  [u] undo this change  [⏎] roll back to here  [esc] back
```

- `u` on a commit undoes just that change, as a new `Undo: …` commit. The new
  commit lands at the top of the list, so `u` again redoes it.
- `⏎` rolls the whole store back to how it was at that commit — every newer
  change undone together, as one new commit — after asking.
- Nothing is rewritten: undo only adds commits, so it works after a push, and
  rolling back can itself be undone. If later changes conflict with an undo,
  or the store has uncommitted changes, nothing is touched.
- `esc` goes back to the tree, on an entry the undo brought back.

ezer-pass's own file-index commits are hidden from the list. History needs the
store in git (`:git init`).

## File entries

```
ezer:/ > import backup-codes.pdf apple/backup-codes
imported backup-codes.pdf (48213 bytes) as /apple/backup-codes
ezer:/ > show apple/backup-codes
file entry: backup-codes.pdf (48213 bytes) — use `export apple/backup-codes` to write it out
ezer:/ > export apple/backup-codes ~/Desktop/
exported /apple/backup-codes (48213 bytes) to /home/you/Desktop/backup-codes.pdf
```

A file entry is a normal entry whose first line is a marker, followed by the
base64-encoded file, so it syncs like any other entry:

```
!ezer-pass:b64:backup-codes.pdf
JVBERi0xLjcKJcTl8uXrp/Og0MTGCjQgMCBvYmoK...
```

`show` recognizes the marker and reports file metadata instead of dumping base64;
`export` decodes it, defaulting to the original filename.

Because entry contents are encrypted, ezer-pass can't tell a file from a
password without decrypting it. Instead it keeps `.files` in the store root — a
git-tracked list of file-entry paths and sizes — so the TUI and `ls` can show
files in their own color without a single decrypt. ezer-pass updates it on
`import`, `mv`, `cp`, `rm` and paste, and corrects it whenever an entry is
decrypted, so changes made with other tools heal the next time you view the
entry. It reveals only *that* an entry is a file; the names are already
unencrypted.

For an older store, with files imported before the index existed or by another
tool, run `reindex` (`:reindex` in the TUI, or `ezer-pass reindex`) once. It
decrypts every entry (or just those under `reindex <folder>`) without printing
anything, records which are files, and commits the updated index.

In the TUI, file entries are magenta with a `▪`; the info pane shows their size,
and `Enter` (or `E`) opens `:export <entry> ` — add a destination, or press
Enter again to write the original filename to the current directory. `c` won't
copy a file entry (it would only grab the marker line). Imports over 1 MiB are
refused without `--force` — a password store is a poor home for large blobs.

## Environment

| Variable | Effect |
|---|---|
| `PASSWORD_STORE_DIR` | store location (default `~/.password-store`), same as pass |
| `EZER_PASS_IDENTITY` | identity file (default `$XDG_CONFIG_HOME/ezer-pass/identity.age`) |
| `PASSWORD_STORE_CLIP_TIME` | seconds a copied password stays on the clipboard, and before a revealed one re-masks in the TUI (default 45) |
| `PASSWORD_STORE_GENERATED_LENGTH` | default length for `generate` (default 25) |
| `VISUAL` / `EDITOR` | editor for `edit` (default `vi`) |
| `XDG_STATE_HOME` | history file location (default `~/.local/state`) |
| `EZER_PASS_ICONS` | `plain` draws folders as `▶`/`▼` instead of Nerd Font folder icons, for fonts without them |

## Development

```sh
cargo test
```

To try it against a throwaway store and identity instead of your real ones:

```sh
export PASSWORD_STORE_DIR=$(mktemp -d) XDG_CONFIG_HOME=$(mktemp -d)
cargo run -- init
cargo run -- generate -n servers/web1 20
cargo run
```
