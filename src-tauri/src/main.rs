#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
#[cfg(any(windows, test))]
mod cli;
mod discord;
mod doc;
#[cfg(test)]
mod fuzz;
mod ext;
mod git;
mod bundle;
mod grammars;
mod langs;
mod lexer;
mod llama;
mod lsp;
mod project;
mod runner;
mod suggest;
mod tm;
mod term;
mod tools;
mod update;
mod words;

use doc::{Cmd, Doc, View};
use runner::Runner;
use lsp::{parse_workspace_edit, path_from_uri, uri_from_path, FileEdits, Lsp};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_clipboard_manager::ClipboardExt;

const MAX_FILE: u64 = 64 << 20;
const RUN_TERM: u32 = 0;

pub struct AppState {
    pub docs: Mutex<HashMap<u32, Doc>>,
    next_doc: AtomicU32,
    runner: Arc<Runner>,
    terms: Mutex<HashMap<u32, Arc<term::Term>>>,
    next_term: AtomicU32,
    lsp: lsp::Servers,
}

type Res<T> = Result<T, String>;

#[tauri::command]
fn app_ready(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        if let Ok(icon) = tauri::image::Image::from_bytes(include_bytes!("../icons/128x128@2x.png"))
        {
            let _ = w.set_icon(icon);
        }
        let _ = w.show();
        #[cfg(feature = "selftest")]
        if let Some(code) =
            std::env::var_os("ASSIOMA_SELFTEST").and_then(|p| fs::read_to_string(p).ok())
        {
            let _ = w.eval(code);
        }
    }
}

#[cfg(feature = "selftest")]
#[tauri::command]
fn selftest_log(msg: String) {
    println!("{msg}");
}

#[derive(Serialize)]
struct StartTarget {
    root: String,
    file: Option<String>,
}

#[tauri::command]
fn initial_path() -> Option<StartTarget> {
    let arg = std::env::args().nth(1)?;
    let path = match std::env::var_os("OWD") {
        Some(dir) if Path::new(&arg).is_relative() => Path::new(&dir).join(&arg),
        _ => Path::new(&arg).to_path_buf(),
    };
    let p = canon(&path.to_string_lossy()).ok()?;
    let text = |p: &Path| p.to_string_lossy().into_owned();
    if p.is_dir() {
        Some(StartTarget {
            root: text(&p),
            file: None,
        })
    } else {
        Some(StartTarget {
            root: text(p.parent()?),
            file: Some(text(&p)),
        })
    }
}

#[tauri::command(async)]
fn clipboard_read(app: AppHandle) -> Res<String> {
    app.clipboard().read_text().map_err(|e| e.to_string())
}

/// Copies what Ctrl+C would, straight from the document: used when it is too big to send ahead.
#[tauri::command(async)]
fn doc_copy(id: u32, app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    let text = state.docs.lock().unwrap().get(&id).ok_or("Document is closed")?.clip_text();
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn clipboard_write(text: String, app: AppHandle) -> Res<()> {
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}

#[cfg(unix)]
#[tauri::command(async)]
fn install_cli() -> Res<String> {
    use std::os::unix::fs::PermissionsExt;
    let exe = std::env::var("APPIMAGE")
        .or_else(|_| std::env::current_exe().map(|p| p.to_string_lossy().into_owned()))
        .map_err(|e| e.to_string())?;
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    let bin = Path::new(&home).join(".local/bin");
    fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
    let script = format!("#!/bin/sh\nnohup \"{exe}\" \"$@\" >/dev/null 2>&1 </dev/null &\n");
    let mut installed = vec![];
    for name in ["assioma"] {
        let target = bin.join(name);
        let meta = fs::symlink_metadata(&target);
        let ours = meta.as_ref().is_ok_and(|m| m.file_type().is_symlink())
            || fs::read_to_string(&target).is_ok_and(|s| s.contains("assioma"));
        if meta.is_ok() && !ours {
            continue;
        }
        if meta.is_ok() {
            fs::remove_file(&target).map_err(|e| e.to_string())?;
        }
        fs::write(&target, &script).map_err(|e| e.to_string())?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        installed.push(name);
    }
    Ok(installed.join(" and "))
}

#[cfg(windows)]
#[tauri::command(async)]
fn install_cli() -> Res<String> {
    cli::install()
}

fn stop_children(state: &AppState) {
    state.runner.kill();
    state.lsp.stop_all();
    llama::stop();
    let terms: Vec<_> = state.terms.lock().unwrap().drain().collect();
    for (id, t) in terms {
        if id == RUN_TERM {
            t.stop();
        } else {
            t.kill();
        }
    }
}

#[tauri::command]
fn app_quit(app: AppHandle, state: State<AppState>) {
    stop_children(&state);
    app.exit(0);
}

#[tauri::command]
fn discord_presence(activity: Option<discord::Activity>) {
    discord::set(activity);
}

#[tauri::command]
fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

#[tauri::command]
async fn update_check(app: AppHandle) -> Res<Option<update::UpdateInfo>> {
    update::check(&app).await
}

#[tauri::command]
async fn update_install(app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    update::install(&app).await?;
    stop_children(&state);
    app.restart()
}

#[tauri::command(async)]
fn tools_status(app: AppHandle) -> Vec<tools::Status> {
    tools::status(&app)
}

#[tauri::command(async)]
fn ai_builtin_installed(size: String, app: AppHandle) -> bool {
    llama::installed(&app, &size)
}

#[tauri::command(async)]
async fn ai_warm(size: String, app: AppHandle) -> Res<()> {
    llama::ensure(&app, 0, &size).await.map(|_| ())
}

#[tauri::command(async)]
fn ai_remove(size: String, app: AppHandle) {
    llama::remove(&app, &size);
}

#[tauri::command]
async fn tools_install(ids: Vec<String>, app: AppHandle) -> Res<Vec<tools::Failure>> {
    tools::install(&app, &ids).await
}

#[derive(Serialize)]
struct Opened {
    id: u32,
    path: String,
    name: String,
}

fn read_text(path: &Path) -> Res<String> {
    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_FILE {
        return Err("File is larger than 64 MB".into());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return Err("This looks like a binary file".into());
    }
    String::from_utf8(bytes).map_err(|_| "Only UTF-8 files can be opened".to_string())
}

fn canon(path: &str) -> std::io::Result<std::path::PathBuf> {
    let p = fs::canonicalize(path)?;
    #[cfg(windows)]
    if let Some(short) = p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        return Ok(std::path::PathBuf::from(short));
    }
    Ok(p)
}

fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[tauri::command(async)]
fn doc_open(path: String, state: State<AppState>) -> Res<Opened> {
    let canon = canon(&path).map_err(|e| format!("{path}: {e}"))?;
    let shown = canon.to_string_lossy().into_owned();
    let name = canon
        .file_name()
        .map_or_else(|| shown.clone(), |n| n.to_string_lossy().into_owned());
    let known = |docs: &HashMap<u32, Doc>| docs.iter().find(|(_, d)| d.path == canon).map(|(id, _)| *id);
    if let Some(id) = known(&state.docs.lock().unwrap()) {
        return Ok(Opened { id, path: shown, name });
    }
    // The file is read without holding the lock, so a big file doesn't stop the other documents.
    // The time is taken first: a change made while reading is then noticed by the next refresh.
    let m = mtime(&canon);
    let text = read_text(&canon)?;
    let mut docs = state.docs.lock().unwrap();
    if let Some(id) = known(&docs) {
        return Ok(Opened { id, path: shown, name });
    }
    let id = state.next_doc.fetch_add(1, Ordering::Relaxed);
    docs.insert(id, Doc::new(canon, &text, m));
    Ok(Opened {
        id,
        path: shown,
        name,
    })
}

#[tauri::command]
fn doc_close(id: u32, state: State<AppState>) {
    let doc = state.docs.lock().unwrap().remove(&id);
    if let Some(doc) = doc {
        if let Some(l) = doc.lang.lsp_id().and_then(|lang| state.lsp.running_for(lang)) {
            l.close(&doc);
        }
    }
}

/// Off the window's thread, which also puts the frames on screen: a slow view (a grammar
/// coloring lines) must not hold them up. Order is kept by the editor, which sends one request
/// at a time and waits for its answer.
#[tauri::command(async)]
fn doc_apply(
    id: u32,
    cmds: Vec<Cmd>,
    top: usize,
    rows: usize,
    reveal: bool,
    state: State<AppState>,
) -> Res<View> {
    let mut docs = state.docs.lock().unwrap();
    let doc = docs.get_mut(&id).ok_or("Document is closed")?;
    for c in cmds {
        doc.run(c)?;
    }
    Ok(doc.view(top, rows, reveal))
}

/// The start of the error for a read-only file: the window then offers to save it anyway.
const READ_ONLY: &str = "is read-only";

/// Writes a whole file. Usually the text goes to a file next to it, which then takes its
/// place, so a crash or a full disk never leaves half of it. Where that would lose something,
/// the file is written in place instead, which is what VS Code does for every save.
/// With `unlock`, a read-only file is made writable first (VS Code's "Overwrite").
fn write_safely(path: &Path, text: &str, app: &str, unlock: bool) -> Res<()> {
    let mut meta = fs::metadata(path).ok();
    if let Some(m) = meta.as_mut().filter(|m| m.permissions().readonly()) {
        // Replacing the file would get around its permissions (only the folder's count).
        if !unlock {
            let name = path.file_name().map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy());
            return Err(format!("{name} {READ_ONLY}, so the changes were not saved"));
        }
        let mut p = m.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            p.set_mode(p.mode() | 0o200);
        }
        #[cfg(not(unix))]
        #[allow(clippy::permissions_set_readonly_false)]
        p.set_readonly(false);
        fs::set_permissions(path, p).map_err(|e| format!("{}: {e}", path.display()))?;
        *m = fs::metadata(path).map_err(|e| e.to_string())?;
    }
    if meta.as_ref().is_some_and(must_write_in_place) {
        return fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()));
    }
    let tmp = path.with_extension(format!("{app}-{}.tmp", std::process::id()));
    match fs::write(&tmp, text) {
        Ok(()) => {}
        // A folder we may not create files in can still hold a file we may change: write it in
        // place. Only for this error: after a full disk, writing in place could cut the file.
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return fs::write(path, text).map_err(|_| format!("{}: {e}", path.display()));
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(format!("{}: {e}", path.display()));
        }
    }
    if let Some(meta) = &meta {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            // On Windows a file another program holds open can't be replaced, but it can still be
            // written to.
            if cfg!(windows) && e.kind() == std::io::ErrorKind::PermissionDenied {
                return fs::write(path, text).map_err(|_| format!("{}: {e}", path.display()));
            }
            Err(format!("{}: {e}", path.display()))
        }
    }
}

/// Replacing a file makes a new one: the other names of a hard-linked file would keep the old
/// text, and a file of another user (one we may write to) would become ours.
#[cfg(unix)]
fn must_write_in_place(m: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    m.nlink() > 1 || m.uid() != unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn must_write_in_place(_: &fs::Metadata) -> bool {
    false
}

/// Saves go one at a time, so two of them never write the same temporary file.
static SAVING: Mutex<()> = Mutex::new(());

#[tauri::command(async)]
fn doc_save(id: u32, force: bool, unlock: Option<bool>, state: State<AppState>) -> Res<bool> {
    let _one = SAVING.lock().unwrap_or_else(|e| e.into_inner());
    // Only a copy of the text is taken under the lock: the disk is written to after letting go,
    // so typing and the other documents aren't held up. What was typed meanwhile stays unsaved.
    let (path, expected, text, rev) = {
        let docs = state.docs.lock().unwrap();
        let doc = docs.get(&id).ok_or("Document is closed")?;
        (doc.path.clone(), doc.mtime, doc.contents_for_disk(), doc.rev)
    };
    if !force && expected.is_some() && mtime(&path) != expected {
        return Ok(false);
    }
    write_safely(&path, &text, "assioma", unlock.unwrap_or(false))?;
    let lang = state.docs.lock().unwrap().get(&id).and_then(|d| d.lang.lsp_id());
    if let Some(l) = lang.and_then(|lang| state.lsp.running_for(lang)) {
        if let Some(d) = state.docs.lock().unwrap().get(&id) {
            l.saved(d);
        }
    }
    let now = mtime(&path);
    if let Some(doc) = state.docs.lock().unwrap().get_mut(&id) {
        doc.mtime = now;
        doc.saved_rev = doc.saved_rev.max(rev);
    }
    Ok(true)
}

#[tauri::command]
fn doc_search(id: u32, q: String, case: bool, state: State<AppState>) -> Res<usize> {
    let mut docs = state.docs.lock().unwrap();
    Ok(docs
        .get_mut(&id)
        .ok_or("Document is closed")?
        .set_search(&q, case))
}

#[tauri::command(async)]
fn docs_refresh(state: State<AppState>) -> Vec<u32> {
    let open: Vec<_> = {
        let docs = state.docs.lock().unwrap();
        docs.iter().filter(|(_, d)| !d.is_dirty()).map(|(id, d)| (*id, d.path.clone(), d.mtime)).collect()
    };
    // The files are looked at and read without the lock; it is taken again only to apply them.
    let fresh: Vec<_> = open
        .into_iter()
        .filter_map(|(id, path, old)| {
            let now = mtime(&path).filter(|n| Some(*n) != old)?;
            Some((id, read_text(&path).ok()?, now, old))
        })
        .collect();
    let mut docs = state.docs.lock().unwrap();
    let mut changed = vec![];
    for (id, text, now, old) in fresh {
        if let Some(d) = docs.get_mut(&id).filter(|d| !d.is_dirty() && d.mtime == old) {
            d.reload(&text, Some(now));
            changed.push(id);
        }
    }
    changed
}

#[tauri::command(async)]
fn list_dir(path: String, hidden: bool) -> Res<Vec<project::Entry>> {
    project::list_dir(&path, hidden)
}

#[tauri::command(async)]
fn list_files(root: String) -> Vec<String> {
    project::list_files(&root)
}

#[tauri::command(async)]
fn grep(root: String, query: String) -> Vec<project::Hit> {
    project::grep(&root, &query)
}

/// Replaces text in the files of the project. Files with unsaved changes in the editor are left alone.
#[tauri::command(async)]
fn replace_in_project(root: String, query: String, with: String, state: State<AppState>) -> Res<(usize, usize)> {
    let skip: Vec<std::path::PathBuf> = state.docs.lock().unwrap().values().filter(|d| d.is_dirty()).map(|d| d.path.clone()).collect();
    project::replace_all(&root, &query, &with, &skip)
}

#[tauri::command(async)]
fn fs_new_file(path: String) -> Res<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map(|_| ())
        .map_err(|e| format!("{path}: {e}"))
}

#[tauri::command(async)]
fn fs_new_dir(path: String) -> Res<()> {
    fs::create_dir(&path).map_err(|e| format!("{path}: {e}"))
}

/// Renames a file or a folder. Documents open from inside it follow it, unsaved changes and
/// all, so a later save doesn't go to the old place. Returns them with their new paths.
#[tauri::command(async)]
fn fs_rename(from: String, to: String, state: State<AppState>) -> Res<Vec<Opened>> {
    if Path::new(&to).exists() {
        return Err(format!("{to} already exists"));
    }
    let old = canon(&from).ok();
    fs::rename(&from, &to).map_err(|e| e.to_string())?;
    let (Some(old), Ok(new)) = (old, canon(&to)) else {
        return Ok(vec![]);
    };
    let mut docs = state.docs.lock().unwrap();
    let mut moved = vec![];
    for (id, d) in docs.iter_mut() {
        let Ok(rest) = d.path.strip_prefix(&old) else { continue };
        let path = if rest.as_os_str().is_empty() { new.clone() } else { new.join(rest) };
        // Under its new name it is another file for the language server.
        if let Some(l) = d.lang.lsp_id().and_then(|lang| state.lsp.running_for(lang)) {
            l.close(d);
        }
        d.moved_to(path);
        let shown = d.path.to_string_lossy().into_owned();
        let name = d.path.file_name().map_or_else(|| shown.clone(), |n| n.to_string_lossy().into_owned());
        moved.push(Opened { id: *id, path: shown, name });
    }
    Ok(moved)
}

#[tauri::command(async)]
fn fs_delete(path: String) -> Res<()> {
    let p = Path::new(&path);
    if p.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
    .map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn path_exists(path: String) -> bool {
    Path::new(&path).exists()
}

#[tauri::command(async)]
fn sys_info() -> project::Sys {
    project::sys_info()
}

#[tauri::command(async)]
fn task_start(
    program: String,
    args: Vec<String>,
    cwd: String,
    env: Vec<(String, String)>,
    background: bool,
    app: AppHandle,
    state: State<AppState>,
) -> Res<()> {
    state
        .runner
        .start(app, &program, &args, &cwd, &env, background)
}

#[tauri::command(async)]
fn task_stdin(text: String, state: State<AppState>) -> Res<()> {
    state.runner.write_stdin(&text)
}

#[tauri::command(async)]
fn task_kill(state: State<AppState>) {
    state.runner.kill();
    let run = state.terms.lock().unwrap().get(&RUN_TERM).cloned();
    if let Some(t) = run {
        t.stop();
    }
}

#[allow(clippy::too_many_arguments)]
#[tauri::command(async)]
fn run_start(
    program: String,
    args: Vec<String>,
    cwd: String,
    env: Vec<(String, String)>,
    cols: usize,
    rows: usize,
    banner: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Res<()> {
    #[cfg(feature = "selftest")]
    if std::env::var_os("ASSIOMA_NO_PTY").is_some() {
        return Err("disabled by the self test".into());
    }
    // Held across the spawn so a program that exits at once is already registered.
    let mut terms = state.terms.lock().unwrap();
    if let Some(old) = terms.remove(&RUN_TERM) {
        old.stop();
    }
    let launch = term::Launch {
        program,
        args,
        cwd,
        env,
    };
    let t = term::Term::run(app, RUN_TERM, launch, cols.max(2), rows.max(2), &banner)?;
    terms.insert(RUN_TERM, t);
    Ok(())
}

#[tauri::command]
fn term_note(id: u32, text: String, state: State<AppState>) -> Res<()> {
    let t = state
        .terms
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or("Terminal is closed")?;
    t.note(&text);
    Ok(())
}

#[tauri::command(async)]
fn make_dirs(path: String) -> Res<()> {
    fs::create_dir_all(&path).map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct Completion {
    label: String,
    insert: String,
    detail: String,
    /// What the typed text is matched against, when it is not `insert` (snippets).
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    /// The tab stops of a snippet, as ranges of `insert`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stops: Vec<[usize; 2]>,
}

#[derive(Serialize)]
struct Completions {
    prefix: String,
    incomplete: bool,
    items: Vec<Completion>,
}

// Language servers. Each document goes to the server of its language, started when first needed.

fn lsp_of(app: &AppHandle, state: &AppState, id: u32) -> Res<Arc<Lsp>> {
    let lang = {
        let docs = state.docs.lock().unwrap();
        let doc = docs.get(&id).ok_or("Document is closed")?;
        doc.lang.lsp_id().ok_or("No language server for this file")?
    };
    state.lsp.client(app, lang)
}

fn at_cursor(id: u32, lsp: &Lsp, state: &AppState) -> Res<(Value, String, String)> {
    lsp.sync(state, id)?;
    let docs = state.docs.lock().unwrap();
    let doc = docs.get(&id).ok_or("Document is closed")?;
    let (line, col) = doc.cursor_pos();
    let params = json!({
        "textDocument": {"uri": uri_from_path(&doc.path)},
        "position": {"line": line, "character": doc.utf16_col(line, col)},
    });
    Ok((params, doc.prefix(), doc.before_prefix()))
}

/// The folder of the project: servers start there. A new folder stops the running ones.
#[tauri::command(async)]
fn lsp_root(root: Option<String>, state: State<'_, AppState>) {
    state.lsp.set_root(root.map(|r| fs::canonicalize(&r).unwrap_or_else(|_| r.into())));
}

#[tauri::command(async)]
fn lsp_config(config: HashMap<String, lsp::Config>, state: State<'_, AppState>) {
    state.lsp.set_config(config);
}

#[derive(Serialize)]
struct ServerRow {
    id: &'static str,
    name: &'static str,
    languages: &'static [&'static str],
    /// The command it would be started with, when one is installed.
    command: Option<String>,
    install: &'static str,
    status: lsp::Status,
}

#[tauri::command(async)]
fn lsp_servers(state: State<'_, AppState>) -> Vec<ServerRow> {
    lsp::SERVERS
        .iter()
        .map(|s| ServerRow {
            id: s.id,
            name: s.name,
            languages: s.languages,
            command: lsp::command_for(s, &state.lsp.config_of(s.id)).map(|(p, a)| std::iter::once(p.to_string_lossy().into_owned()).chain(a).collect::<Vec<_>>().join(" ")),
            install: s.install,
            status: state.lsp.status(s.languages.first().copied()),
        })
        .collect()
}

/// How the server of a document is doing, for the status bar.
#[tauri::command(async)]
fn lsp_status(id: u32, state: State<'_, AppState>) -> lsp::Status {
    let lang = state.docs.lock().unwrap().get(&id).and_then(|d| d.lang.lsp_id());
    state.lsp.status(lang)
}

#[tauri::command(async)]
fn lsp_restart(server: String, state: State<'_, AppState>) {
    state.lsp.stop(&server);
}

#[tauri::command(async)]
fn lsp_sync(id: u32, app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    lsp_of(&app, &state, id)?.sync(&state, id)
}

#[tauri::command(async)]
fn lsp_complete(id: u32, force: bool, app: AppHandle, state: State<'_, AppState>) -> Res<Completions> {
    let lsp = lsp_of(&app, &state, id)?;
    let (params, prefix, before) = at_cursor(id, &lsp, &state)?;
    let member = before.ends_with('.') || before.ends_with("->") || before.ends_with("::");
    if !force && !member && prefix.chars().count() < 2 {
        return Ok(Completions { prefix, incomplete: false, items: vec![] });
    }
    let (indent, unit) = {
        let docs = state.docs.lock().unwrap();
        let doc = docs.get(&id).ok_or("Document is closed")?;
        (doc.line_start_text().chars().take_while(|c| c.is_whitespace()).collect::<String>(), doc.indent_unit())
    };
    let res = lsp.request("textDocument/completion", params, lsp::TIMEOUT)?;
    let incomplete = res["isIncomplete"].as_bool().unwrap_or(false);
    let list = res.get("items").unwrap_or(&res).as_array().cloned().unwrap_or_default();
    let mut sorted: Vec<&Value> = list.iter().collect();
    sorted.sort_by_key(|i| i["sortText"].as_str().or(i["label"].as_str()).unwrap_or("").to_string());
    let items = sorted
        .iter()
        .take(80)
        .map(|i| {
            let label = i["label"].as_str().unwrap_or("").trim().trim_start_matches('•').trim().to_string();
            let text = i["textEdit"]["newText"].as_str().or(i["insertText"].as_str()).unwrap_or(&label).to_string();
            // A snippet (insertTextFormat 2) has tab stops, the same syntax as VS Code's snippets.
            let (insert, stops) = if i["insertTextFormat"].as_u64() == Some(2) { ext::expand(&text, &indent, &unit) } else { (text, vec![]) };
            let key = i["filterText"].as_str().map(String::from).or_else(|| (!stops.is_empty()).then(|| label.clone()));
            Completion { detail: i["detail"].as_str().unwrap_or("").to_string(), insert, label, key, stops }
        })
        .collect();
    Ok(Completions { prefix, incomplete, items })
}

#[derive(Serialize)]
struct Location {
    path: String,
    line: usize,
    col: usize,
}

fn location(loc: &Value) -> Option<Location> {
    let uri = loc["uri"].as_str().or(loc["targetUri"].as_str())?;
    let range = if loc["range"].is_object() { &loc["range"] } else { &loc["targetSelectionRange"] };
    Some(Location {
        path: path_from_uri(uri)?.to_string_lossy().into_owned(),
        line: range["start"]["line"].as_u64().unwrap_or(0) as usize,
        col: range["start"]["character"].as_u64().unwrap_or(0) as usize,
    })
}

#[tauri::command(async)]
fn lsp_definition(id: u32, app: AppHandle, state: State<'_, AppState>) -> Res<Option<Location>> {
    let lsp = lsp_of(&app, &state, id)?;
    let (params, _, _) = at_cursor(id, &lsp, &state)?;
    let res = lsp.request("textDocument/definition", params, lsp::TIMEOUT)?;
    let loc = if res.is_array() { res.get(0).cloned().unwrap_or(Value::Null) } else { res };
    Ok(location(&loc))
}

#[tauri::command(async)]
fn lsp_hover(id: u32, app: AppHandle, state: State<'_, AppState>) -> Res<String> {
    let lsp = lsp_of(&app, &state, id)?;
    let (params, _, _) = at_cursor(id, &lsp, &state)?;
    let res = lsp.request("textDocument/hover", params, lsp::TIMEOUT)?;
    Ok(lsp::hover_text(&res["contents"]))
}

/// The hover of the symbol under the pointer, as VS Code shows it.
#[tauri::command(async)]
fn lsp_hover_at(id: u32, row: usize, vcol: usize, app: AppHandle, state: State<'_, AppState>) -> Res<String> {
    let lsp = lsp_of(&app, &state, id)?;
    if !lsp.can("hoverProvider") {
        return Ok(String::new());
    }
    lsp.sync(&state, id)?;
    let params = {
        let docs = state.docs.lock().unwrap();
        let doc = docs.get(&id).ok_or("Document is closed")?;
        let (line, col) = doc.point_at(row, vcol);
        json!({"textDocument": {"uri": uri_from_path(&doc.path)}, "position": {"line": line, "character": doc.utf16_col(line, col)}})
    };
    let res = lsp.request("textDocument/hover", params, lsp::TIMEOUT)?;
    Ok(lsp::hover_text(&res["contents"]))
}

#[tauri::command(async)]
fn lsp_format(id: u32, app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    let lsp = lsp_of(&app, &state, id)?;
    if !lsp.can("documentFormattingProvider") {
        return Err(format!("{} can't format this file", lsp.server.name));
    }
    lsp.sync(&state, id)?;
    let (params, rev) = {
        let docs = state.docs.lock().unwrap();
        let doc = docs.get(&id).ok_or("Document is closed")?;
        (json!({"textDocument": {"uri": uri_from_path(&doc.path)}, "options": {"tabSize": doc.tab, "insertSpaces": !doc.use_tabs}}), doc.rev)
    };
    let res = lsp.request("textDocument/formatting", params, lsp::TIMEOUT)?;
    let n = |v: &Value| v.as_u64().unwrap_or(0) as usize;
    let edits = res
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .map(|e| {
            let r = &e["range"];
            (n(&r["start"]["line"]), n(&r["start"]["character"]), n(&r["end"]["line"]), n(&r["end"]["character"]), e["newText"].as_str().unwrap_or("").to_string())
        })
        .collect();
    let mut docs = state.docs.lock().unwrap();
    let doc = docs.get_mut(&id).ok_or("Document is closed")?;
    if doc.rev != rev {
        return Err("The file changed while it was being formatted".into());
    }
    doc.apply_utf16_edits(edits);
    Ok(())
}

#[tauri::command(async)]
fn lsp_references(id: u32, app: AppHandle, state: State<'_, AppState>) -> Res<Vec<project::Hit>> {
    let lsp = lsp_of(&app, &state, id)?;
    let (mut params, _, _) = at_cursor(id, &lsp, &state)?;
    params["context"] = json!({"includeDeclaration": true});
    let res = lsp.request("textDocument/references", params, std::time::Duration::from_secs(20))?;
    let mut files: HashMap<std::path::PathBuf, Vec<String>> = HashMap::new();
    let mut hits = vec![];
    for loc in res.as_array().map_or(&[][..], Vec::as_slice) {
        let Some(path) = loc["uri"].as_str().and_then(path_from_uri) else { continue };
        let line = loc["range"]["start"]["line"].as_u64().unwrap_or(0) as usize;
        let lines = files.entry(path.clone()).or_insert_with(|| fs::read_to_string(&path).map(|t| t.lines().map(String::from).collect()).unwrap_or_default());
        let shown = path.to_string_lossy().into_owned();
        hits.push(project::Hit {
            rel: shown.clone(),
            path: shown,
            line,
            col: loc["range"]["start"]["character"].as_u64().unwrap_or(0) as usize,
            text: lines.get(line).map_or(String::new(), |l| l.trim().chars().take(160).collect()),
        });
    }
    Ok(hits)
}

#[tauri::command(async)]
fn lsp_rename(id: u32, name: String, app: AppHandle, state: State<'_, AppState>) -> Res<Vec<FileEdits>> {
    let lsp = lsp_of(&app, &state, id)?;
    let (mut params, _, _) = at_cursor(id, &lsp, &state)?;
    params["newName"] = json!(name);
    let res = lsp.request("textDocument/rename", params, std::time::Duration::from_secs(20))?;
    Ok(parse_workspace_edit(&res))
}

#[derive(Serialize)]
struct Symbol {
    name: String,
    detail: String,
    kind: u64,
    depth: usize,
    line: usize,
    col: usize,
}

fn flatten_symbols(list: &[Value], depth: usize, out: &mut Vec<Symbol>) {
    for s in list {
        // Hierarchical symbols have a selectionRange; the flat kind only a location.
        let start = if s["selectionRange"].is_object() { &s["selectionRange"]["start"] } else { &s["location"]["range"]["start"] };
        out.push(Symbol {
            name: s["name"].as_str().unwrap_or("").to_string(),
            detail: s["detail"].as_str().or(s["containerName"].as_str()).unwrap_or("").to_string(),
            kind: s["kind"].as_u64().unwrap_or(0),
            depth,
            line: start["line"].as_u64().unwrap_or(0) as usize,
            col: start["character"].as_u64().unwrap_or(0) as usize,
        });
        flatten_symbols(s["children"].as_array().map_or(&[], Vec::as_slice), depth + 1, out);
    }
}

#[tauri::command(async)]
fn lsp_symbols(id: u32, app: AppHandle, state: State<'_, AppState>) -> Res<Vec<Symbol>> {
    let lsp = lsp_of(&app, &state, id)?;
    let (params, _, _) = at_cursor(id, &lsp, &state)?;
    let res = lsp.request("textDocument/documentSymbol", json!({"textDocument": params["textDocument"]}), lsp::TIMEOUT)?;
    let mut out = vec![];
    flatten_symbols(res.as_array().map_or(&[], Vec::as_slice), 0, &mut out);
    Ok(out)
}

#[tauri::command(async)]
fn lsp_workspace_symbols(query: String, id: u32, app: AppHandle, state: State<'_, AppState>) -> Res<Vec<project::Hit>> {
    let lsp = lsp_of(&app, &state, id)?;
    let res = lsp.request("workspace/symbol", json!({"query": query}), lsp::TIMEOUT)?;
    Ok(res
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .take(80)
        .filter_map(|s| {
            let path = s["location"]["uri"].as_str().and_then(path_from_uri)?;
            let shown = path.to_string_lossy().into_owned();
            let container = s["containerName"].as_str().filter(|c| !c.is_empty());
            Some(project::Hit {
                rel: shown.clone(),
                path: shown,
                line: s["location"]["range"]["start"]["line"].as_u64().unwrap_or(0) as usize,
                col: s["location"]["range"]["start"]["character"].as_u64().unwrap_or(0) as usize,
                text: match container {
                    Some(c) => format!("{} ({c})", s["name"].as_str().unwrap_or("")),
                    None => s["name"].as_str().unwrap_or("").to_string(),
                },
            })
        })
        .collect())
}

#[derive(Serialize)]
struct CodeAction {
    title: String,
    edit: Vec<FileEdits>,
    command: Option<Value>,
}

#[tauri::command(async)]
fn lsp_code_actions(id: u32, app: AppHandle, state: State<'_, AppState>) -> Res<Vec<CodeAction>> {
    let lsp = lsp_of(&app, &state, id)?;
    let (params, _, _) = at_cursor(id, &lsp, &state)?;
    let diagnostics: Vec<Value> = {
        let docs = state.docs.lock().unwrap();
        let doc = docs.get(&id).ok_or("Document is closed")?;
        let (line, _) = doc.cursor_pos();
        doc.diags.iter().filter(|d| d.sl <= line && line <= d.el).map(|d| d.raw.clone()).collect()
    };
    let req = json!({
        "textDocument": params["textDocument"],
        "range": {"start": params["position"], "end": params["position"]},
        "context": {"diagnostics": diagnostics},
    });
    let res = lsp.request("textDocument/codeAction", req, lsp::TIMEOUT)?;
    Ok(res
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .map(|a| CodeAction {
            title: a["title"].as_str().unwrap_or("").to_string(),
            edit: parse_workspace_edit(&a["edit"]),
            command: if a["command"].is_object() { Some(a["command"].clone()) } else { a["arguments"].is_array().then(|| a.clone()) },
        })
        .collect())
}

#[tauri::command(async)]
fn lsp_exec_command(command: Value, id: u32, app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    let lsp = lsp_of(&app, &state, id)?;
    lsp.request("workspace/executeCommand", json!({"command": command["command"], "arguments": command["arguments"]}), lsp::TIMEOUT).map(|_| ())
}

type Signature = (String, Option<(usize, usize)>);

#[tauri::command(async)]
fn lsp_signature(id: u32, app: AppHandle, state: State<'_, AppState>) -> Res<Option<Signature>> {
    let lsp = lsp_of(&app, &state, id)?;
    if !lsp.can("signatureHelpProvider") {
        return Ok(None);
    }
    let (params, _, _) = at_cursor(id, &lsp, &state)?;
    let res = lsp.request("textDocument/signatureHelp", params, lsp::TIMEOUT)?;
    let i = res["activeSignature"].as_u64().unwrap_or(0) as usize;
    let sig = &res["signatures"][i];
    let Some(label) = sig["label"].as_str() else { return Ok(None) };
    let param = sig["activeParameter"].as_u64().or(res["activeParameter"].as_u64()).unwrap_or(0) as usize;
    let p = &sig["parameters"][param]["label"];
    // The parameter is given as a range of the label, or as its text.
    let range = p
        .as_array()
        .and_then(|r| Some((r.first()?.as_u64()? as usize, r.get(1)?.as_u64()? as usize)))
        .or_else(|| p.as_str().and_then(|t| label.find(t).map(|b| (label[..b].encode_utf16().count(), label[..b].encode_utf16().count() + t.encode_utf16().count()))));
    Ok(Some((label.to_string(), range)))
}

#[tauri::command(async)]
fn lsp_semantic(id: u32, app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    let lsp = lsp_of(&app, &state, id)?;
    if !lsp.can("semanticTokensProvider") {
        return Ok(());
    }
    lsp.sync(&state, id)?;
    let (uri, rev) = {
        let docs = state.docs.lock().unwrap();
        let doc = docs.get(&id).ok_or("Document is closed")?;
        (uri_from_path(&doc.path), doc.rev)
    };
    let res = lsp.request("textDocument/semanticTokens/full", json!({"textDocument": {"uri": uri}}), lsp::TIMEOUT)?;
    let tokens = lsp.decode_semantic(res["data"].as_array().map_or(&[], Vec::as_slice));
    let mut docs = state.docs.lock().unwrap();
    if let Some(doc) = docs.get_mut(&id).filter(|d| d.rev == rev) {
        doc.set_semantic(tokens);
    }
    Ok(())
}

#[tauri::command(async)]
async fn ai_chat(
    id: u32,
    endpoint: ai::Endpoint,
    messages: Vec<ai::Message>,
    app: AppHandle,
) -> Res<()> {
    ai::chat(&app, id, endpoint, messages).await
}

#[tauri::command]
fn ai_cancel(id: u32) {
    ai::cancel(id);
}

#[tauri::command(async)]
async fn ai_models(endpoint: ai::Endpoint) -> Res<Vec<String>> {
    ai::models(endpoint).await
}

/// The open file as context for the chat: all of it when short, otherwise the part around the caret.
#[tauri::command]
fn doc_context(id: u32, max: usize, state: State<AppState>) -> Option<(String, String)> {
    let docs = state.docs.lock().unwrap();
    let doc = docs.get(&id)?;
    let name = doc.path.file_name()?.to_string_lossy().into_owned();
    let total = doc.rope.len_chars();
    if total <= max {
        return Some((name, doc.rope.to_string()));
    }
    let (line, _) = doc.cursor_pos();
    let around = doc
        .rope
        .line_to_char(line.min(doc.rope.len_lines().saturating_sub(1)));
    let from = around.saturating_sub(max / 2);
    let to = (from + max).min(total);
    Some((name, doc.rope.slice(from..to).to_string()))
}

/// Asks the built-in AI to finish the line; the answer arrives as an `ai-suggestion` event.
#[tauri::command]
fn ai_suggest(id: u32, size: String, app: AppHandle, state: State<AppState>) {
    let snapshot = {
        let docs = state.docs.lock().unwrap();
        let Some(doc) = docs.get(&id) else { return };
        let Some((line, col, prefix, suffix)) = doc.line_context() else {
            return;
        };
        if !suggest::suffix_is_closers(&suffix) || prefix.trim().chars().count() < 2 {
            return;
        }
        let (head, before, after) = doc.windows(2000, 800);
        let name = |d: &Doc| {
            d.path
                .file_name()
                .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned())
        };
        let mut others: Vec<(bool, String, String)> = docs
            .iter()
            .filter(|(k, d)| **k != id && d.lang.lsp_id().is_some() && d.mtime.is_some())
            .map(|(_, d)| {
                let n = d.rope.len_chars().min(700);
                (
                    name(d).ends_with(".h"),
                    name(d),
                    d.rope.slice(..n).to_string(),
                )
            })
            .collect();
        others.sort_by_key(|o| !o.0);
        ai::Snapshot {
            key: format!("{}:{}:{}", doc.rev, line, col + 1),
            file: name(doc),
            head,
            prefix: before,
            suffix: after,
            others: others.into_iter().take(2).map(|o| (o.1, o.2)).collect(),
        }
    };
    ai::suggest(app, size, snapshot);
}

#[tauri::command]
fn ai_suggest_cancel() {
    ai::cancel_suggest();
}

#[derive(Serialize)]
struct LineGhost {
    text: String,
    key: String,
}

#[tauri::command(async)]
fn line_suggest(id: u32, root: String, state: State<'_, AppState>) -> Option<LineGhost> {
    let docs = state.docs.lock().unwrap();
    let doc = docs.get(&id)?;
    let (line, col, prefix, suffix) = doc.line_context()?;
    if !suggest::suffix_is_closers(&suffix) {
        return None;
    }
    let key = format!("{}:{}:{}", doc.rev, line, col + 1);
    let open: Vec<&Path> = docs.values().map(|d| d.path.as_path()).collect();
    let project = suggest::project_files(&root);
    let here = doc
        .lines_for_suggest()
        .filter(|(i, _)| *i != line)
        .map(|(_, t)| (3, t));
    let others = docs
        .iter()
        .filter(|(k, _)| **k != id)
        .flat_map(|(_, d)| d.lines_for_suggest().map(|(_, t)| (2, t)));
    let files = project
        .iter()
        .filter(|(p, _)| !open.contains(&p.as_path()))
        .flat_map(|(_, text)| text.lines().map(|l| (1, std::borrow::Cow::Borrowed(l))));
    let text = suggest::complete(
        &prefix,
        &suffix,
        here.chain(others).chain(files),
        match doc.lang.legacy() {
            lexer::Lang::C => Some(false),
            lexer::Lang::Cpp => Some(true),
            _ => None,
        },
    )?;
    Some(LineGhost { text, key })
}

#[tauri::command(async)]
fn word_complete(id: u32, force: bool, state: State<'_, AppState>) -> Res<Completions> {
    const MAX_SCANNED: usize = 512 * 1024;
    let mut docs = state.docs.lock().unwrap();
    let doc = docs.get_mut(&id).ok_or("Document is closed")?;
    let prefix = doc.prefix();
    let lang = doc.lang;
    let indent: String = doc.line_start_text().chars().take_while(|c| c.is_whitespace()).collect();
    let unit = doc.indent_unit();
    let long_enough = prefix.chars().count() >= if force { 1 } else { 2 };
    if lang.lsp_id().is_none() || !long_enough || doc.after_comment_or_string() {
        return Ok(Completions {
            prefix,
            incomplete: false,
            items: vec![],
        });
    }
    let mut left = MAX_SCANNED;
    let mut sources = vec![];
    let active = docs.get(&id).into_iter();
    let others = docs.iter().filter(|(k, _)| **k != id).map(|(_, d)| d);
    for d in active.chain(others) {
        if d.lang.lsp_id().is_none() || left == 0 {
            continue;
        }
        let n = d.rope.len_chars().min(left);
        left -= n;
        sources.push(d.rope.slice(..n).to_string());
    }
    let items: Vec<Completion> = words::suggest(&prefix, &lexer::vocabulary(lang), &sources)
        .into_iter()
        .map(|w| Completion {
            label: w.text.clone(),
            insert: w.text,
            detail: if w.keyword { "keyword" } else { "" }.to_string(),
            key: None,
            stops: vec![],
        })
        .collect();
    let items = {
        let mut all: Vec<Completion> = ext::snippets_for(lang.lsp_id().unwrap_or(""), &prefix, &indent, &unit)
            .into_iter()
            .map(|(key, insert, detail, stops)| Completion { label: key.clone(), insert, detail: if detail.is_empty() { "snippet".into() } else { detail }, key: Some(key), stops })
            .collect();
        all.extend(items);
        all
    };
    Ok(Completions {
        prefix,
        incomplete: false,
        items,
    })
}

#[tauri::command]
fn doc_open_virtual(key: String, name: String, text: String, state: State<AppState>) -> Opened {
    let mut docs = state.docs.lock().unwrap();
    let path = std::path::PathBuf::from(&key);
    let existing = docs.iter().find(|(_, d)| d.path == path).map(|(id, _)| *id);
    let id = existing.unwrap_or_else(|| state.next_doc.fetch_add(1, Ordering::Relaxed));
    let mut doc = Doc::new(path, &text, None);
    doc.lang = lexer::Lang::Diff;
    doc.readonly = true;
    docs.insert(id, doc);
    Opened {
        id,
        path: key,
        name,
    }
}

#[tauri::command]
fn doc_apply_edits(
    id: u32,
    edits: Vec<(usize, usize, usize, usize, String)>,
    state: State<AppState>,
) -> Res<()> {
    let mut docs = state.docs.lock().unwrap();
    docs.get_mut(&id)
        .ok_or("Document is closed")?
        .apply_utf16_edits(edits);
    Ok(())
}

#[derive(Serialize)]
struct DiagRow {
    line: usize,
    col: usize,
    sev: usize,
    msg: String,
}

#[tauri::command]
fn doc_diags(id: u32, state: State<AppState>) -> Vec<DiagRow> {
    let docs = state.docs.lock().unwrap();
    let Some(d) = docs.get(&id) else {
        return vec![];
    };
    let mut rows: Vec<DiagRow> = d
        .diags
        .iter()
        .map(|x| DiagRow {
            line: x.sl,
            col: d.utf16_col(x.sl.min(d.rope.len_lines() - 1), x.sc),
            sev: x.sev,
            msg: x.msg.clone(),
        })
        .collect();
    rows.sort_by_key(|r| (r.sev, r.line));
    rows
}

#[tauri::command]
fn doc_probe(
    id: u32,
    row: usize,
    vcol: usize,
    state: State<AppState>,
) -> (Option<String>, Option<String>) {
    let docs = state.docs.lock().unwrap();
    docs.get(&id).map_or((None, None), |d| {
        (d.diag_at(row, vcol), d.word_at(row, vcol))
    })
}

#[tauri::command]
fn doc_next_problem(id: u32, state: State<AppState>) {
    let mut docs = state.docs.lock().unwrap();
    if let Some(d) = docs.get_mut(&id) {
        if let Some((l, c)) = d.next_problem() {
            d.goto_chars(l, c);
        }
    }
}

#[tauri::command(async)]
fn git_status(root: String) -> Option<git::Status> {
    git::status(&root).ok()
}

#[tauri::command(async)]
fn git_stage(root: String, paths: Vec<String>) -> Res<()> {
    git::stage(&root, &paths)
}

#[tauri::command(async)]
fn git_unstage(root: String, paths: Vec<String>) -> Res<()> {
    git::unstage(&root, &paths)
}

#[tauri::command(async)]
fn git_discard(root: String, path: String, untracked: bool) -> Res<()> {
    git::discard(&root, &path, untracked)
}

#[tauri::command(async)]
fn git_commit(root: String, message: String, amend: bool) -> Res<String> {
    git::commit(&root, &message, amend)
}

#[tauri::command(async)]
fn git_diff(root: String, path: String, untracked: bool) -> String {
    git::diff(&root, &path, untracked)
}

#[tauri::command(async)]
fn git_log(root: String) -> Res<Vec<git::Commit>> {
    git::log(&root)
}

#[tauri::command(async)]
fn git_show(root: String, hash: String) -> Res<String> {
    git::show(&root, &hash)
}

#[tauri::command(async)]
fn git_branches(root: String) -> Res<Vec<String>> {
    git::branches(&root)
}

#[tauri::command(async)]
fn git_clone(url: String, parent: String) -> Res<String> {
    git::clone(&url, &parent)
}

#[tauri::command(async)]
fn git_stash(root: String, pop: bool) -> Res<String> {
    git::stash(&root, pop)
}

#[tauri::command(async)]
fn git_merge(root: String, name: String) -> Res<String> {
    git::merge(&root, &name)
}

#[tauri::command(async)]
fn git_delete_branch(root: String, name: String) -> Res<()> {
    git::delete_branch(&root, &name)
}

#[tauri::command(async)]
fn git_checkout(root: String, name: String, create: bool) -> Res<()> {
    git::checkout(&root, &name, create)
}

#[tauri::command(async)]
fn git_refresh(id: u32, state: State<'_, AppState>) {
    let path = state
        .docs
        .lock()
        .unwrap()
        .get(&id)
        .filter(|d| d.mtime.is_some())
        .map(|d| d.path.clone());
    let Some(path) = path else { return };
    let marks = git::line_marks(&path);
    if let Some(d) = state.docs.lock().unwrap().get_mut(&id) {
        d.git_marks = marks;
    }
}

#[tauri::command(async)]
fn term_open(
    cwd: String,
    cols: usize,
    rows: usize,
    app: AppHandle,
    state: State<AppState>,
) -> Res<u32> {
    let id = state.next_term.fetch_add(1, Ordering::Relaxed);
    let t = term::Term::spawn(app, id, &cwd, cols.max(2), rows.max(2))?;
    state.terms.lock().unwrap().insert(id, t);
    Ok(id)
}

#[tauri::command]
fn term_view(id: u32, offset: usize, full: bool, state: State<AppState>) -> Res<term::TermView> {
    let t = state
        .terms
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or("Terminal is closed")?;
    t.notified.store(false, Ordering::Release);
    let v = t.inner.lock().unwrap().grid.view(offset, full);
    Ok(v)
}

#[tauri::command]
fn term_input(id: u32, data: String, state: State<AppState>) -> Res<()> {
    let t = state
        .terms
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or("Terminal is closed")?;
    t.write(data.as_bytes())
}

#[tauri::command]
fn term_resize(id: u32, cols: usize, rows: usize, state: State<AppState>) {
    if let Some(t) = state.terms.lock().unwrap().get(&id).cloned() {
        t.resize(cols.max(2), rows.max(2));
    }
}

#[tauri::command(async)]
fn term_info(id: u32, state: State<'_, AppState>) -> Res<term::TermInfo> {
    let t = state
        .terms
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or("Terminal is closed")?;
    Ok(t.info())
}

#[tauri::command(async)]
fn term_close(id: u32, state: State<AppState>) {
    if let Some(t) = state.terms.lock().unwrap().remove(&id) {
        t.kill();
    }
}

#[cfg(target_os = "linux")]
fn tune_webkit() {
    if std::env::var_os("JSC_useJIT").is_none() {
        std::env::set_var("JSC_useJIT", "0");
    }
}

#[tauri::command]
async fn ext_search(query: String, category: String, offset: u32) -> Res<ext::Page> {
    ext::search(&query, &category, offset).await
}

#[tauri::command]
async fn ext_install(item: ext::Item, app: AppHandle) -> Res<ext::Installed> {
    ext::install(&app, item).await
}

#[tauri::command(async)]
fn ext_remove(id: String, app: AppHandle) -> Res<()> {
    ext::remove(&app, &id)
}

#[tauri::command(async)]
fn ext_list(app: AppHandle) -> Vec<ext::Installed> {
    ext::list(&app)
}

#[tauri::command(async)]
fn ext_theme(id: String, path: String, label: String, light: bool, app: AppHandle) -> Res<serde_json::Value> {
    ext::theme(&app, &id, &path, &label, light)
}

#[tauri::command(async)]
fn ext_snippets(app: AppHandle) -> Vec<ext::Snippet> {
    ext::snippets(&app)
}

/// After extensions changed: documents whose language changed are colored again.
#[tauri::command]
fn docs_relang(state: State<AppState>) -> Vec<u32> {
    let mut docs = state.docs.lock().unwrap();
    docs.iter_mut().filter_map(|(id, d)| d.relang().then_some(*id)).collect()
}

#[derive(Serialize)]
struct LangItem {
    id: String,
    label: String,
}

/// The languages that can be chosen by hand.
#[tauri::command]
fn languages_list() -> Vec<LangItem> {
    let mut list: Vec<LangItem> = grammars::languages().into_iter().map(|(id, label)| LangItem { id: id.into(), label: label.into() }).collect();
    for (id, label) in [("kotlin", "Kotlin"), ("toml", "TOML"), ("cmake", "CMake"), ("config", "Config")] {
        list.push(LangItem { id: id.into(), label: label.into() });
    }
    list.sort_by_key(|l| l.label.to_lowercase());
    list.insert(0, LangItem { id: "plaintext".into(), label: "Plain Text".into() });
    list
}

/// Picks the language of an open document by hand. `None` goes back to what the file name says.
#[tauri::command]
fn doc_set_language(id: u32, lang: Option<String>, state: State<AppState>) -> Res<String> {
    let mut docs = state.docs.lock().unwrap();
    let doc = docs.get_mut(&id).ok_or("Document is closed")?;
    doc.set_language(lang.as_deref())?;
    Ok(doc.lang.label())
}

#[cfg(unix)]
static SIGNALED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_signal(_: libc::c_int) {
    SIGNALED.store(true, Ordering::SeqCst);
}

/// Being asked to end (logging out, the terminal closing, `kill`) stops the helpers too, so
/// the AI engine does not stay behind using memory.
#[cfg(unix)]
fn watch_signals(app: AppHandle) {
    unsafe {
        for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
            libc::signal(sig, on_signal as *const () as libc::sighandler_t);
        }
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if SIGNALED.load(Ordering::SeqCst) {
            stop_children(&app.state::<AppState>());
            app.exit(0);
            return;
        }
    });
}

/// Works out the line states of the open files while nothing else needs the processor, a few
/// milliseconds at a time. The slices are short because the documents are locked meanwhile, and
/// drawing the editor waits for them: at 120 Hz a frame has 8 ms.
fn spawn_warmer(app: AppHandle) {
    std::thread::spawn(move || loop {
        let did = {
            let state = app.state::<AppState>();
            let mut docs = state.docs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            docs.values_mut().any(|d| d.warm(4))
        };
        std::thread::sleep(std::time::Duration::from_millis(if did { 6 } else { 400 }));
    });
}

/// Many commands wait: for git, for the disk, for the network. Each one holds a worker of the
/// runtime while it waits, and Tauri starts only as many as there are cores, so on a small
/// machine a few slow requests were enough to keep everything else in line. Waiting costs no CPU,
/// so there are more workers than that.
fn use_own_runtime() {
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    if let Ok(rt) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads((cores * 8).clamp(16, 32))
        .thread_name("assioma-worker")
        .enable_all()
        .build()
    {
        // It lives as long as the program, and is never dropped (that would wait for its tasks).
        tauri::async_runtime::set(Box::leak(Box::new(rt)).handle().clone());
    }
}

fn main() {
    #[cfg(windows)]
    if let Some(code) = cli::handle_flag() {
        std::process::exit(code);
    }

    #[cfg(target_os = "linux")]
    tune_webkit();

    use_own_runtime();

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState {
            docs: Mutex::new(HashMap::new()),
            lsp: lsp::Servers::default(),
            next_doc: AtomicU32::new(1),
            runner: Arc::new(Runner::default()),
            terms: Mutex::new(HashMap::new()),
            next_term: AtomicU32::new(1),
        })
        .setup(|app| {
            ext::reload(app.handle());
            llama::sweep(app.handle());
            spawn_warmer(app.handle().clone());
            #[cfg(unix)]
            watch_signals(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.emit("close-requested", ());
            }
        })
        .invoke_handler(tauri::generate_handler![
            #[cfg(feature = "selftest")]
            selftest_log,
            term_open,
            term_view,
            term_input,
            term_resize,
            term_info,
            term_close,
            #[cfg(any(unix, windows))]
            install_cli,
            word_complete,
            line_suggest,
            ai_chat,
            ai_suggest,
            ai_suggest_cancel,
            ai_cancel,
            ai_models,
            doc_context,
            tools_status,
            ai_builtin_installed,
            ai_warm,
            ai_remove,
            tools_install,
            clipboard_read,
            clipboard_write,
            doc_copy,
            discord_presence,
            app_version,
            update_check,
            update_install,
            doc_open_virtual,
            doc_apply_edits,
            lsp_root,
            lsp_config,
            lsp_servers,
            lsp_status,
            lsp_restart,
            lsp_sync,
            lsp_complete,
            lsp_definition,
            lsp_hover,
            lsp_hover_at,
            lsp_format,
            lsp_references,
            lsp_rename,
            lsp_symbols,
            lsp_workspace_symbols,
            lsp_code_actions,
            lsp_exec_command,
            lsp_signature,
            lsp_semantic,
            doc_diags,
            doc_probe,
            doc_next_problem,
            git_status,
            git_stage,
            git_unstage,
            git_discard,
            git_commit,
            git_diff,
            git_log,
            git_show,
            git_branches,
            git_checkout,
            git_stash,
            git_merge,
            git_delete_branch,
            git_clone,
            git_refresh,
            app_ready,
            app_quit,
            initial_path,
            doc_open,
            doc_close,
            doc_apply,
            doc_save,
            doc_search,
            docs_refresh,
            list_dir,
            list_files,
            grep,
            replace_in_project,
            fs_new_file,
            fs_new_dir,
            fs_rename,
            fs_delete,
            path_exists,
            sys_info,
            task_start,
            task_stdin,
            task_kill,
            run_start,
            term_note,
            make_dirs,
            ext_search,
            ext_install,
            ext_remove,
            ext_list,
            ext_theme,
            ext_snippets,
            docs_relang,
            languages_list,
            doc_set_language
        ])
        .run(tauri::generate_context!())
        .expect("failed to start assioma");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("assioma-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn saving_replaces_the_file_and_leaves_nothing_behind() {
        let d = scratch("save");
        let f = d.join("a.c");
        fs::write(&f, "old").unwrap();
        write_safely(&f, "new", "assioma", false).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "new");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_read_only_file_is_not_overwritten() {
        let d = scratch("ro");
        let f = d.join("a.c");
        fs::write(&f, "old").unwrap();
        let mut p = fs::metadata(&f).unwrap().permissions();
        p.set_readonly(true);
        fs::set_permissions(&f, p).unwrap();
        let e = write_safely(&f, "new", "assioma", false).unwrap_err();
        assert!(e.contains("read-only"), "{e}");
        assert_eq!(fs::read_to_string(&f).unwrap(), "old");
        let mut p = fs::metadata(&f).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        p.set_readonly(false);
        let _ = fs::set_permissions(&f, p);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn overwriting_a_read_only_file_makes_it_writable() {
        let d = scratch("unlock");
        let f = d.join("a.c");
        fs::write(&f, "old").unwrap();
        let mut p = fs::metadata(&f).unwrap().permissions();
        p.set_readonly(true);
        fs::set_permissions(&f, p).unwrap();
        write_safely(&f, "new", "assioma", true).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "new");
        assert!(!fs::metadata(&f).unwrap().permissions().readonly());
        let _ = fs::remove_dir_all(&d);
    }

    /// Saving through one name of a hard-linked file changes it under every name.
    #[cfg(unix)]
    #[test]
    fn a_hard_linked_file_stays_linked() {
        let d = scratch("link");
        let (f, g) = (d.join("a.c"), d.join("b.c"));
        fs::write(&f, "old").unwrap();
        fs::hard_link(&f, &g).unwrap();
        write_safely(&f, "new", "assioma", false).unwrap();
        assert_eq!(fs::read_to_string(&g).unwrap(), "new");
        let _ = fs::remove_dir_all(&d);
    }

    /// A shared folder where only the file itself may be changed.
    #[cfg(unix)]
    #[test]
    fn a_file_in_a_locked_folder_is_written_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("locked");
        let f = d.join("a.c");
        fs::write(&f, "old").unwrap();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o555)).unwrap();
        let r = write_safely(&f, "new", "assioma", false);
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        // Running as root ignores the folder's permissions; then the usual way is taken.
        r.unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "new");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&d);
    }
}
