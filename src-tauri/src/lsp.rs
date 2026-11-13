//! Language servers, the way VS Code uses them: one server per language, started when a file of
//! that language is opened, spoken to over its standard input and output (JSON-RPC with
//! Content-Length headers). A server that crashes is started again, up to 4 times; after 5
//! crashes in 3 minutes it is left stopped, as VS Code's language client does.

use crate::doc::{Diag, Doc};
use crate::lexer;
use crate::runner::{clean_env, lower_priority};
use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

const MAX_LSP_BYTES: usize = 2 << 20;
pub const TIMEOUT: Duration = Duration::from_secs(8);
const RESTARTS: usize = 4;
const RESTART_WINDOW: Duration = Duration::from_secs(180);

/// A language server Assioma knows how to start.
pub struct Server {
    pub id: &'static str,
    pub name: &'static str,
    /// The language ids (VS Code's) it serves.
    pub languages: &'static [&'static str],
    /// Commands to try, in order: the first whose program is installed is used.
    pub commands: &'static [&'static [&'static str]],
    /// How to get it, shown when none is installed.
    pub install: &'static str,
}

pub const SERVERS: &[Server] = &[
    Server { id: "clangd", name: "clangd", languages: &["c", "cpp", "objective-c", "objective-cpp"], commands: &[&["clangd", "--header-insertion=never", "--log=error"]], install: "Install clangd from your package manager (it comes with LLVM)." },
    Server { id: "rust", name: "rust-analyzer", languages: &["rust"], commands: &[&["rust-analyzer"]], install: "rustup component add rust-analyzer" },
    Server { id: "python", name: "Python", languages: &["python"], commands: &[&["basedpyright-langserver", "--stdio"], &["pyright-langserver", "--stdio"], &["pylsp"], &["jedi-language-server"]], install: "pip install basedpyright (or npm install -g pyright)" },
    Server { id: "go", name: "gopls", languages: &["go"], commands: &[&["gopls"]], install: "go install golang.org/x/tools/gopls@latest" },
    Server { id: "typescript", name: "TypeScript", languages: &["typescript", "typescriptreact", "javascript", "javascriptreact"], commands: &[&["typescript-language-server", "--stdio"]], install: "npm install -g typescript typescript-language-server" },
    Server { id: "html", name: "HTML", languages: &["html"], commands: &[&["vscode-html-language-server", "--stdio"]], install: "npm install -g vscode-langservers-extracted" },
    Server { id: "css", name: "CSS", languages: &["css", "scss", "less"], commands: &[&["vscode-css-language-server", "--stdio"]], install: "npm install -g vscode-langservers-extracted" },
    Server { id: "json", name: "JSON", languages: &["json", "jsonc"], commands: &[&["vscode-json-language-server", "--stdio"]], install: "npm install -g vscode-langservers-extracted" },
    Server { id: "yaml", name: "YAML", languages: &["yaml"], commands: &[&["yaml-language-server", "--stdio"]], install: "npm install -g yaml-language-server" },
    Server { id: "bash", name: "Bash", languages: &["shellscript"], commands: &[&["bash-language-server", "start"]], install: "npm install -g bash-language-server" },
    Server { id: "lua", name: "Lua", languages: &["lua"], commands: &[&["lua-language-server"]], install: "Install lua-language-server from your package manager." },
    Server { id: "php", name: "PHP", languages: &["php"], commands: &[&["intelephense", "--stdio"], &["phpactor", "language-server"]], install: "npm install -g intelephense" },
    Server { id: "ruby", name: "Ruby", languages: &["ruby"], commands: &[&["ruby-lsp"], &["solargraph", "stdio"]], install: "gem install ruby-lsp" },
    Server { id: "java", name: "Java", languages: &["java"], commands: &[&["jdtls"]], install: "Install jdtls (Eclipse JDT Language Server) from your package manager." },
    Server { id: "csharp", name: "C#", languages: &["csharp"], commands: &[&["csharp-ls"]], install: "dotnet tool install -g csharp-ls" },
    Server { id: "dart", name: "Dart", languages: &["dart"], commands: &[&["dart", "language-server", "--protocol=lsp"]], install: "Install the Dart SDK." },
    Server { id: "swift", name: "SourceKit-LSP", languages: &["swift"], commands: &[&["sourcekit-lsp"]], install: "Install the Swift toolchain." },
    Server { id: "kotlin", name: "Kotlin", languages: &["kotlin"], commands: &[&["kotlin-language-server"]], install: "Install kotlin-language-server." },
    Server { id: "toml", name: "Taplo", languages: &["toml"], commands: &[&["taplo", "lsp", "stdio"]], install: "cargo install taplo-cli --features lsp" },
    Server { id: "markdown", name: "Marksman", languages: &["markdown"], commands: &[&["marksman", "server"]], install: "Install marksman from your package manager." },
];

pub fn server_for(lang: &str) -> Option<&'static Server> {
    SERVERS.iter().find(|s| s.languages.contains(&lang))
}

/// What the user changed in Settings for a server.
#[derive(Deserialize, Clone, Default, PartialEq)]
pub struct Config {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// A command line of their own, instead of the ones Assioma tries.
    #[serde(default)]
    pub command: String,
}

fn yes() -> bool {
    true
}

/// The program to run, if it is installed. On Windows npm installs `.cmd` files.
pub fn resolve(program: &str) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    std::env::split_paths(&path).find_map(|d| exts.iter().map(|e| d.join(format!("{program}{e}"))).find(|f| f.is_file()))
}

fn split_command(line: &str) -> Vec<String> {
    let (mut out, mut cur, mut quote) = (vec![], String::new(), None);
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (_, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The command line a server would be started with: the user's own, or the first installed one.
pub fn command_for(server: &Server, config: &Config) -> Option<(PathBuf, Vec<String>)> {
    if !config.command.trim().is_empty() {
        let mut parts = split_command(&config.command);
        let program = parts.remove(0);
        return Some((resolve(&program).unwrap_or_else(|| PathBuf::from(program)), parts));
    }
    server.commands.iter().find_map(|c| Some((resolve(c[0])?, c[1..].iter().map(|s| s.to_string()).collect())))
}

#[derive(Serialize, Clone)]
pub struct FileEdits {
    pub path: String,
    pub edits: Vec<(usize, usize, usize, usize, String)>,
}

pub fn parse_workspace_edit(v: &Value) -> Vec<FileEdits> {
    let mut out = vec![];
    let n = |v: &Value| v.as_u64().unwrap_or(0) as usize;
    let mut add = |uri: &str, list: &Value| {
        let Some(path) = path_from_uri(uri) else {
            return;
        };
        let edits = list
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(|e| {
                let r = &e["range"];
                (n(&r["start"]["line"]), n(&r["start"]["character"]), n(&r["end"]["line"]), n(&r["end"]["character"]), e["newText"].as_str().unwrap_or("").to_string())
            })
            .collect();
        out.push(FileEdits { path: path.to_string_lossy().into_owned(), edits });
    };
    if let Some(map) = v["changes"].as_object() {
        for (uri, list) in map {
            add(uri, list);
        }
    }
    if let Some(arr) = v["documentChanges"].as_array() {
        for d in arr {
            if let Some(uri) = d["textDocument"]["uri"].as_str() {
                add(uri, &d["edits"]);
            }
        }
    }
    out
}

pub fn same_file(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
    } else {
        a == b
    }
}

pub fn uri_from_path(p: &Path) -> String {
    let mut out = String::from("file://");
    let s = p.to_string_lossy().replace('\\', "/");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b':' if cfg!(windows) => out.push(':'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let s = String::from_utf8(out).ok()?;
    if cfg!(windows) {
        let mut p = s.trim_start_matches('/').replace('/', "\\");
        if let Some(first) = p.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        Some(PathBuf::from(p))
    } else {
        Some(PathBuf::from(s))
    }
}

/// A running language server.
pub struct Lsp {
    pub server: &'static Server,
    sync_order: Mutex<()>,
    legend: Mutex<(Vec<String>, Vec<String>)>,
    caps: Mutex<Value>,
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    next: AtomicI64,
    pending: Mutex<HashMap<i64, Sender<Result<Value, String>>>>,
    alive: AtomicBool,
    stopping: AtomicBool,
    root: PathBuf,
    /// The last lines the server wrote on its error output: they explain a failed start.
    errors: Arc<Mutex<std::collections::VecDeque<String>>>,
}

/// Every language server of the project, one per kind, started on demand.
#[derive(Default)]
pub struct Servers {
    slots: Mutex<HashMap<&'static str, Arc<Mutex<Slot>>>>,
    root: Mutex<Option<PathBuf>>,
    config: Mutex<HashMap<String, Config>>,
}

#[derive(Default)]
struct Slot {
    client: Option<Arc<Lsp>>,
    crashes: Vec<Instant>,
    given_up: bool,
}

/// How the server of a language is doing, for the status bar and Settings.
#[derive(Serialize, Clone)]
pub struct Status {
    pub server: Option<String>,
    pub id: Option<String>,
    /// none, off, missing, starting, running, stopped
    pub state: String,
    pub detail: String,
}

impl Servers {
    fn slot(&self, id: &'static str) -> Arc<Mutex<Slot>> {
        self.slots.lock().unwrap().entry(id).or_default().clone()
    }

    pub fn config_of(&self, id: &str) -> Config {
        self.config.lock().unwrap().get(id).cloned().unwrap_or(Config { enabled: true, command: String::new() })
    }

    /// The folder servers are started in. A new one stops them all.
    pub fn set_root(&self, root: Option<PathBuf>) {
        let same = *self.root.lock().unwrap() == root;
        if !same {
            self.stop_all();
            *self.root.lock().unwrap() = root;
        }
    }

    pub fn set_config(&self, config: HashMap<String, Config>) {
        let old = std::mem::replace(&mut *self.config.lock().unwrap(), config);
        // Servers whose settings changed start again with the new ones, when next needed.
        for s in SERVERS {
            let new = self.config_of(s.id);
            if old.get(s.id).cloned().unwrap_or(Config { enabled: true, command: String::new() }) != new {
                self.stop(s.id);
            }
        }
    }

    pub fn stop(&self, id: &str) {
        let Some(s) = SERVERS.iter().find(|s| s.id == id) else { return };
        let slot = self.slot(s.id);
        let mut slot = slot.lock().unwrap();
        if let Some(c) = slot.client.take() {
            c.stop();
        }
        slot.crashes.clear();
        slot.given_up = false;
    }

    pub fn stop_all(&self) {
        let slots: Vec<_> = self.slots.lock().unwrap().values().cloned().collect();
        for slot in slots {
            if let Some(c) = slot.lock().unwrap().client.take() {
                c.stop();
            }
        }
    }

    /// The running server of a language, if any; it is not started for this.
    pub fn running_for(&self, lang: &str) -> Option<Arc<Lsp>> {
        let s = server_for(lang)?;
        let slot = self.slot(s.id);
        let c = slot.lock().unwrap().client.clone();
        c.filter(|c| c.alive())
    }

    pub fn status(&self, lang: Option<&str>) -> Status {
        let none = |state: &str, detail: &str| Status { server: None, id: None, state: state.into(), detail: detail.into() };
        let Some(lang) = lang else { return none("none", "") };
        let Some(s) = server_for(lang) else { return none("none", "No language server for this language") };
        let config = self.config_of(s.id);
        let mk = |state: &str, detail: String| Status { server: Some(s.name.into()), id: Some(s.id.into()), state: state.into(), detail };
        if !config.enabled {
            return mk("off", format!("{} is turned off in Settings", s.name));
        }
        let slot = self.slot(s.id);
        let slot = slot.lock().unwrap();
        if slot.given_up {
            return mk("stopped", format!("{} crashed {} times in 3 minutes and was not started again", s.name, RESTARTS + 1));
        }
        match &slot.client {
            Some(c) if c.alive() => mk("running", String::new()),
            _ if command_for(s, &config).is_none() => mk("missing", s.install.into()),
            _ => mk("idle", String::new()),
        }
    }

    /// The server for `lang`, started if needed. Errors say why there is none.
    pub fn client(&self, app: &AppHandle, lang: &str) -> Result<Arc<Lsp>, String> {
        let s = server_for(lang).ok_or("No language server for this language")?;
        let config = self.config_of(s.id);
        if !config.enabled {
            return Err(format!("{} is turned off in Settings", s.name));
        }
        // Held while starting, so that two requests at once don't start two servers.
        let slot = self.slot(s.id);
        let mut slot = slot.lock().unwrap();
        if let Some(c) = slot.client.clone().filter(|c| c.alive()) {
            return Ok(c);
        }
        if let Some(dead) = slot.client.take() {
            if !dead.stopping.load(Ordering::SeqCst) {
                let now = Instant::now();
                slot.crashes.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
                slot.crashes.push(now);
                if slot.crashes.len() > RESTARTS {
                    slot.given_up = true;
                }
            }
        }
        if slot.given_up {
            return Err(format!("{} crashed {} times in 3 minutes and was not started again", s.name, RESTARTS + 1));
        }
        let (program, args) = command_for(s, &config).ok_or_else(|| format!("{} is not installed. {}", s.name, s.install))?;
        let root = self.root.lock().unwrap().clone().unwrap_or_else(std::env::temp_dir);
        let _ = app.emit("lsp-status", json!({ "server": s.id, "state": "starting" }));
        let c = Lsp::spawn(app.clone(), s, &program, &args, &root).inspect_err(|e| {
            let _ = app.emit("lsp-status", json!({ "server": s.id, "state": "failed", "detail": e }));
        })?;
        slot.client = Some(c.clone());
        let _ = app.emit("lsp-status", json!({ "server": s.id, "state": "running" }));
        Ok(c)
    }
}

impl Lsp {
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn spawn(app: AppHandle, server: &'static Server, program: &Path, args: &[String], root: &Path) -> Result<Arc<Lsp>, String> {
        let ext = program.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let mut cmd = if cfg!(windows) && (ext == "cmd" || ext == "bat") {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(program);
            c
        } else {
            Command::new(program)
        };
        clean_env(&mut cmd);
        cmd.args(args).current_dir(root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        lower_priority(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", server.name))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let errors: Arc<Mutex<std::collections::VecDeque<String>>> = Arc::default();
        if let Some(stderr) = child.stderr.take() {
            let keep = errors.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let mut k = keep.lock().unwrap();
                    if !line.trim().is_empty() {
                        k.push_back(line.chars().take(300).collect());
                    }
                    if k.len() > 4 {
                        k.pop_front();
                    }
                }
            });
        }
        let lsp = Arc::new(Lsp {
            server,
            sync_order: Mutex::new(()),
            legend: Mutex::new((vec![], vec![])),
            caps: Mutex::new(Value::Null),
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            next: AtomicI64::new(1),
            pending: Mutex::new(HashMap::new()),
            alive: AtomicBool::new(true),
            stopping: AtomicBool::new(false),
            root: root.to_path_buf(),
            errors,
        });
        let reader = lsp.clone();
        std::thread::spawn(move || reader.read_loop(stdout, app));

        let folder = json!([{ "uri": uri_from_path(root), "name": root.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()) }]);
        let init = lsp.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "clientInfo": { "name": "Assioma", "version": env!("CARGO_PKG_VERSION") },
                "rootUri": uri_from_path(root),
                "rootPath": root.to_string_lossy(),
                "workspaceFolders": folder,
                "capabilities": {
                    "textDocument": {
                        "synchronization": { "didSave": true, "dynamicRegistration": false },
                        "completion": { "completionItem": { "snippetSupport": true, "insertReplaceSupport": false }, "contextSupport": true },
                        "publishDiagnostics": { "relatedInformation": false },
                        "hover": { "contentFormat": ["plaintext", "markdown"] },
                        "definition": { "linkSupport": true },
                        "references": {},
                        "rename": { "prepareSupport": false },
                        "formatting": {},
                        "semanticTokens": { "requests": { "full": true }, "tokenTypes": [], "tokenModifiers": [], "formats": ["relative"] },
                        "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                        "signatureHelp": { "signatureInformation": { "parameterInformation": { "labelOffsetSupport": true } } },
                        "codeAction": { "codeActionLiteralSupport": { "codeActionKind": { "valueSet": ["", "quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite", "source", "source.organizeImports"] } } },
                    },
                    "workspace": { "applyEdit": true, "configuration": true, "workspaceFolders": true, "workspaceEdit": { "documentChanges": true } },
                    "window": { "workDoneProgress": true, "showMessage": {} },
                    "general": { "positionEncodings": ["utf-16"] },
                },
            }),
            Duration::from_secs(30),
        )
        .map_err(|e| {
            lsp.stop();
            // What the server said before it gave up says more than "it did not answer".
            std::thread::sleep(Duration::from_millis(100));
            let said: Vec<String> = lsp.errors.lock().unwrap().iter().cloned().collect();
            if said.is_empty() { e } else { format!("{} could not start: {}", server.name, said.join(" ")) }
        })?;
        let strings = |v: &Value| -> Vec<String> { v.as_array().map_or(vec![], |a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()) };
        let legend = &init["capabilities"]["semanticTokensProvider"]["legend"];
        *lsp.legend.lock().unwrap() = (strings(&legend["tokenTypes"]), strings(&legend["tokenModifiers"]));
        *lsp.caps.lock().unwrap() = init["capabilities"].clone();
        lsp.notify("initialized", json!({}))?;
        Ok(lsp)
    }

    /// Whether the server says it can do `what` (a capability like "renameProvider").
    pub fn can(&self, what: &str) -> bool {
        let caps = self.caps.lock().unwrap();
        !matches!(caps[what], Value::Null | Value::Bool(false))
    }

    fn write(&self, v: &Value) -> Result<(), String> {
        let body = v.to_string();
        let mut w = self.stdin.lock().unwrap();
        write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body).map_err(|e| format!("{}: {e}", self.server.name))?;
        w.flush().map_err(|e| e.to_string())
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.write(&json!({"jsonrpc":"2.0","method":method,"params":params}))
    }

    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        if !self.alive() {
            return Err(format!("{} stopped", self.server.name));
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        let res = rx.recv_timeout(timeout).map_err(|_| format!("{} did not answer in time", self.server.name));
        self.pending.lock().unwrap().remove(&id);
        if res.is_err() {
            // Tell the server to drop it, as VS Code does when it gives up on a request.
            let _ = self.notify("$/cancelRequest", json!({ "id": id }));
        }
        res?
    }

    fn read_loop(self: Arc<Self>, stdout: std::process::ChildStdout, app: AppHandle) {
        let mut r = BufReader::with_capacity(64 * 1024, stdout);
        let mut header = String::new();
        loop {
            let mut len = 0usize;
            loop {
                header.clear();
                match r.read_line(&mut header) {
                    Ok(0) | Err(_) => {
                        self.alive.store(false, Ordering::SeqCst);
                        self.pending.lock().unwrap().clear();
                        self.forget_documents(&app);
                        let crashed = !self.stopping.load(Ordering::SeqCst);
                        let _ = app.emit("lsp-status", json!({ "server": self.server.id, "state": if crashed { "crashed" } else { "stopped" } }));
                        return;
                    }
                    Ok(_) => {}
                }
                let line = header.trim();
                if line.is_empty() {
                    break;
                }
                if let Some(v) = line.strip_prefix("Content-Length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; len];
            if r.read_exact(&mut body).is_err() {
                continue;
            }
            let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
                continue;
            };
            self.dispatch(msg, &app);
        }
    }

    /// The documents it had open must be opened again by the next server.
    fn forget_documents(&self, app: &AppHandle) {
        let state = app.state::<AppState>();
        for d in state.docs.lock().unwrap().values_mut() {
            if d.lang.lsp_id().is_some_and(|l| self.server.languages.contains(&l)) {
                d.lsp_rev = None;
                d.diags.clear();
            }
        }
    }

    fn answer(&self, id: &Value, result: Value) {
        let _ = self.write(&json!({"jsonrpc":"2.0","id":id,"result":result}));
    }

    fn dispatch(&self, msg: Value, app: &AppHandle) {
        let params = &msg["params"];
        match (msg.get("id"), msg.get("method").and_then(Value::as_str)) {
            // Requests from the server. Left unanswered, some servers wait for ever.
            (Some(id), Some(method)) => match method {
                "workspace/applyEdit" => {
                    let _ = app.emit("apply-edit", parse_workspace_edit(&params["edit"]));
                    self.answer(id, json!({"applied": true}));
                }
                // No settings of our own: one null per item asks the server to use its defaults.
                "workspace/configuration" => {
                    let n = params["items"].as_array().map_or(0, Vec::len);
                    self.answer(id, Value::Array(vec![Value::Null; n]));
                }
                "workspace/workspaceFolders" => {
                    let name = self.root.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
                    self.answer(id, json!([{ "uri": uri_from_path(&self.root), "name": name }]));
                }
                "client/registerCapability" | "client/unregisterCapability" | "window/workDoneProgress/create" | "window/showMessageRequest" | "workspace/semanticTokens/refresh" | "workspace/diagnostic/refresh" | "workspace/inlayHint/refresh" | "workspace/codeLens/refresh" => {
                    self.answer(id, Value::Null);
                }
                _ => {
                    let _ = self.write(&json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}));
                }
            },
            (Some(id), None) => {
                let Some(id) = id.as_i64() else { return };
                if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(match msg.get("error") {
                        Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("language server error").to_string()),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    });
                }
            }
            (None, Some("textDocument/publishDiagnostics")) => {
                let Some(path) = params["uri"].as_str().and_then(path_from_uri) else {
                    return;
                };
                let state = app.state::<AppState>();
                let mut docs = state.docs.lock().unwrap();
                let Some((&id, doc)) = docs.iter_mut().find(|(_, d)| same_file(&d.path, &path)) else {
                    return;
                };
                doc.diags = parse_diags(doc, params["diagnostics"].as_array().map_or(&[], Vec::as_slice));
                let _ = app.emit("diag", id);
            }
            // Work the server reports (indexing, loading the project), for the status bar.
            (None, Some("$/progress")) => {
                let v = &params["value"];
                let text = match v["kind"].as_str() {
                    Some("end") => String::new(),
                    _ => [v["title"].as_str(), v["message"].as_str()].into_iter().flatten().collect::<Vec<_>>().join(" "),
                };
                let pct = v["percentage"].as_u64();
                let _ = app.emit("lsp-progress", json!({ "server": self.server.id, "token": params["token"], "text": text, "percent": pct }));
            }
            // Errors and warnings the server wants the user to see.
            (None, Some("window/showMessage")) if params["type"].as_u64().is_some_and(|t| t <= 2) => {
                let _ = app.emit("lsp-message", format!("{}: {}", self.server.name, params["message"].as_str().unwrap_or("")));
            }
            _ => {}
        }
    }

    pub fn sync(&self, state: &AppState, id: u32) -> Result<(), String> {
        let _order = self.sync_order.lock().unwrap();
        let (uri, lang, version, first, rope) = {
            let mut docs = state.docs.lock().unwrap();
            let doc = docs.get_mut(&id).ok_or("Document is closed")?;
            let Some(lang) = doc.lang.lsp_id() else {
                return Ok(());
            };
            if doc.rope.len_bytes() > MAX_LSP_BYTES {
                return Err(format!("This file is too large for {} (over 2 MB)", self.server.name));
            }
            if doc.lsp_rev == Some(doc.rev) {
                return Ok(());
            }
            let first = doc.lsp_rev.is_none();
            doc.lsp_ver += 1;
            doc.lsp_rev = Some(doc.rev);
            (uri_from_path(&doc.path), lang, doc.lsp_ver, first, doc.rope.clone())
        };
        let text = rope.to_string();
        // The whole text each time: every server takes it, and a change without a range means just that.
        if first {
            self.notify("textDocument/didOpen", json!({"textDocument":{"uri":uri,"languageId":lang,"version":version,"text":text}}))
        } else {
            self.notify("textDocument/didChange", json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":text}]}))
        }
    }

    pub fn saved(&self, doc: &Doc) {
        if doc.lsp_rev.is_some() {
            let _ = self.notify("textDocument/didSave", json!({"textDocument":{"uri":uri_from_path(&doc.path)}}));
        }
    }

    pub fn decode_semantic(&self, data: &[Value]) -> Vec<(usize, usize, usize, u8)> {
        let raw: Vec<u64> = data.iter().map(|v| v.as_u64().unwrap_or(0)).collect();
        let (types, mods) = &*self.legend.lock().unwrap();
        decode_tokens(&raw, types, mods)
    }

    pub fn close(&self, doc: &Doc) {
        if doc.lsp_rev.is_some() {
            let _ = self.notify("textDocument/didClose", json!({"textDocument":{"uri":uri_from_path(&doc.path)}}));
        }
    }

    /// Asks the server to end (VS Code allows it 2 seconds), then makes sure it did.
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        if self.alive() {
            let _ = self.request("shutdown", Value::Null, Duration::from_secs(2));
            let _ = self.notify("exit", Value::Null);
        }
        let mut child = self.child.lock().unwrap();
        for _ in 0..20 {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn tag_for(kind: &str, global: bool) -> u8 {
    match kind {
        "function" | "method" => lexer::FUNCTION,
        "class" | "interface" | "enum" | "type" | "typeParameter" | "concept" | "struct" => lexer::USER_TYPE,
        "property" | "enumMember" => lexer::MEMBER,
        "namespace" => lexer::NAMESPACE,
        "macro" => lexer::MACRO,
        "variable" if global => lexer::MEMBER,
        "variable" | "parameter" => lexer::VARIABLE,
        "comment" => lexer::COMMENT,
        _ => 0,
    }
}

fn decode_tokens(data: &[u64], types: &[String], mods: &[String]) -> Vec<(usize, usize, usize, u8)> {
    let global_bits = mods.iter().enumerate().filter(|(_, m)| *m == "globalScope" || *m == "fileScope").fold(0u64, |b, (i, _)| b | (1 << i));
    let (mut line, mut col) = (0usize, 0usize);
    let mut out = Vec::with_capacity(data.len() / 5);
    for t in data.as_chunks::<5>().0 {
        if t[0] > 0 {
            line += t[0] as usize;
            col = t[1] as usize;
        } else {
            col += t[1] as usize;
        }
        let kind = types.get(t[3] as usize).map_or("", String::as_str);
        out.push((line, col, t[2] as usize, tag_for(kind, t[4] & global_bits != 0)));
    }
    out
}

pub fn parse_diags(doc: &Doc, list: &[Value]) -> Vec<Diag> {
    let n = |v: &Value| v.as_u64().unwrap_or(0) as usize;
    list.iter()
        .map(|d| {
            let (sl, el) = (n(&d["range"]["start"]["line"]), n(&d["range"]["end"]["line"]));
            Diag {
                sl,
                sc: doc.col_from_utf16(sl, n(&d["range"]["start"]["character"])),
                el,
                ec: doc.col_from_utf16(el, n(&d["range"]["end"]["character"])),
                sev: d["severity"].as_u64().unwrap_or(1) as usize,
                msg: d["message"].as_str().unwrap_or("").to_string(),
                raw: d.clone(),
            }
        })
        .collect()
}

/// Hover text as plain text: markdown code fences and the most common marks are taken out.
pub fn hover_text(contents: &Value) -> String {
    let piece = |v: &Value| -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Object(o) => o.get("value").and_then(Value::as_str).unwrap_or("").to_string(),
            _ => String::new(),
        }
    };
    let raw = match contents {
        Value::Array(a) => a.iter().map(piece).filter(|s| !s.trim().is_empty()).collect::<Vec<_>>().join("\n\n"),
        v => piece(v),
    };
    raw.lines().filter(|l| !l.trim_start().starts_with("```")).map(|l| l.replace("**", "").replace("\\_", "_")).collect::<Vec<_>>().join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_tokens_decode_relative_positions() {
        let types = vec!["variable".to_string(), "class".to_string()];
        let mods = vec!["declaration".to_string(), "globalScope".to_string()];
        let data = [1, 4, 3, 1, 1, 0, 6, 2, 0, 2, 2, 0, 1, 0, 0];
        assert_eq!(decode_tokens(&data, &types, &mods), vec![(1, 4, 3, lexer::USER_TYPE), (1, 10, 2, lexer::MEMBER), (3, 0, 1, lexer::VARIABLE)]);
    }

    #[test]
    fn uris_round_trip_with_spaces_and_accents() {
        let (p, want) = if cfg!(windows) {
            (PathBuf::from(r"C:\Users\user\Café C++\a b.cpp"), "file:///C:/Users/user/Caf%C3%A9%20C%2B%2B/a%20b.cpp")
        } else {
            (PathBuf::from("/home/user/Café C++/a b.cpp"), "file:///home/user/Caf%C3%A9%20C%2B%2B/a%20b.cpp")
        };
        let uri = uri_from_path(&p);
        assert_eq!(uri, want);
        assert_eq!(path_from_uri(&uri), Some(p));
    }

    #[test]
    fn every_language_has_at_most_one_server() {
        let mut seen = std::collections::HashSet::new();
        for s in SERVERS {
            for l in s.languages {
                assert!(seen.insert(*l), "{l} is served twice");
            }
        }
    }

    #[test]
    fn a_command_of_ones_own_is_split_like_a_shell_would() {
        assert_eq!(split_command(r#"pyright-langserver --stdio"#), ["pyright-langserver", "--stdio"]);
        assert_eq!(split_command(r#""/opt/my server/ls" --flag 'a b'"#), ["/opt/my server/ls", "--flag", "a b"]);
    }

    #[test]
    fn hover_markdown_becomes_plain_text() {
        let v = json!({ "kind": "markdown", "value": "```rust\nfn main()\n```\n---\n**Runs** the\\_program" });
        assert_eq!(hover_text(&v), "fn main()\n---\nRuns the_program");
        assert_eq!(hover_text(&json!(["a", { "language": "c", "value": "int x" }])), "a\n\nint x");
    }
}
