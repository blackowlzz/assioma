//! Extensions from Open VSX (https://open-vsx.org), the open marketplace of VS Code's code.
//! Assioma runs no extension code. It uses what an extension only declares: color themes, languages
//! with their TextMate grammars, and snippets.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use zip::ZipArchive;

const HOST: &str = "open-vsx.org";
const MAX_DOWNLOAD: u64 = 120 << 20;
const MAX_UNPACKED: u64 = 400 << 20;
const MAX_FILES: usize = 20_000;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Item {
    pub id: String,
    pub ns: String,
    pub name: String,
    pub display: String,
    pub description: String,
    pub version: String,
    pub downloads: u64,
    pub rating: f64,
    pub icon: Option<String>,
    pub download: String,
    pub sha256: Option<String>,
    pub verified: bool,
}

#[derive(Serialize)]
pub struct Page {
    pub items: Vec<Item>,
    pub total: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct Installed {
    pub id: String,
    pub ns: String,
    pub name: String,
    pub display: String,
    pub description: String,
    pub version: String,
    pub themes: Vec<ThemeRef>,
    pub languages: Vec<String>,
    pub snippets: usize,
    /// The extension also runs code, which Assioma does not do, so only part of it works here.
    pub partial: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct ThemeRef {
    pub label: String,
    pub path: String,
    pub light: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct Meta {
    ns: String,
    name: String,
    version: String,
    display: String,
    description: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct Snippet {
    pub lang: String,
    pub prefix: String,
    pub description: String,
    pub body: String,
}

fn valid_part(s: &str) -> bool {
    !s.is_empty() && s.len() < 100 && !s.starts_with('.') && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub fn root(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?.join("extensions");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn client() -> Result<reqwest::Client, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .user_agent("assioma")
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| e.to_string())
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn item_of(v: &Value) -> Option<Item> {
    let (ns, name) = (str_of(v, "namespace"), str_of(v, "name"));
    let download = v["files"]["download"].as_str()?.to_string();
    if !valid_part(&ns) || !valid_part(&name) || v["deprecated"].as_bool() == Some(true) {
        return None;
    }
    let display = Some(str_of(v, "displayName")).filter(|d| !d.is_empty()).unwrap_or_else(|| name.clone());
    Some(Item {
        id: format!("{ns}.{name}"),
        ns,
        name,
        display,
        description: str_of(v, "description"),
        version: str_of(v, "version"),
        downloads: v["downloadCount"].as_u64().unwrap_or(0),
        rating: v["averageRating"].as_f64().unwrap_or(0.0),
        icon: v["files"]["icon"].as_str().map(String::from),
        download,
        sha256: v["files"]["sha256"].as_str().map(String::from),
        verified: v["verified"].as_bool().unwrap_or(false),
    })
}

pub async fn search(query: &str, category: &str, offset: u32) -> Result<Page, String> {
    let mut url = reqwest::Url::parse(&format!("https://{HOST}/api/-/search")).map_err(|e| e.to_string())?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("query", query.trim());
        if !category.is_empty() {
            q.append_pair("category", category);
        }
        q.append_pair("size", "30");
        q.append_pair("offset", &offset.to_string());
        q.append_pair("sortBy", if query.trim().is_empty() { "downloadCount" } else { "relevance" });
        q.append_pair("sortOrder", "desc");
    }
    let body: Value = client()?
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Open VSX can't be reached: {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))?;
    let items = body["extensions"].as_array().map(|a| a.iter().filter_map(item_of).collect()).unwrap_or_default();
    Ok(Page { items, total: body["totalSize"].as_u64().unwrap_or(0) })
}

fn trusted(url: &str) -> Result<reqwest::Url, String> {
    let u = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
    if u.scheme() != "https" || u.host_str() != Some(HOST) {
        return Err("Extensions can only come from open-vsx.org".into());
    }
    Ok(u)
}

async fn download(client: &reqwest::Client, url: &str, dest: &Path) -> Result<String, String> {
    let mut response = client
        .get(trusted(url)?)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("The download failed: {e}"))?;
    if response.content_length().is_some_and(|n| n > MAX_DOWNLOAD) {
        return Err("The extension is too big".into());
    }
    let mut file = File::create(dest).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut done = 0u64;
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        done += chunk.len() as u64;
        if done > MAX_DOWNLOAD {
            return Err("The extension is too big".into());
        }
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        hash.update(&chunk);
    }
    file.flush().map_err(|e| e.to_string())?;
    Ok(format!("{:x}", hash.finalize()))
}

/// Unpacks the `extension/` folder of a .vsix, refusing paths that leave the target.
fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    let mut zip = ZipArchive::new(File::open(archive).map_err(|e| e.to_string())?).map_err(|e| format!("Not a valid extension: {e}"))?;
    if zip.len() > MAX_FILES {
        return Err("The extension has too many files".into());
    }
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(name) = entry.enclosed_name() else { continue };
        let Ok(rel) = name.strip_prefix("extension") else { continue };
        if rel.as_os_str().is_empty() || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            continue;
        }
        let to = into.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&to).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = File::create(&to).map_err(|e| e.to_string())?;
        // The sizes in a zip can lie, so what is really written is what counts.
        total += std::io::copy(&mut entry.by_ref().take(MAX_UNPACKED - total + 1), &mut out).map_err(|e| e.to_string())?;
        if total > MAX_UNPACKED {
            return Err("The extension is too big once unpacked".into());
        }
    }
    Ok(())
}

pub async fn install(app: &AppHandle, item: Item) -> Result<Installed, String> {
    if !valid_part(&item.ns) || !valid_part(&item.name) || !valid_part(&item.version) {
        return Err("This extension has an invalid name".into());
    }
    let base = root(app)?;
    let client = client()?;
    let work = base.join(format!(".work-{}-{}", std::process::id(), item.id));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = async {
        let vsix = work.join("pkg.vsix");
        let got = download(&client, &item.download, &vsix).await?;
        if let Some(url) = &item.sha256 {
            let expected = client.get(trusted(url)?).send().await.map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?;
            let expected = expected.split_whitespace().next().unwrap_or("").to_lowercase();
            if expected.len() == 64 && expected != got {
                return Err("The download is corrupted".to_string());
            }
        }
        let tree = work.join("tree");
        unpack(&vsix, &tree)?;
        if !tree.join("package.json").is_file() {
            return Err("This is not an extension".to_string());
        }
        let meta = Meta { ns: item.ns.clone(), name: item.name.clone(), version: item.version.clone(), display: item.display.clone(), description: item.description.clone() };
        fs::write(tree.join(".assioma.json"), serde_json::to_string(&meta).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let dest = base.join(&item.id);
        let _ = fs::remove_dir_all(&dest);
        fs::rename(&tree, &dest).map_err(|e| e.to_string())?;
        Ok(dest)
    }
    .await;
    let _ = fs::remove_dir_all(&work);
    let dest = result?;
    reload(app);
    describe(&dest).ok_or_else(|| "The extension can't be read".to_string())
}

pub fn remove(app: &AppHandle, id: &str) -> Result<(), String> {
    if !valid_part(id) {
        return Err("Invalid extension".into());
    }
    let dir = root(app)?.join(id);
    if dir.is_dir() {
        fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    reload(app);
    Ok(())
}

fn manifest(dir: &Path) -> Option<Value> {
    let text = fs::read_to_string(dir.join("package.json")).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

fn describe(dir: &Path) -> Option<Installed> {
    let pkg = manifest(dir)?;
    let meta: Meta = fs::read_to_string(dir.join(".assioma.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let ns = if meta.ns.is_empty() { str_of(&pkg, "publisher") } else { meta.ns };
    let name = if meta.name.is_empty() { str_of(&pkg, "name") } else { meta.name };
    let pick = |a: String, b: String| if a.is_empty() || a.starts_with('%') { b } else { a };
    let display = pick(meta.display, pick(str_of(&pkg, "displayName"), name.clone()));
    let c = &pkg["contributes"];
    let themes = c["themes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let path = t["path"].as_str()?.to_string();
            let label = t["label"].as_str().map(String::from).or_else(|| t["id"].as_str().map(String::from))?;
            let ui = t["uiTheme"].as_str().unwrap_or("vs-dark");
            Some(ThemeRef { label, path, light: ui == "vs" || ui == "hc-light" })
        })
        .collect();
    let languages = c["grammars"].as_array().into_iter().flatten().filter_map(|g| g["language"].as_str().map(String::from)).collect::<Vec<_>>();
    let snippets = c["snippets"].as_array().map_or(0, Vec::len);
    let partial = ["main", "browser"].iter().any(|k| pkg.get(*k).is_some_and(|v| v.is_string()));
    Some(Installed {
        id: format!("{ns}.{name}"),
        ns,
        name,
        display,
        description: pick(meta.description, str_of(&pkg, "description")),
        version: pick(meta.version, str_of(&pkg, "version")),
        themes,
        languages,
        snippets,
        partial,
    })
}

fn dirs(app: &AppHandle) -> Vec<PathBuf> {
    let Ok(base) = root(app) else { return vec![] };
    let mut list: Vec<PathBuf> = fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .collect();
    list.sort();
    list
}

pub fn list(app: &AppHandle) -> Vec<Installed> {
    dirs(app).iter().filter_map(|d| describe(d)).collect()
}

static SNIPPETS: std::sync::RwLock<Vec<Snippet>> = std::sync::RwLock::new(Vec::new());

/// Loads the languages and snippets of every installed extension.
pub fn reload(app: &AppHandle) {
    crate::grammars::reload(&dirs(app));
    *SNIPPETS.write().unwrap() = snippets(app);
}

/// The snippets of a language whose prefix starts with what was typed, expanded for a line.
pub fn snippets_for(lang: &str, typed: &str, indent: &str, unit: &str) -> Vec<(String, String, String, Vec<[usize; 2]>)> {
    let typed = typed.to_lowercase();
    SNIPPETS
        .read()
        .unwrap()
        .iter()
        .filter(|s| s.lang == lang && s.prefix.to_lowercase().starts_with(&typed))
        .take(12)
        .map(|s| {
            let (text, stops) = expand(&s.body, indent, unit);
            (s.prefix.clone(), text, s.description.clone(), stops)
        })
        .collect()
}

/// Expands a VS Code snippet into plain text. The second value is where the tab stops are, as
/// character ranges of that text, in the order Tab visits them. `$0` comes last, at the end
/// of the text when the snippet has none.
pub fn expand(body: &str, indent: &str, unit: &str) -> (String, Vec<[usize; 2]>) {
    struct P<'a> {
        c: Vec<char>,
        i: usize,
        out: String,
        len: usize,
        stops: Vec<(u32, usize, usize)>,
        indent: &'a str,
        unit: &'a str,
        line_start: bool,
    }
    impl P<'_> {
        fn push(&mut self, ch: char) {
            if ch == '\n' {
                self.out.push('\n');
                self.out.push_str(self.indent);
                self.len += 1 + self.indent.chars().count();
                self.line_start = true;
            } else if ch == '\t' && self.line_start {
                self.out.push_str(self.unit);
                self.len += self.unit.chars().count();
            } else {
                self.out.push(ch);
                self.len += 1;
                self.line_start = false;
            }
        }
        fn number(&mut self) -> Option<u32> {
            let start = self.i;
            while self.i < self.c.len() && self.c[self.i].is_ascii_digit() {
                self.i += 1;
            }
            (self.i > start).then(|| self.c[start..self.i].iter().collect::<String>().parse().unwrap_or(0))
        }
        fn run(&mut self, depth: u8, in_braces: bool) {
            while self.i < self.c.len() {
                let ch = self.c[self.i];
                if in_braces && ch == '}' {
                    return;
                }
                self.i += 1;
                match ch {
                    '\\' if self.i < self.c.len() && matches!(self.c[self.i], '$' | '}' | '\\') => {
                        let n = self.c[self.i];
                        self.i += 1;
                        self.push(n);
                    }
                    '$' if depth < 8 => self.dollar(depth),
                    _ => self.push(ch),
                }
            }
        }
        fn dollar(&mut self, depth: u8) {
            if let Some(n) = self.number() {
                self.stops.push((n, self.len, self.len));
            } else if self.c.get(self.i) == Some(&'{') {
                self.i += 1;
                if let Some(n) = self.number() {
                    self.stops.push((n, self.len, self.len));
                    let k = self.stops.len() - 1;
                    match self.c.get(self.i) {
                        Some(':') => {
                            self.i += 1;
                            self.run(depth + 1, true);
                            self.stops[k].2 = self.len;
                        }
                        Some('|') => {
                            self.i += 1;
                            let mut first = true;
                            while self.i < self.c.len() && self.c[self.i] != '|' && self.c[self.i] != '}' {
                                let ch = self.c[self.i];
                                self.i += 1;
                                if ch == ',' {
                                    first = false;
                                } else if first {
                                    self.push(ch);
                                }
                            }
                            self.stops[k].2 = self.len;
                            if self.c.get(self.i) == Some(&'|') {
                                self.i += 1;
                            }
                        }
                        _ => {}
                    }
                } else {
                    while self.i < self.c.len() && (self.c[self.i].is_ascii_alphanumeric() || self.c[self.i] == '_') {
                        self.i += 1;
                    }
                    if self.c.get(self.i) == Some(&':') {
                        self.i += 1;
                        self.run(depth + 1, true);
                    }
                }
                while self.i < self.c.len() && self.c[self.i] != '}' {
                    self.i += 1;
                }
                self.i += 1;
            } else {
                while self.i < self.c.len() && (self.c[self.i].is_ascii_alphanumeric() || self.c[self.i] == '_') {
                    self.i += 1;
                }
            }
        }
    }
    let mut p = P { c: body.chars().collect(), i: 0, out: String::new(), len: 0, stops: vec![], indent, unit, line_start: true };
    p.run(0, false);
    let mut order: Vec<[usize; 2]> = vec![];
    let mut seen: Vec<u32> = vec![];
    let mut numbered: Vec<&(u32, usize, usize)> = p.stops.iter().filter(|s| s.0 > 0).collect();
    numbered.sort_by_key(|s| s.0);
    for s in numbered {
        // A tab stop that appears again is a mirror of the first one.
        if !seen.contains(&s.0) {
            seen.push(s.0);
            order.push([s.1, s.2]);
        }
    }
    let last = p.stops.iter().find(|s| s.0 == 0).map_or([p.len, p.len], |s| [s.1, s.2]);
    order.push(last);
    (p.out, order)
}

fn find_dir(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    if !valid_part(id) {
        return Err("Invalid extension".into());
    }
    let dir = root(app)?.join(id);
    dir.is_dir().then_some(dir).ok_or_else(|| "The extension is not installed".to_string())
}

// ---- snippets ----

pub fn snippets(app: &AppHandle) -> Vec<Snippet> {
    let mut out = vec![];
    for dir in dirs(app) {
        let Some(pkg) = manifest(&dir) else { continue };
        for s in pkg["contributes"]["snippets"].as_array().into_iter().flatten() {
            let (Some(lang), Some(path)) = (s["language"].as_str(), s["path"].as_str()) else { continue };
            let Some(file) = crate::grammars::inside(&dir, path) else { continue };
            let Ok(text) = fs::read_to_string(&file) else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&crate::grammars::strip_jsonc(text.trim_start_matches('\u{feff}'))) else { continue };
            for (name, def) in v.as_object().into_iter().flatten() {
                let body = match &def["body"] {
                    Value::String(s) => s.clone(),
                    Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"),
                    _ => continue,
                };
                let prefixes: Vec<String> = match &def["prefix"] {
                    Value::String(s) => vec![s.clone()],
                    Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                    _ => vec![],
                };
                let description = def["description"].as_str().unwrap_or(name).to_string();
                for prefix in prefixes {
                    out.push(Snippet { lang: lang.to_string(), prefix, description: description.clone(), body: body.clone() });
                }
            }
        }
    }
    out
}

// ---- color themes ----

type Rgb = [f64; 3];

fn parse_color(s: &str) -> Option<([f64; 3], f64)> {
    let h = s.trim().strip_prefix('#')?;
    let digits: Vec<u32> = h.chars().map(|c| c.to_digit(16)).collect::<Option<_>>()?;
    let byte = |i: usize| f64::from(digits[i] * 16 + digits[i + 1]);
    let short = |i: usize| f64::from(digits[i] * 17);
    match digits.len() {
        3 => Some(([short(0), short(1), short(2)], 1.0)),
        4 => Some(([short(0), short(1), short(2)], short(3) / 255.0)),
        6 => Some(([byte(0), byte(2), byte(4)], 1.0)),
        8 => Some(([byte(0), byte(2), byte(4)], byte(6) / 255.0)),
        _ => None,
    }
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)
}

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0].round().clamp(0.0, 255.0) as u8, c[1].round().clamp(0.0, 255.0) as u8, c[2].round().clamp(0.0, 255.0) as u8)
}

fn luminance(c: Rgb) -> f64 {
    (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]) / 255.0
}

struct Rule {
    selector: String,
    color: Option<Rgb>,
    order: usize,
}

/// A rule applies to a scope when its selector is that scope or a dotted prefix of it.
fn selector_covers(sel: &str, scope: &str) -> bool {
    scope == sel || (scope.starts_with(sel) && scope.as_bytes().get(sel.len()) == Some(&b'.'))
}

fn best_color(rules: &[Rule], scope: &str) -> Option<Rgb> {
    rules
        .iter()
        .filter(|r| r.color.is_some() && selector_covers(&r.selector, scope))
        .max_by_key(|r| (r.selector.len(), r.order))
        .and_then(|r| r.color)
}

/// Reads a theme file with its `include`s, as VS Code does.
fn load_theme(dir: &Path, rel: &str, depth: u8) -> Result<Value, String> {
    let file = crate::grammars::inside(dir, rel).ok_or("The theme file is not in the extension")?;
    let text = fs::read_to_string(&file).map_err(|e| format!("The theme can't be read: {e}"))?;
    let mut theme: Value = serde_json::from_str(&crate::grammars::strip_jsonc(text.trim_start_matches('\u{feff}'))).map_err(|e| format!("The theme is not valid: {e}"))?;
    if depth < 4 {
        if let Some(inc) = theme["include"].as_str().map(String::from) {
            let parent = Path::new(rel).parent().unwrap_or(Path::new(""));
            let path = parent.join(inc);
            {
                if let Ok(base) = load_theme(dir, &path.to_string_lossy(), depth + 1) {
                    let mut merged = base;
                    for (k, v) in theme["colors"].as_object().into_iter().flatten() {
                        merged["colors"][k] = v.clone();
                    }
                    let mut tokens = merged["tokenColors"].as_array().cloned().unwrap_or_default();
                    tokens.extend(theme["tokenColors"].as_array().cloned().unwrap_or_default());
                    merged["tokenColors"] = Value::Array(tokens);
                    if theme.get("type").is_some() {
                        merged["type"] = theme["type"].clone();
                    }
                    theme = merged;
                }
            }
        }
    }
    Ok(theme)
}

/// Turns a VS Code color theme into the colors of an Assioma theme.
pub fn convert_theme(theme: &Value, light_hint: bool, label: &str) -> Value {
    let light = match theme["type"].as_str() {
        Some("light" | "hc-light") => true,
        Some(_) => false,
        None => light_hint,
    };
    let colors = &theme["colors"];
    let bg0: Rgb = if light { [255.0; 3] } else { [30.0; 3] };
    let color_over = |key: &str, under: Rgb| -> Option<Rgb> {
        let (c, a) = parse_color(colors[key].as_str()?)?;
        Some(mix(under, c, a))
    };
    let bg = color_over("editor.background", bg0).unwrap_or(bg0);
    let first = |keys: &[&str], under: Rgb| keys.iter().find_map(|k| color_over(k, under));
    let global = theme["tokenColors"].as_array().into_iter().flatten().find(|t| t["scope"].is_null()).and_then(|t| parse_color(t["settings"]["foreground"].as_str()?));
    let fg = first(&["editor.foreground", "foreground"], bg).or(global.map(|(c, _)| c)).unwrap_or(if light { [51.0; 3] } else { [212.0; 3] });

    let chrome = first(&["sideBar.background", "activityBar.background", "editorGroupHeader.tabsBackground"], bg).unwrap_or_else(|| mix(bg, fg, 0.04));
    let hover = first(&["list.hoverBackground"], chrome).unwrap_or_else(|| mix(chrome, fg, 0.08));
    let active = first(&["list.activeSelectionBackground", "list.inactiveSelectionBackground"], chrome).unwrap_or_else(|| mix(chrome, fg, 0.16));
    let line = first(&["editorGroup.border", "sideBar.border", "panel.border"], bg).unwrap_or_else(|| mix(bg, fg, 0.10));
    let strong = first(&["contrastBorder", "input.border", "dropdown.border"], bg).unwrap_or_else(|| mix(bg, fg, 0.22));
    let muted = first(&["descriptionForeground"], chrome).unwrap_or_else(|| mix(fg, bg, 0.35));
    let faint = mix(fg, bg, 0.6);
    let accent = first(&["focusBorder", "button.background", "activityBarBadge.background", "textLink.foreground"], bg).unwrap_or(fg);
    let sel = first(&["editor.selectionBackground"], bg).unwrap_or_else(|| mix(bg, accent, 0.3));
    let mark = first(&["editor.findMatchHighlightBackground", "editor.findMatchBackground"], bg).unwrap_or(active);
    let num = first(&["editorLineNumber.foreground"], bg).unwrap_or(faint);
    let guide = first(&["editorIndentGuide.background1", "editorIndentGuide.background"], bg).unwrap_or(line);
    let curline = first(&["editor.lineHighlightBackground"], bg).unwrap_or_else(|| mix(bg, fg, 0.04));
    let caret = first(&["editorCursor.foreground"], bg).unwrap_or(fg);
    let err = first(&["editorError.foreground", "errorForeground"], bg).unwrap_or([241.0, 76.0, 76.0]);
    let warn = first(&["editorWarning.foreground"], bg).unwrap_or([204.0, 167.0, 0.0]);
    let bp = first(&["debugIcon.breakpointForeground"], bg).unwrap_or(err);
    let gadd = first(&["gitDecoration.addedResourceForeground", "editorGutter.addedBackground"], bg).unwrap_or([87.0, 150.0, 92.0]);
    let gmod = first(&["gitDecoration.modifiedResourceForeground", "editorGutter.modifiedBackground"], bg).unwrap_or([74.0, 136.0, 199.0]);

    let mut rules = vec![];
    for (order, t) in theme["tokenColors"].as_array().into_iter().flatten().enumerate() {
        let color = t["settings"]["foreground"].as_str().and_then(parse_color).map(|(c, a)| mix(bg, c, a));
        let scopes: Vec<String> = match &t["scope"] {
            Value::String(s) => s.split(',').map(|x| x.trim().to_string()).collect(),
            Value::Array(a) => a.iter().filter_map(Value::as_str).flat_map(|s| s.split(',').map(|x| x.trim().to_string()).collect::<Vec<_>>()).collect(),
            _ => vec![],
        };
        for s in scopes {
            // For `a b` the rule is about b, and exclusions are not supported.
            let last = s.split_whitespace().last().unwrap_or("").to_string();
            if last.is_empty() || last.starts_with('-') {
                continue;
            }
            rules.push(Rule { selector: last, color, order });
        }
    }
    let pick = |probes: &[&str]| probes.iter().find_map(|p| best_color(&rules, p)).unwrap_or(global.map_or(fg, |(c, _)| c));
    let code = global.map_or(fg, |(c, _)| c);
    let syntax = [
        ("k", pick(&["keyword.control", "keyword", "storage.modifier", "storage"])),
        ("t", pick(&["storage.type.primitive", "support.type.primitive", "storage.type", "support.type"])),
        ("y", pick(&["entity.name.type", "entity.name.class", "support.class", "entity.other.inherited-class", "support.type"])),
        ("f", pick(&["entity.name.function", "support.function", "meta.function-call"])),
        ("v", pick(&["variable.other.property", "variable.other.member", "support.variable.property", "meta.object-literal.key", "variable.other"])),
        ("l", pick(&["variable.parameter", "variable", "variable.other.readwrite"])),
        ("o", pick(&["entity.name.namespace", "entity.name.module", "support.other.namespace", "entity.name.type.module"])),
        ("r", pick(&["string", "string.quoted"])),
        ("x", pick(&["constant.character.escape", "constant.character", "string.regexp"])),
        ("n", pick(&["constant.numeric"])),
        ("z", pick(&["constant.language", "constant"])),
        ("c", pick(&["comment"])),
        ("d", pick(&["meta.preprocessor", "keyword.control.directive", "entity.name.function.preprocessor", "keyword.other.preprocessor"])),
        ("w", pick(&["entity.name.function.preprocessor", "constant.other.macro", "entity.name.function.macro", "constant.other"])),
    ];
    let mut out = serde_json::Map::new();
    let mut put = |k: &str, c: Rgb| {
        out.insert(k.to_string(), Value::String(hex(c)));
    };
    for (k, c) in [
        ("bg", bg), ("chrome", chrome), ("hover", hover), ("active", active), ("line", line), ("line-strong", strong), ("text", fg), ("muted", muted), ("faint", faint),
        ("accent", accent), ("code", code), ("sel", sel), ("mark", mark), ("num", num), ("guide", guide), ("curline", curline), ("caret", caret), ("err", err), ("warn", warn),
        ("bp", bp), ("git-add", gadd), ("git-mod", gmod), ("run", if luminance(bg) > 0.5 { fg } else { [255.0; 3] }), ("glow", if luminance(bg) > 0.5 { [0.0; 3] } else { [255.0; 3] }),
    ] {
        put(k, c);
    }
    for (k, c) in syntax {
        put(k, c);
    }
    serde_json::json!({ "name": label, "light": light, "colors": Value::Object(out) })
}

pub fn theme(app: &AppHandle, id: &str, path: &str, label: &str, light: bool) -> Result<Value, String> {
    let dir = find_dir(app, id)?;
    Ok(convert_theme(&load_theme(&dir, path, 0)?, light, label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn colors_with_alpha_are_blended_over_the_background() {
        let (c, a) = parse_color("#ff000080").unwrap();
        assert_eq!(c, [255.0, 0.0, 0.0]);
        assert!((a - 0.502).abs() < 0.01);
        assert_eq!(parse_color("#fff").unwrap().0, [255.0; 3]);
        assert!(parse_color("red").is_none());
        assert_eq!(hex(mix([0.0; 3], [255.0; 3], 0.5)), "#808080");
    }

    #[test]
    fn a_vs_code_theme_becomes_assioma_colors() {
        let t = json!({
            "type": "dark",
            "colors": { "editor.background": "#282828", "editor.foreground": "#ebdbb2", "sideBar.background": "#1d2021", "editorLineNumber.foreground": "#7c6f64" },
            "tokenColors": [
                { "settings": { "foreground": "#ebdbb2" } },
                { "scope": ["comment", "punctuation.definition.comment"], "settings": { "foreground": "#928374", "fontStyle": "italic" } },
                { "scope": "keyword, storage.type", "settings": { "foreground": "#fb4934" } },
                { "scope": "keyword.operator", "settings": { "foreground": "#8ec07c" } },
                { "scope": "string", "settings": { "foreground": "#b8bb26" } },
                { "scope": "entity.name.function", "settings": { "foreground": "#fabd2f" } },
                { "scope": "meta.function entity.name.function", "settings": { "foreground": "#aabbcc" } }
            ]
        });
        let v = convert_theme(&t, false, "Gruvbox");
        let c = &v["colors"];
        assert_eq!(c["bg"], "#282828");
        assert_eq!(c["chrome"], "#1d2021");
        assert_eq!(c["c"], "#928374");
        assert_eq!(c["k"], "#fb4934");
        assert_eq!(c["r"], "#b8bb26");
        assert_eq!(c["f"], "#aabbcc");
        assert_eq!(c["num"], "#7c6f64");
        assert_eq!(c["code"], "#ebdbb2");
        assert_eq!(v["light"], false);
        for (_, value) in c.as_object().unwrap() {
            let s = value.as_str().unwrap();
            assert!(s.len() == 7 && s.starts_with('#'), "{s}");
        }
    }

    #[test]
    fn snippets_expand_with_tab_stops_and_indentation() {
        let (text, stops) = expand("for (${1:i} = 0; $2; $3) {\n\t$0\n}", "  ", "    ");
        assert_eq!(text, "for (i = 0; ; ) {\n      \n  }");
        // $1 is the `i`, then the empty $2 and $3, and $0 on the new line.
        assert_eq!(stops, vec![[5, 6], [12, 12], [14, 14], [24, 24]]);
        assert_eq!(expand("print($1)", "", "\t"), ("print()".to_string(), vec![[6, 6], [7, 7]]));
        let (text, stops) = expand("a ${2:two} ${1|x,y|} $TM_FILENAME \\$x", "", "\t");
        assert_eq!(text, "a two x  $x");
        assert_eq!(stops, vec![[6, 7], [2, 5], [11, 11]]);
        assert_eq!(expand("plain", "", "\t"), ("plain".to_string(), vec![[5, 5]]));
        let (text, stops) = expand("${1:a ${2:b}}c$0", "", "\t");
        assert_eq!(text, "a bc");
        assert_eq!(stops, vec![[0, 3], [2, 3], [4, 4]]);
        // A repeated tab stop is a mirror and is visited once.
        assert_eq!(expand("$1 and $1", "", "\t").1, vec![[0, 0], [5, 5]]);
    }

    #[test]
    fn selectors_cover_only_dotted_prefixes() {
        assert!(selector_covers("keyword", "keyword.control.python"));
        assert!(selector_covers("keyword.control", "keyword.control"));
        assert!(!selector_covers("key", "keyword.control"));
    }

    #[test]
    fn names_and_hosts_are_checked() {
        assert!(valid_part("jdinhlife"));
        assert!(!valid_part("../x"));
        assert!(!valid_part(".hidden"));
        assert!(trusted("https://open-vsx.org/api/a/b/1/file/x.vsix").is_ok());
        assert!(trusted("https://evil.example/x.vsix").is_err());
        assert!(trusted("http://open-vsx.org/x").is_err());
        assert!(trusted("https://open-vsx.org.evil.example/x").is_err());
    }

    #[test]
    fn a_vsix_is_unpacked_without_escaping() {
        use zip::write::SimpleFileOptions;
        let dir = std::env::temp_dir().join(format!("assioma-ext-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let pkg = dir.join("a.vsix");
        {
            let mut z = zip::ZipWriter::new(File::create(&pkg).unwrap());
            let o = SimpleFileOptions::default();
            z.start_file("extension/package.json", o).unwrap();
            z.write_all(b"{\"name\":\"x\"}").unwrap();
            z.start_file("extension/themes/a.json", o).unwrap();
            z.write_all(b"{}").unwrap();
            z.start_file("extension/../../escape.txt", o).unwrap();
            z.write_all(b"no").unwrap();
            z.start_file("other/file.txt", o).unwrap();
            z.write_all(b"no").unwrap();
            z.finish().unwrap();
        }
        let out = dir.join("out");
        unpack(&pkg, &out).unwrap();
        assert!(out.join("package.json").is_file());
        assert!(out.join("themes/a.json").is_file());
        assert!(!dir.join("escape.txt").exists());
        assert!(!out.join("other").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
