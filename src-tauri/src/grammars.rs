//! The languages Assioma knows: the ones built in, taken from VS Code, and the ones installed
//! from extensions. Each language has a TextMate grammar and, when there is one, comment markers.

use crate::bundle;
use crate::tm::{self, Grammar, Lookup, States};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

enum Source {
    Bundled(&'static [u8]),
    File(PathBuf),
}

struct Entry {
    #[cfg_attr(not(test), allow(dead_code))]
    scope: String,
    source: Source,
    cell: OnceLock<Option<Arc<Grammar>>>,
}

pub struct LangInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub grammar: usize,
    pub line_comment: Option<&'static str>,
    pub block_comment: Option<(&'static str, &'static str)>,
}

pub struct Registry {
    entries: Vec<Entry>,
    by_scope: HashMap<String, usize>,
    langs: Vec<LangInfo>,
    by_ext: HashMap<String, u16>,
    by_name: HashMap<String, u16>,
    states: Mutex<States>,
}

fn leak(s: &str) -> &'static str {
    static POOL: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut pool = POOL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(hit) = pool.iter().find(|x| **x == s) {
        return hit;
    }
    let made: &'static str = Box::leak(s.to_string().into_boxed_str());
    pool.push(made);
    made
}

fn gunzip(bytes: &[u8]) -> Option<String> {
    let mut out = String::new();
    flate2::read::GzDecoder::new(bytes).read_to_string(&mut out).ok()?;
    Some(out)
}

pub fn strip_jsonc(text: &str) -> String {
    let b: Vec<char> = text.chars().collect();
    let (mut out, mut i, mut in_str) = (String::new(), 0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 1;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        } else {
            out.push(c);
        }
        i += 1;
    }
    // trailing commas
    let mut cleaned = String::with_capacity(out.len());
    let chars: Vec<char> = out.chars().collect();
    for (k, c) in chars.iter().enumerate() {
        if *c == ',' {
            let next = chars[k + 1..].iter().find(|x| !x.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                continue;
            }
        }
        cleaned.push(*c);
    }
    cleaned
}

fn plist_to_json(v: plist::Value) -> Value {
    match v {
        plist::Value::Dictionary(d) => Value::Object(d.into_iter().map(|(k, v)| (k, plist_to_json(v))).collect()),
        plist::Value::Array(a) => Value::Array(a.into_iter().map(plist_to_json).collect()),
        plist::Value::String(s) => Value::String(s),
        plist::Value::Boolean(b) => Value::Bool(b),
        plist::Value::Integer(i) => i.as_signed().map_or(Value::Null, |n| Value::Number(n.into())),
        plist::Value::Real(r) => serde_json::Number::from_f64(r).map_or(Value::Null, Value::Number),
        _ => Value::Null,
    }
}

fn read_grammar(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.first().is_some_and(|b| *b == b'{' || b.is_ascii_whitespace()) && std::str::from_utf8(&bytes).ok()?.trim_start().starts_with('{') {
        return String::from_utf8(bytes).ok();
    }
    let value = plist::Value::from_reader(std::io::Cursor::new(bytes)).ok()?;
    serde_json::to_string(&plist_to_json(value)).ok()
}

/// The file `rel` of an extension, if it really is inside the extension's folder.
pub fn inside(dir: &Path, rel: &str) -> Option<PathBuf> {
    let base = dir.canonicalize().ok()?;
    let file = dir.join(rel).canonicalize().ok()?;
    file.starts_with(&base).then_some(file)
}

type Comments = (Option<&'static str>, Option<(&'static str, &'static str)>);

fn block_pair(v: &Value) -> Option<(&'static str, &'static str)> {
    let a = v.as_array()?;
    Some((leak(a.first()?.as_str()?), leak(a.get(1)?.as_str()?)))
}

fn comment_of(cfg: &Path) -> Comments {
    let Ok(text) = std::fs::read_to_string(cfg) else { return (None, None) };
    let Ok(v) = serde_json::from_str::<Value>(&strip_jsonc(&text)) else { return (None, None) };
    let line = v["comments"]["lineComment"].as_str().map(|s| leak(s.trim_end()));
    let block = block_pair(&v["comments"]["blockComment"]);
    (line, block)
}

impl Registry {
    fn empty() -> Registry {
        Registry { entries: vec![], by_scope: HashMap::new(), langs: vec![], by_ext: HashMap::new(), by_name: HashMap::new(), states: Mutex::new(States::new()) }
    }

    fn add_grammar(&mut self, scope: &str, source: Source) -> usize {
        let entry = Entry { scope: scope.to_string(), source, cell: OnceLock::new() };
        self.entries.push(entry);
        let id = self.entries.len() - 1;
        self.by_scope.insert(scope.to_string(), id);
        id
    }

    #[allow(clippy::too_many_arguments)]
    fn add_language(&mut self, id: &str, scope: &str, exts: &[String], names: &[String], aliases: &[String], comment: Comments) {
        let Some(&g) = self.by_scope.get(scope) else { return };
        let idx = self.langs.len() as u16;
        let label = aliases.first().map_or(id, String::as_str);
        self.langs.push(LangInfo { id: leak(id), label: leak(label), grammar: g, line_comment: comment.0, block_comment: comment.1 });
        for e in exts {
            self.by_ext.insert(e.to_lowercase(), idx);
        }
        for n in names {
            self.by_name.insert(n.to_string(), idx);
        }
        for a in aliases.iter().chain(std::iter::once(&id.to_string())) {
            self.by_name.insert(format!("lang:{}", a.to_lowercase()), idx);
        }
    }

    pub fn bundled() -> Registry {
        let mut r = Registry::empty();
        for (scope, bytes) in bundle::GRAMMARS {
            r.add_grammar(scope, Source::Bundled(bytes));
        }
        let index: Value = serde_json::from_str(bundle::INDEX).unwrap_or(Value::Null);
        let strings = |v: &Value| -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
        for l in index["languages"].as_array().into_iter().flatten() {
            let line = (l["comments"]["line"].as_str().map(|s| leak(s.trim_end())), block_pair(&l["comments"]["block"]));
            r.add_language(l["id"].as_str().unwrap_or(""), l["scope"].as_str().unwrap_or(""), &strings(&l["extensions"]), &strings(&l["filenames"]), &strings(&l["aliases"]), line);
        }
        r
    }

    /// Adds the languages and grammars of an installed VS Code extension.
    pub fn add_extension(&mut self, dir: &Path) {
        let Ok(text) = std::fs::read_to_string(dir.join("package.json")) else { return };
        let Ok(pkg) = serde_json::from_str::<Value>(&text) else { return };
        let strings = |v: &Value| -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
        let mut lang_scope: HashMap<String, String> = HashMap::new();
        for g in pkg["contributes"]["grammars"].as_array().into_iter().flatten() {
            let (Some(scope), Some(path)) = (g["scopeName"].as_str(), g["path"].as_str()) else { continue };
            let Some(file) = inside(dir, path).filter(|f| f.is_file()) else { continue };
            self.add_grammar(scope, Source::File(file));
            if let Some(l) = g["language"].as_str() {
                lang_scope.entry(l.to_string()).or_insert_with(|| scope.to_string());
            }
        }
        for l in pkg["contributes"]["languages"].as_array().into_iter().flatten() {
            let Some(id) = l["id"].as_str() else { continue };
            let Some(scope) = lang_scope.get(id) else { continue };
            let comment = l["configuration"].as_str().and_then(|c| Some(comment_of(&inside(dir, c)?))).unwrap_or((None, None));
            self.add_language(id, scope, &strings(&l["extensions"]), &strings(&l["filenames"]), &strings(&l["aliases"]), comment);
        }
    }

    pub fn lang_for_path(&self, path: &Path) -> Option<u16> {
        let name = path.file_name()?.to_str()?;
        if let Some(i) = self.by_name.get(name) {
            return Some(*i);
        }
        let lower = name.to_lowercase();
        // The longest extension wins, so `.hpp.in` beats `.in`.
        let mut dots: Vec<usize> = lower.match_indices('.').map(|(i, _)| i).collect();
        dots.sort_unstable();
        dots.iter().find_map(|&i| self.by_ext.get(&lower[i..]).copied())
    }
}

impl Lookup for Registry {
    fn grammar(&self, id: usize) -> Option<Arc<Grammar>> {
        let e = self.entries.get(id)?;
        e.cell
            .get_or_init(|| {
                let json = match &e.source {
                    Source::Bundled(b) => gunzip(b)?,
                    Source::File(p) => read_grammar(p)?,
                };
                Grammar::parse(&json).ok().map(Arc::new)
            })
            .clone()
    }

    fn by_scope(&self, scope: &str) -> Option<usize> {
        self.by_scope.get(scope).copied()
    }
}

static REGISTRY: OnceLock<RwLock<Registry>> = OnceLock::new();

fn registry() -> &'static RwLock<Registry> {
    REGISTRY.get_or_init(|| RwLock::new(Registry::bundled()))
}

/// Starts again from the built-in languages plus the extensions in these folders.
pub fn reload(extension_dirs: &[PathBuf]) {
    let mut r = Registry::bundled();
    for d in extension_dirs {
        r.add_extension(d);
    }
    *registry().write().unwrap_or_else(std::sync::PoisonError::into_inner) = r;
}

pub fn lang_for_path(path: &Path) -> Option<u16> {
    registry().read().unwrap_or_else(std::sync::PoisonError::into_inner).lang_for_path(path)
}

pub fn lang_id(lang: u16) -> Option<&'static str> {
    registry().read().unwrap_or_else(std::sync::PoisonError::into_inner).langs.get(lang as usize).map(|l| l.id)
}

pub fn label(lang: u16) -> Option<&'static str> {
    registry().read().unwrap_or_else(std::sync::PoisonError::into_inner).langs.get(lang as usize).map(|l| l.label)
}

pub fn lang_by_id(id: &str) -> Option<u16> {
    registry().read().unwrap_or_else(std::sync::PoisonError::into_inner).langs.iter().position(|l| l.id == id).map(|i| i as u16)
}

/// Every language of the registry: its id and the name to show.
pub fn languages() -> Vec<(&'static str, &'static str)> {
    registry().read().unwrap_or_else(std::sync::PoisonError::into_inner).langs.iter().map(|l| (l.id, l.label)).collect()
}

pub fn block_comment(lang: u16) -> Option<(&'static str, &'static str)> {
    registry().read().unwrap_or_else(std::sync::PoisonError::into_inner).langs.get(lang as usize).and_then(|l| l.block_comment)
}

pub fn line_comment(lang: u16) -> Option<&'static str> {
    registry().read().unwrap_or_else(std::sync::PoisonError::into_inner).langs.get(lang as usize).and_then(|l| l.line_comment)
}

pub fn tokenize(lang: u16, line: &str, state: u32, out: &mut Vec<crate::lexer::Tok>) -> u32 {
    let reg = registry().read().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(info) = reg.langs.get(lang as usize) else { return 0 };
    let start = out.len();
    // A grammar that trips the engine must never take the editor down with it.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tm::tokenize(&*reg, &reg.states, info.grammar, line, state, out))).unwrap_or_else(|_| {
        out.truncate(start);
        0
    })
}

pub fn in_block_comment(lang: u16, state: u32) -> bool {
    let reg = registry().read().unwrap_or_else(std::sync::PoisonError::into_inner);
    reg.langs.get(lang as usize).is_some() && tm::in_block_comment(&*reg, &reg.states, state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(lang: &str, lines: &[&str]) -> Vec<Vec<(String, u8)>> {
        let l = lang_for_path(Path::new(lang)).unwrap_or_else(|| panic!("no language for {lang}"));
        let mut st = 0;
        lines
            .iter()
            .map(|line| {
                let mut out = vec![];
                st = tokenize(l, line, st, &mut out);
                out.iter().map(|t| (line[t.start as usize..t.end as usize].to_string(), t.tag)).collect()
            })
            .collect()
    }

    #[test]
    fn every_built_in_grammar_loads() {
        let reg = Registry::bundled();
        let broken: Vec<&str> = reg.entries.iter().filter(|e| reg.grammar(reg.by_scope[&e.scope]).is_none()).map(|e| e.scope.as_str()).collect();
        assert!(broken.is_empty(), "{broken:?}");
    }

    #[test]
    fn files_are_matched_by_extension_and_name() {
        for (file, id) in [("a.py", "python"), ("a.RS", "rust"), ("x.hpp", "cpp"), ("main.go", "go"), ("Dockerfile", "dockerfile"), ("Makefile", "makefile"), ("a.tsx", "typescriptreact"), ("a.json", "json"), (".env", "dotenv"), ("s.sh", "shellscript")] {
            let l = lang_for_path(Path::new(file)).unwrap_or_else(|| panic!("{file}"));
            assert_eq!(lang_id(l), Some(id), "{file}");
        }
        assert!(lang_for_path(Path::new("notes.unknown")).is_none());
    }

    #[test]
    fn python_is_colored_like_vs_code() {
        use crate::lexer::*;
        let got = color("a.py", &["@decorator", "def greet(name: str) -> None:", "    \"\"\"Say hi\"\"\"", "    print(f\"hello {name}\", 42, True)  # note"]);
        assert!(got[0].iter().any(|(_, t)| *t == DIRECTIVE), "{:?}", got[0]);
        assert!(got[1].contains(&("def".into(), KEYWORD)), "{:?}", got[1]);
        assert!(got[1].contains(&("greet".into(), FUNCTION)), "{:?}", got[1]);
        assert!(got[2].iter().any(|(_, t)| *t == STRING), "{:?}", got[2]);
        assert!(got[3].contains(&("42".into(), NUMBER)), "{:?}", got[3]);
        assert!(got[3].contains(&("True".into(), CONSTANT)), "{:?}", got[3]);
        assert!(got[3].iter().any(|(s, t)| s.starts_with("# note") && *t == COMMENT), "{:?}", got[3]);
    }

    #[test]
    fn typescript_c_and_html_work_across_lines() {
        use crate::lexer::*;
        let ts = color("a.ts", &["const x: number = 1; // c", "/* open", "still */ let y = `t${x}`;"]);
        assert!(ts[0].contains(&("const".into(), KEYWORD)), "{:?}", ts[0]);
        assert!(ts[0].contains(&("number".into(), TYPE)), "{:?}", ts[0]);
        assert!(ts[1].iter().all(|(_, t)| *t == COMMENT), "{:?}", ts[1]);
        assert!(ts[2].iter().any(|(s, t)| s.starts_with("still") && *t == COMMENT), "{:?}", ts[2]);
        let c = color("a.c", &["#include <stdio.h>", "int main(void) { return 0; }"]);
        assert!(c[0].iter().any(|(_, t)| *t == DIRECTIVE), "{:?}", c[0]);
        assert!(c[1].contains(&("int".into(), TYPE)), "{:?}", c[1]);
        assert!(c[1].contains(&("return".into(), KEYWORD)), "{:?}", c[1]);
        let html = color("a.html", &["<div class=\"a\">hi</div>"]);
        assert!(html[0].contains(&("div".into(), KEYWORD)), "{:?}", html[0]);
        assert!(html[0].contains(&("class".into(), MEMBER)), "{:?}", html[0]);
    }

    #[test]
    fn a_big_file_is_colored_quickly() {
        let line = "int value = compute(a, b) + 42; // comment with text";
        let l = lang_for_path(Path::new("a.cpp")).unwrap();
        let started = std::time::Instant::now();
        let mut st = 0;
        for _ in 0..3000 {
            let mut out = vec![];
            st = tokenize(l, line, st, &mut out);
        }
        let ms = started.elapsed().as_millis();
        assert!(ms < 20_000, "3000 lines took {ms} ms");
    }

    #[test]
    fn extension_files_must_stay_inside_their_folder() {
        let base = std::env::temp_dir().join(format!("assioma-inside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("ext/themes")).unwrap();
        std::fs::write(base.join("ext/themes/a.json"), "{}").unwrap();
        std::fs::write(base.join("secret.json"), "{}").unwrap();
        let ext = base.join("ext");
        assert!(inside(&ext, "themes/a.json").is_some());
        assert!(inside(&ext, "./themes/../themes/a.json").is_some());
        assert!(inside(&ext, "../secret.json").is_none());
        assert!(inside(&ext, "themes/missing.json").is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn comments_in_json_with_comments_and_trailing_commas_are_read() {
        let v: Value = serde_json::from_str(&strip_jsonc("{ /* a */ \"x\": [1, 2,], // c\n \"u\": \"//not\" }")).unwrap();
        assert_eq!(v["x"][1], 2);
        assert_eq!(v["u"], "//not");
    }
}


