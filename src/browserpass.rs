//! The native host for the browserpass extension (Firefox, Chromium and
//! friends), so the browser can fill logins from the store. The browser
//! runs the host once per request, writes one length-prefixed JSON request
//! to its stdin and reads one reply from its stdout. browserpass's own host
//! answers by running gpg over `*.gpg` files; this one speaks the same
//! protocol (v3.1, `PROTOCOL.md` in browserpass-native) over the store's
//! age files. The browser gives the host no terminal to ask a passphrase
//! on, so a passphrase-protected identity is read through the agent.
//!
//! `ezer-pass browserpass install` registers the host with every browser
//! it finds; the browser then runs `ezer-pass browserpass host`.

use crate::agent::{self, read_frame, write_frame};
use crate::context::{self, tilde};
use crate::crypto;
use crate::git;
use crate::index::FileIndex;
use crate::store::{self, EXT, Store};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// The protocol version answered, as `MAJOR * 1000000 + MINOR * 1000 + PATCH`.
/// From 3.1.0 the extension also edits and deletes entries.
const VERSION: u32 = 3_001_000;

/// What browsers know the host as.
const HOST_NAME: &str = "com.github.browserpass.native";

const FIREFOX_EXTENSION: &str = "browserpass@maximbaz.com";

const CHROMIUM_EXTENSIONS: &[&str] = &[
    "chrome-extension://naepdomgkenhinolocfifgehidddafch/",
    "chrome-extension://pjmbgaakjkbhpopmakjoedenlfdmcdgm/",
    "chrome-extension://klfoddkbhleoaabpmiigbmpbjfljimgb/",
];

/// `ezer-pass browserpass ...`.
pub fn run(args: &[String]) -> Result<()> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args[..] {
        ["host", ..] => host(),
        ["install"] => install(false),
        ["install", "-f" | "--force"] => install(true),
        ["uninstall"] => uninstall(),
        _ => bail!("usage: ezer-pass browserpass install [--force] | uninstall"),
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Request {
    action: String,
    settings: Settings,
    store_id: String,
    file: String,
    contents: Option<Zeroizing<String>>,
    echo_response: Value,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Settings {
    /// Keyed by store id.
    stores: HashMap<String, StoreSettings>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StoreSettings {
    name: String,
    path: String,
}

/// An error reply: a code from the protocol, and parameters the extension
/// shows (`message` first among them).
struct Failure {
    code: u32,
    params: Map<String, Value>,
}

fn fail(code: u32, message: &str, request: &Request) -> Failure {
    let mut params = Map::new();
    params.insert("message".into(), message.into());
    params.insert("action".into(), request.action.as_str().into());
    Failure { code, params }
}

impl Failure {
    fn with(mut self, key: &str, value: impl std::fmt::Display) -> Self {
        self.params.insert(key.into(), value.to_string().into());
        self
    }

    fn error(self, e: &anyhow::Error) -> Self {
        self.with("error", format!("{e:#}"))
    }

    fn store(self, id: &str, settings: &StoreSettings) -> Self {
        self.with("storeId", id)
            .with("storeName", &settings.name)
            .with("storePath", &settings.path)
    }
}

/// Answer one request from the browser.
fn host() -> Result<()> {
    let request = read_frame(&mut std::io::stdin().lock());
    let reply = match request {
        Err(e) => error_reply(
            fail(
                10,
                "Unable to parse the length of the browser request",
                &Request::default(),
            )
            .error(&e),
        ),
        Ok(buf) => match serde_json::from_slice::<Request>(&buf) {
            Err(e) => error_reply(
                fail(
                    11,
                    "Unable to parse the browser request",
                    &Request::default(),
                )
                .with("error", e),
            ),
            Ok(request) => respond(&request),
        },
    };
    let reply = Zeroizing::new(serde_json::to_vec(&reply)?);
    write_frame(&mut std::io::stdout().lock(), &reply)
}

fn respond(request: &Request) -> Value {
    let result = match request.action.as_str() {
        "configure" => configure(request),
        "list" => list(request, false),
        "tree" => list(request, true),
        "fetch" => fetch(request),
        "save" => save(request),
        "delete" => delete(request),
        "echo" => return request.echo_response.clone(),
        _ => Err(fail(12, "Invalid request action", request)),
    };
    match result {
        Ok(Value::Null) => json!({ "status": "ok", "version": VERSION }),
        Ok(data) => json!({ "status": "ok", "version": VERSION, "data": data }),
        Err(failure) => error_reply(failure),
    }
}

fn error_reply(failure: Failure) -> Value {
    json!({
        "status": "error",
        "code": failure.code,
        "version": VERSION,
        "params": failure.params,
    })
}

/// A store the extension named, and the identity that reads it: that of
/// the context whose store it is, else the current context's.
struct Opened {
    store: Store,
    identity: PathBuf,
}

fn open(path: &str) -> Result<Opened> {
    if path.is_empty() {
        bail!("the store path cannot be empty");
    }
    let root =
        fs::canonicalize(context::expand(path)?).with_context(|| format!("cannot find {path}"))?;
    let store = Store::open(root)?;
    let owner = context::load()?
        .into_iter()
        .find(|c| fs::canonicalize(&c.path).is_ok_and(|p| p == store.root()));
    let identity = match owner {
        Some(owner) => owner.identity()?,
        None => context::current(None)?.identity()?,
    };
    Ok(Opened { store, identity })
}

/// The store `request.store_id` names.
fn open_requested(request: &Request) -> Result<(Opened, &StoreSettings), Failure> {
    let id = &request.store_id;
    let settings = request
        .settings
        .stores
        .get(id)
        .ok_or_else(|| fail(20, "Invalid password store ID", request).with("storeId", id))?;
    let opened = open(&settings.path).map_err(|e| {
        fail(13, "The password store is not accessible", request)
            .error(&e)
            .store(id, settings)
    })?;
    Ok((opened, settings))
}

/// The raw `.browserpass.json` in a store's root, `{}` if there is none.
fn store_settings(root: &Path) -> Result<String> {
    let text = match fs::read_to_string(root.join(".browserpass.json")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok("{}".into()),
        Err(e) => return Err(e.into()),
    };
    serde_json::from_str::<Map<String, Value>>(&text).context("not a JSON object")?;
    Ok(text)
}

/// Each named store's settings, or with none named, where the current
/// context's store is (the extension then uses it as its only store).
fn configure(request: &Request) -> Result<Value, Failure> {
    let mut stores = Map::new();
    for (id, settings) in &request.settings.stores {
        let opened = open(&settings.path).map_err(|e| {
            fail(13, "The password store is not accessible", request)
                .error(&e)
                .store(id, settings)
        })?;
        let raw = store_settings(opened.store.root()).map_err(|e| {
            fail(
                16,
                "Unable to read .browserpass.json of the password store",
                request,
            )
            .error(&e)
            .store(id, settings)
        })?;
        stores.insert(id.clone(), raw.into());
    }
    let mut default = json!({ "path": "", "settings": "" });
    if request.settings.stores.is_empty() {
        let path = context::current(None).map(|c| c.path).map_err(|e| {
            fail(
                15,
                "Unable to determine the location of the default password store",
                request,
            )
            .error(&e)
        })?;
        let shown = path.display().to_string();
        let opened = open(&shown).map_err(|e| {
            fail(14, "The default password store is not accessible", request)
                .error(&e)
                .with("storePath", &shown)
        })?;
        let root = opened.store.root();
        let raw = store_settings(root).map_err(|e| {
            fail(
                17,
                "Unable to read .browserpass.json of the default password store",
                request,
            )
            .error(&e)
            .with("storePath", root.display())
        })?;
        default = json!({ "path": root, "settings": raw });
    }
    Ok(json!({ "defaultStore": default, "storeSettings": stores }))
}

/// Every entry (`tree`: every folder) in each store, as paths relative to
/// it. Entries are listed with their `.age`, which the extension strips.
/// File entries are left out, as they hold no logins.
fn list(request: &Request, folders: bool) -> Result<Value, Failure> {
    let mut out = Map::new();
    for (id, settings) in &request.settings.stores {
        let opened = open(&settings.path).map_err(|e| {
            fail(13, "The password store is not accessible", request)
                .error(&e)
                .store(id, settings)
        })?;
        let files = FileIndex::load(opened.store.root());
        let mut paths: Vec<String> = opened
            .store
            .walk()
            .into_iter()
            .filter(|(path, is_dir)| *is_dir == folders && (folders || !files.is_file(path)))
            .map(|(path, _)| {
                if folders {
                    path
                } else {
                    format!("{path}{EXT}")
                }
            })
            .collect();
        paths.sort();
        out.insert(id.clone(), paths.into());
    }
    let key = if folders { "directories" } else { "files" };
    Ok(json!({ key: out }))
}

/// The store path of an entry the extension names by its file: `.age`
/// from `list`, or `.gpg` from an edit, which it always spells so.
/// Nothing outside the store, nor hidden, is accepted.
fn entry_path(file: &str) -> Option<String> {
    let path = file
        .strip_suffix(EXT)
        .or_else(|| file.strip_suffix(".gpg"))?
        .trim_start_matches('/');
    let ok = !path.is_empty()
        && path
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.'));
    ok.then(|| path.to_string())
}

fn requested_entry(request: &Request) -> Result<String, Failure> {
    entry_path(&request.file).ok_or_else(|| {
        fail(23, "Invalid password file extension", request).with("file", &request.file)
    })
}

/// Decrypt with `identity`: through the agent if it takes a passphrase.
fn decrypt(identity: &Path, ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if crypto::needs_passphrase(identity)? {
        agent::decrypt(identity, ciphertext)
    } else {
        crypto::decrypt(&crypto::unlock(identity, None)?, ciphertext)
    }
}

fn fetch(request: &Request) -> Result<Value, Failure> {
    let path = requested_entry(request)?;
    let (opened, settings) = open_requested(request)?;
    let contents = fs::read(opened.store.entry_file(&path))
        .map_err(anyhow::Error::from)
        .and_then(|ciphertext| decrypt(&opened.identity, &ciphertext))
        .map_err(|e| {
            fail(24, "Unable to decrypt the password file", request)
                .error(&e)
                .store(&request.store_id, settings)
                .with("file", &request.file)
        })?;
    Ok(json!({ "contents": String::from_utf8_lossy(&contents) }))
}

fn save(request: &Request) -> Result<Value, Failure> {
    let path = requested_entry(request)?;
    let contents = request
        .contents
        .as_ref()
        .ok_or_else(|| fail(27, "The entry contents is missing", request))?;
    let (opened, settings) = open_requested(request)?;
    let store = &opened.store;
    let failed = |code, message, e: anyhow::Error| {
        fail(code, message, request)
            .error(&e)
            .store(&request.store_id, settings)
            .with("file", &request.file)
    };
    store.recipients(store::parent(&path)).map_err(|e| {
        failed(
            28,
            "Unable to determine the recipients for the encryption",
            e,
        )
    })?;
    let existed = store.is_entry(&path);
    store
        .write_entry(&path, contents.as_bytes())
        .map_err(|e| failed(29, "Unable to encrypt the password file", e))?;
    let message = if existed {
        format!("Edit password for {path} using browserpass.")
    } else {
        format!("Add password for {path} using browserpass.")
    };
    commit(store, &[&format!("{path}{EXT}")], &message);
    // Whatever was there, it is text now.
    let mut files = FileIndex::load(store.root());
    files.remove(&path);
    files.save(store.root());
    Ok(Value::Null)
}

fn delete(request: &Request) -> Result<Value, Failure> {
    let path = requested_entry(request)?;
    let (opened, settings) = open_requested(request)?;
    let store = &opened.store;
    fs::remove_file(store.entry_file(&path)).map_err(|e| {
        fail(30, "Unable to delete the password file", request)
            .error(&e.into())
            .store(&request.store_id, settings)
            .with("file", &request.file)
    })?;
    store.remove_empty_dirs(store::parent(&path));
    commit(
        store,
        &[&format!("{path}{EXT}")],
        &format!("Remove {path} from store."),
    );
    let mut files = FileIndex::load(store.root());
    files.remove(&path);
    files.save(store.root());
    Ok(Value::Null)
}

/// Commit a change the extension made. The change itself is done, so a
/// failure only goes to stderr, which the browser shows in its console.
fn commit(store: &Store, paths: &[&str], message: &str) {
    if let Err(e) = git::commit(store.root(), paths, message) {
        eprintln!("ezer-pass: the change is saved, but committing it to git failed: {e:#}");
    }
}

struct Browser {
    name: &'static str,
    /// Its own folder, which says it is installed, and where it looks for
    /// native hosts, both relative to home (`config` ones to
    /// `$XDG_CONFIG_HOME`).
    dir: &'static str,
    hosts: &'static str,
    config: bool,
    firefox: bool,
}

const BROWSERS: &[Browser] = &[
    Browser {
        name: "Firefox",
        dir: ".mozilla",
        hosts: ".mozilla/native-messaging-hosts",
        config: false,
        firefox: true,
    },
    Browser {
        name: "LibreWolf",
        dir: ".librewolf",
        hosts: ".librewolf/native-messaging-hosts",
        config: false,
        firefox: true,
    },
    Browser {
        name: "Chromium",
        dir: "chromium",
        hosts: "chromium/NativeMessagingHosts",
        config: true,
        firefox: false,
    },
    Browser {
        name: "Google Chrome",
        dir: "google-chrome",
        hosts: "google-chrome/NativeMessagingHosts",
        config: true,
        firefox: false,
    },
    Browser {
        name: "Brave",
        dir: "BraveSoftware/Brave-Browser",
        hosts: "BraveSoftware/Brave-Browser/NativeMessagingHosts",
        config: true,
        firefox: false,
    },
    Browser {
        name: "Vivaldi",
        dir: "vivaldi",
        hosts: "vivaldi/NativeMessagingHosts",
        config: true,
        firefox: false,
    },
];

/// Hosts installed for every user, which a browser may pick over ours.
const SYSTEM_HOSTS: &[&str] = &[
    "/usr/lib/mozilla/native-messaging-hosts",
    "/usr/lib64/mozilla/native-messaging-hosts",
    "/etc/chromium/native-messaging-hosts",
    "/etc/opt/chrome/native-messaging-hosts",
];

impl Browser {
    fn base(&self) -> Result<PathBuf> {
        let home = context::home()?;
        Ok(match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) if self.config => PathBuf::from(dir),
            _ if self.config => home.join(".config"),
            _ => home,
        })
    }

    fn installed(&self) -> Result<bool> {
        Ok(self.base()?.join(self.dir).is_dir())
    }

    fn manifest(&self) -> Result<PathBuf> {
        Ok(self
            .base()?
            .join(self.hosts)
            .join(format!("{HOST_NAME}.json")))
    }
}

/// The script browsers run as the host. A manifest names a program but no
/// arguments, so this adds `browserpass host`.
fn wrapper() -> Result<PathBuf> {
    // Not a valid context name, so never a store.
    context::new_path("browserpass-host.sh")
}

/// The host a manifest runs, if it is a manifest.
fn manifest_host(file: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(file).ok()?;
    let manifest: Map<String, Value> = serde_json::from_str(&text).ok()?;
    manifest.get("path")?.as_str().map(PathBuf::from)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn write_new(file: &Path, data: &[u8], mode: u32) -> Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", tilde(dir)))?;
    }
    let _ = fs::remove_file(file);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(file)
        .and_then(|mut f| std::io::Write::write_all(&mut f, data))
        .with_context(|| format!("cannot write {}", tilde(file)))
}

/// `ezer-pass browserpass install`: write the host script, and a manifest
/// for it in every browser found. One that already names another host
/// (browserpass's own) is left alone unless `force`.
fn install(force: bool) -> Result<()> {
    let browsers: Vec<&Browser> = BROWSERS
        .iter()
        .filter(|b| b.installed().unwrap_or(false))
        .collect();
    if browsers.is_empty() {
        let names: Vec<&str> = BROWSERS.iter().map(|b| b.name).collect();
        bail!(
            "found no browser to install into (looked for {})",
            names.join(", ")
        );
    }
    let script = wrapper()?;
    let mut manifests = Vec::new();
    for browser in &browsers {
        let manifest = browser.manifest()?;
        match manifest_host(&manifest) {
            Some(other) if other != script && !force => bail!(
                "{} already runs another browserpass host ({}); `ezer-pass browserpass install --force` replaces it",
                browser.name,
                tilde(&other)
            ),
            _ => manifests.push((browser, manifest)),
        }
    }

    let exe = std::env::current_exe().context("cannot find ezer-pass itself")?;
    let exe = exe.to_str().context("ezer-pass's path is not UTF-8")?;
    let text = format!(
        "#!/bin/sh\n# The browserpass extension's native host, written by `ezer-pass browserpass install`.\nexec {} browserpass host \"$@\"\n",
        shell_quote(exe)
    );
    write_new(&script, text.as_bytes(), 0o755)?;
    for (browser, manifest) in manifests {
        let allowed = if browser.firefox {
            json!({ "allowed_extensions": [FIREFOX_EXTENSION] })
        } else {
            json!({ "allowed_origins": CHROMIUM_EXTENSIONS })
        };
        let mut json = json!({
            "name": HOST_NAME,
            "description": "ezer-pass, answering for browserpass",
            "path": script,
            "type": "stdio",
        });
        json.as_object_mut()
            .expect("an object")
            .extend(allowed.as_object().expect("an object").clone());
        let text = serde_json::to_string_pretty(&json)? + "\n";
        write_new(&manifest, text.as_bytes(), 0o644)?;
        println!("installed for {} ({})", browser.name, tilde(&manifest));
    }
    for dir in SYSTEM_HOSTS {
        let file = Path::new(dir).join(format!("{HOST_NAME}.json"));
        if file.exists() {
            println!(
                "note: {} installs a host for every user; uninstall browserpass-native so browsers don't run it instead",
                file.display()
            );
        }
    }
    println!("next: install the browserpass extension, and run `ezer-pass unlock` before using it");
    Ok(())
}

/// `ezer-pass browserpass uninstall`: remove the manifests naming our host
/// script, and the script.
fn uninstall() -> Result<()> {
    let script = wrapper()?;
    let mut removed = false;
    for browser in BROWSERS {
        let manifest = browser.manifest()?;
        if manifest_host(&manifest).as_deref() == Some(script.as_path()) {
            fs::remove_file(&manifest)
                .with_context(|| format!("cannot remove {}", tilde(&manifest)))?;
            println!("uninstalled from {}", browser.name);
            removed = true;
        }
    }
    if script.exists() {
        fs::remove_file(&script).with_context(|| format!("cannot remove {}", tilde(&script)))?;
        removed = true;
    }
    if !removed {
        println!("not installed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_paths() {
        assert_eq!(
            entry_path("servers/web1.age").as_deref(),
            Some("servers/web1")
        );
        assert_eq!(entry_path("/github.com.gpg").as_deref(), Some("github.com"));
        assert_eq!(entry_path("servers/web1"), None);
        assert_eq!(entry_path("../../etc/passwd.age"), None);
        assert_eq!(entry_path("a/../b.age"), None);
        assert_eq!(entry_path(".git/config.age"), None);
        assert_eq!(entry_path("a//b.age"), None);
        assert_eq!(entry_path(".age"), None);
    }

    #[test]
    fn replies() {
        let request: Request =
            serde_json::from_str(r#"{"action":"echo","echoResponse":{"a":1}}"#).unwrap();
        assert_eq!(respond(&request), json!({ "a": 1 }));
        let request: Request =
            serde_json::from_str(r#"{"action":"nope","settings":{"stores":{}}}"#).unwrap();
        let reply = respond(&request);
        assert_eq!(reply["status"], "error");
        assert_eq!(reply["code"], 12);
        assert_eq!(reply["params"]["action"], "nope");
        let request: Request = serde_json::from_str(
            r#"{"action":"fetch","storeId":"x","file":"a.age","settings":{"stores":{}}}"#,
        )
        .unwrap();
        assert_eq!(respond(&request)["code"], 20);
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("/a b/it's"), r"'/a b/it'\''s'");
    }
}
