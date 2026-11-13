//! A TextMate grammar engine, the format VS Code uses to color code.
//!
//! Grammars are the JSON files of VS Code and of the extensions people install. Every scope
//! name is turned into one of Assioma's token colors when the grammar is loaded, so coloring a
//! line never touches a string.

use crate::lexer::{
    Tok, COMMENT, CONSTANT, DIRECTIVE, ESCAPE, FUNCTION, KEYWORD, MACRO, MEMBER, NAMESPACE,
    NUMBER, STRING, TYPE, USER_TYPE, VARIABLE,
};
use onig::{MatchParam, Regex, RegexOptions, Region, SearchOptions, Syntax};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

const MAX_LINE: usize = 4000;
const MAX_STEPS: usize = 4000;

/// What a scope name means for the color of the text it covers.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sc {
    /// No opinion: the scope around it decides.
    Skip,
    /// Explicitly uncolored.
    Plain,
    Tag(u8),
    /// An operator, which is only colored when it is a word such as `new` or `typeof`.
    Op,
}

pub fn scope_class(n: &str) -> Sc {
    let t = Sc::Tag;
    let has = |p: &str| n.starts_with(p);
    // Code of another language inside this one has its own colors, and what it leaves
    // uncolored must not take the color of the block around it.
    if has("meta.embedded") {
        return Sc::Plain;
    }
    if has("comment") {
        return t(COMMENT);
    }
    if has("string.regexp") {
        return t(ESCAPE);
    }
    if has("string") || has("markup.inline.raw") || has("markup.fenced_code") || has("markup.raw") || has("markup.underline.link") {
        return t(STRING);
    }
    if has("constant.numeric") || has("constant.other.color") {
        return t(NUMBER);
    }
    if has("constant.character") {
        return t(ESCAPE);
    }
    if has("constant.language") || has("constant.other.symbol") || has("support.constant") || has("variable.other.constant") || has("variable.other.enummember") {
        return t(CONSTANT);
    }
    if has("constant.other") {
        return t(MACRO);
    }
    if has("constant") {
        return t(CONSTANT);
    }
    if n.contains("directive") || has("meta.preprocessor") || has("keyword.other.preprocessor") || has("entity.name.function.preprocessor") || has("meta.diff.header") || has("meta.diff.range") || has("markup.list") {
        return t(DIRECTIVE);
    }
    if has("meta.decorator") || has("entity.name.function.decorator") || has("punctuation.decorator") || has("meta.annotation") || has("storage.type.annotation") || has("punctuation.definition.annotation") || has("meta.function.decorator") {
        return t(DIRECTIVE);
    }
    if n == "keyword.operator" || has("keyword.operator.") {
        return Sc::Op;
    }
    if has("keyword") {
        return t(KEYWORD);
    }
    if has("storage.type") {
        const KEYWORDY: [&str; 14] = [
            ".function", ".class", ".struct", ".enum", ".interface", ".namespace", ".module",
            ".trait", ".impl", ".union", ".type.type", ".def", ".delegate", ".extension",
        ];
        const TYPEY: [&str; 7] = [".numeric", ".string", ".boolean", ".primitive", ".core", "built", ".void"];
        if KEYWORDY.iter().any(|k| n.contains(k)) {
            return t(KEYWORD);
        }
        if TYPEY.iter().any(|k| n.contains(k)) {
            return t(TYPE);
        }
        const C_LIKE: [&str; 8] = [".c", ".cpp", ".cs", ".java", ".objc", ".swift", ".dart", ".kotlin"];
        return t(if C_LIKE.iter().any(|s| n.ends_with(s)) { TYPE } else { KEYWORD });
    }
    if has("storage") {
        return t(KEYWORD);
    }
    if has("entity.name.function") || has("support.function") || has("entity.name.method") {
        return t(FUNCTION);
    }
    if has("entity.name.namespace") || has("entity.name.module") || has("entity.name.package") || has("support.other.namespace") || has("support.module") {
        return t(NAMESPACE);
    }
    if has("support.type.property-name") || has("entity.other.attribute-name") || has("variable.other.property") || has("variable.other.member") || has("variable.object.property") || has("support.variable.property") || has("meta.object-literal.key") {
        return t(MEMBER);
    }
    if has("entity.name.tag") {
        return t(if n.ends_with(".yaml") { MEMBER } else { KEYWORD });
    }
    if has("support.type.primitive") || has("support.type.builtin") {
        return t(TYPE);
    }
    if has("entity.name.type") || has("entity.name.class") || has("entity.name.struct") || has("entity.name.interface") || has("entity.name.enum") || has("entity.name.union") || has("entity.other.inherited-class") || has("support.class") || has("support.type") {
        return t(USER_TYPE);
    }
    if has("entity.name.section") || has("markup.heading") || has("markup.bold") || has("markup.inserted") {
        return t(KEYWORD);
    }
    if has("markup.quote") || has("markup.deleted") {
        return t(COMMENT);
    }
    if has("variable.language") {
        return t(KEYWORD);
    }
    if has("variable") || has("support.variable") {
        return t(VARIABLE);
    }
    if has("invalid") {
        return Sc::Plain;
    }
    Sc::Skip
}

fn tag_of(scopes: &[Sc], text: &str) -> u8 {
    for sc in scopes.iter().rev() {
        match sc {
            Sc::Skip => continue,
            Sc::Plain => return 0,
            Sc::Tag(t) => return *t,
            Sc::Op => return if !text.is_empty() && text.chars().all(char::is_alphabetic) { KEYWORD } else { 0 },
        }
    }
    0
}

struct Pat {
    src: String,
    re: OnceLock<Option<Regex>>,
    /// The same pattern where `\G` can never match, for when the search is not at the anchor.
    no_g: OnceLock<Option<Regex>>,
    has_g: bool,
}

impl Pat {
    fn new(src: &str) -> Pat {
        Pat {
            src: src.to_string(),
            re: OnceLock::new(),
            no_g: OnceLock::new(),
            has_g: has_anchor(src),
        }
    }

    fn regex(&self, at_anchor: bool) -> Option<&Regex> {
        if self.has_g && !at_anchor {
            return self.no_g.get_or_init(|| compile(&self.src, false)).as_ref();
        }
        self.re.get_or_init(|| compile(&self.src, true)).as_ref()
    }
}

/// Whether the pattern uses `\G`, the end of the previous match.
fn has_anchor(src: &str) -> bool {
    let mut chars = src.chars();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.next() == Some('G') {
            return true;
        }
    }
    false
}

fn compile(src: &str, anchor_ok: bool) -> Option<Regex> {
    let text = if !anchor_ok && has_anchor(src) {
        let mut out = String::with_capacity(src.len());
        let mut chars = src.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('G') => out.push_str("(?!)"),
                    Some(n) => {
                        out.push(c);
                        out.push(n);
                    }
                    None => out.push(c),
                }
            } else {
                out.push(c);
            }
        }
        std::borrow::Cow::Owned(out)
    } else {
        std::borrow::Cow::Borrowed(src)
    };
    Regex::with_options(&text, RegexOptions::REGEX_OPTION_CAPTURE_GROUP, Syntax::ruby()).ok()
}

struct Cap {
    sc: Sc,
    patterns: Vec<usize>,
}

type Caps = Vec<(usize, Cap)>;

enum Kind {
    Match { re: Pat, caps: Caps },
    Begin { begin: Pat, end: String, begin_caps: Caps, end_caps: Caps, patterns: Vec<usize>, apply_end_last: bool },
    While { begin: Pat, cond: String, begin_caps: Caps, patterns: Vec<usize> },
    Include(String),
    Group(Vec<usize>),
}

struct Rule {
    name: Sc,
    content: Sc,
    /// A block comment, which the editor can fold.
    block: bool,
    kind: Kind,
}

/// The (grammar, rule) pairs a rule can start with, includes followed.
type Patterns = Arc<Vec<(usize, usize)>>;

pub struct Grammar {
    #[cfg_attr(not(test), allow(dead_code))]
    pub scope: String,
    rules: Vec<Rule>,
    root: usize,
    repo: HashMap<String, usize>,
    expanded: Mutex<HashMap<usize, Patterns>>,
    ends: Mutex<HashMap<String, Option<Arc<Regex>>>>,
}

/// Where grammars come from when one includes another by scope name.
pub trait Lookup {
    fn grammar(&self, id: usize) -> Option<Arc<Grammar>>;
    fn by_scope(&self, scope: &str) -> Option<usize>;
}

fn name_class(v: &Value, key: &str) -> Sc {
    v.get(key).and_then(Value::as_str).map_or(Sc::Skip, scope_class)
}

struct Builder {
    rules: Vec<Rule>,
}

impl Builder {
    fn push(&mut self, rule: Rule) -> usize {
        self.rules.push(rule);
        self.rules.len() - 1
    }

    fn list(&mut self, v: Option<&Value>) -> Vec<usize> {
        v.and_then(Value::as_array)
            .map(|a| a.iter().map(|p| self.rule(p)).collect())
            .unwrap_or_default()
    }

    fn caps(&mut self, v: Option<&Value>) -> Caps {
        let Some(map) = v.and_then(Value::as_object) else {
            return vec![];
        };
        map.iter()
            .filter_map(|(k, c)| {
                let idx = k.parse::<usize>().ok()?;
                let patterns = self.list(c.get("patterns"));
                Some((idx, Cap { sc: name_class(c, "name"), patterns }))
            })
            .collect()
    }

    fn rule(&mut self, v: &Value) -> usize {
        let name = name_class(v, "name");
        let content = name_class(v, "contentName");
        let kind = if let Some(inc) = v.get("include").and_then(Value::as_str) {
            Kind::Include(inc.to_string())
        } else if let Some(m) = v.get("match").and_then(Value::as_str) {
            Kind::Match { re: Pat::new(m), caps: self.caps(v.get("captures")) }
        } else if let Some(b) = v.get("begin").and_then(Value::as_str) {
            let begin_caps = self.caps(v.get("beginCaptures").or_else(|| v.get("captures")));
            let patterns = self.list(v.get("patterns"));
            if let Some(w) = v.get("while").and_then(Value::as_str) {
                Kind::While { begin: Pat::new(b), cond: w.to_string(), begin_caps, patterns }
            } else {
                Kind::Begin {
                    begin: Pat::new(b),
                    end: v.get("end").and_then(Value::as_str).unwrap_or("(?!)").to_string(),
                    begin_caps,
                    end_caps: self.caps(v.get("endCaptures").or_else(|| v.get("captures"))),
                    patterns,
                    apply_end_last: v.get("applyEndPatternLast").is_some_and(|x| x == 1 || x == true),
                }
            }
        } else {
            Kind::Group(self.list(v.get("patterns")))
        };
        let block = ["name", "contentName"].iter().any(|k| v.get(*k).and_then(Value::as_str).is_some_and(|n| n.starts_with("comment.block")));
        self.push(Rule { name, content, block, kind })
    }
}

impl Grammar {
    pub fn parse(json: &str) -> Result<Grammar, String> {
        let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let scope = v.get("scopeName").and_then(Value::as_str).ok_or("The grammar has no scopeName")?.to_string();
        let mut b = Builder { rules: vec![] };
        let root_patterns = b.list(v.get("patterns"));
        let root = b.push(Rule { name: Sc::Skip, content: Sc::Skip, block: false, kind: Kind::Group(root_patterns) });
        let mut repo = HashMap::new();
        if let Some(r) = v.get("repository").and_then(Value::as_object) {
            for (k, item) in r {
                let id = b.rule(item);
                repo.insert(k.clone(), id);
            }
        }
        Ok(Grammar { scope, rules: b.rules, root, repo, expanded: Mutex::new(HashMap::new()), ends: Mutex::new(HashMap::new()) })
    }

    fn end_regex(&self, src: &str, at_anchor: bool) -> Option<Arc<Regex>> {
        let mut cache = self.ends.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache.len() > 256 {
            cache.clear();
        }
        let key = format!("{}{src}", if at_anchor { '1' } else { '0' });
        cache.entry(key).or_insert_with(|| compile(src, at_anchor).map(Arc::new)).clone()
    }
}

/// Everything a pattern list can stand for once includes are followed.
fn expand(lk: &dyn Lookup, g: usize, list: &[usize], out: &mut Vec<(usize, usize)>, seen: &mut HashSet<(usize, usize)>) {
    let Some(gr) = lk.grammar(g) else { return };
    for &r in list {
        if !seen.insert((g, r)) {
            continue;
        }
        match &gr.rules[r].kind {
            Kind::Include(target) => {
                if let Some((g2, r2)) = resolve(lk, g, &gr, target) {
                    expand(lk, g2, &[r2], out, seen);
                }
            }
            Kind::Group(items) => expand(lk, g, items, out, seen),
            _ => out.push((g, r)),
        }
    }
}

fn resolve(lk: &dyn Lookup, g: usize, gr: &Grammar, target: &str) -> Option<(usize, usize)> {
    if target == "$self" || target == "$base" {
        return Some((g, gr.root));
    }
    if let Some(name) = target.strip_prefix('#') {
        return gr.repo.get(name).map(|r| (g, *r));
    }
    let (scope, name) = match target.split_once('#') {
        Some((s, n)) => (s, Some(n)),
        None => (target, None),
    };
    let g2 = lk.by_scope(scope)?;
    let other = lk.grammar(g2)?;
    match name {
        Some(n) => other.repo.get(n).map(|r| (g2, *r)),
        None => Some((g2, other.root)),
    }
}

fn patterns_of(lk: &dyn Lookup, g: usize, rule: usize) -> Arc<Vec<(usize, usize)>> {
    let Some(gr) = lk.grammar(g) else { return Arc::new(vec![]) };
    if let Some(hit) = gr.expanded.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&rule) {
        return hit.clone();
    }
    let list: Vec<usize> = match &gr.rules[rule].kind {
        Kind::Begin { patterns, .. } | Kind::While { patterns, .. } | Kind::Group(patterns) => patterns.clone(),
        _ => vec![],
    };
    let mut out = vec![];
    expand(lk, g, &list, &mut out, &mut HashSet::new());
    let out = Arc::new(out);
    gr.expanded.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(rule, out.clone());
    out
}

/// One open `begin`/`end` rule. It is the whole state that carries from one line to the next.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Frame {
    pub g: usize,
    pub rule: usize,
    pub end: Option<String>,
}

struct Sink<'a> {
    out: &'a mut Vec<Tok>,
    /// The length of the line, without the newline that is added for matching.
    limit: usize,
}

impl Sink<'_> {
    fn emit(&mut self, text: &str, from: usize, to: usize, scopes: &[Sc]) {
        let to = to.min(self.limit);
        if to <= from || to > text.len() || !text.is_char_boundary(from) || !text.is_char_boundary(to) {
            return;
        }
        let tag = tag_of(scopes, &text[from..to]);
        if tag == 0 {
            return;
        }
        if let Some(last) = self.out.last_mut() {
            if last.tag == tag && last.end as usize == from {
                last.end = to as u32;
                return;
            }
        }
        self.out.push(Tok { start: from as u32, end: to as u32, tag });
    }
}

fn scopes_of(lk: &dyn Lookup, frames: &[Frame], with_content: bool) -> Vec<Sc> {
    let mut v = vec![];
    for (i, f) in frames.iter().enumerate() {
        if let Some(gr) = lk.grammar(f.g) {
            v.push(gr.rules[f.rule].name);
            if with_content || i + 1 < frames.len() {
                v.push(gr.rules[f.rule].content);
            }
        }
    }
    v
}

/// A pattern that needs more steps than this is given up on, so that a bad grammar cannot freeze the editor.
const RETRY_LIMIT: u32 = 200_000;

fn run(re: &Regex, text: &str, from: usize, to: usize, region: &mut Region) -> Option<usize> {
    let mut param = MatchParam::default();
    param.set_retry_limit_in_match(RETRY_LIMIT);
    re.search_with_param(text, from, to, SearchOptions::SEARCH_OPTION_NONE, Some(region), param).ok().flatten()
}

fn backrefs(end: &str, text: &str, region: &Region) -> String {
    if !end.contains('\\') {
        return end.to_string();
    }
    let mut out = String::with_capacity(end.len());
    let mut chars = end.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(d) = chars.peek().and_then(|x| x.to_digit(10)) {
                chars.next();
                if let Some((s, e)) = region.pos(d as usize) {
                    for ch in text[s..e].chars() {
                        if "\\|()[]{}^$*+?.-".contains(ch) {
                            out.push('\\');
                        }
                        out.push(ch);
                    }
                }
                continue;
            }
            out.push(c);
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

struct Hit {
    start: usize,
    region: Region,
    what: What,
}

enum What {
    End,
    Rule(usize, usize),
}

/// Colors the text of one match and of its captures.
#[allow(clippy::too_many_arguments)]
fn captured(lk: &dyn Lookup, g: usize, text: &str, region: &Region, caps: &Caps, scopes: &mut Vec<Sc>, sink: &mut Sink, depth: usize) {
    let Some((ms, me)) = region.pos(0) else { return };
    let mut ranges: Vec<(usize, usize, &Cap)> = caps
        .iter()
        .filter(|(i, c)| *i > 0 || !c.patterns.is_empty())
        .filter_map(|(i, c)| region.pos(*i).filter(|(s, e)| e > s && *s >= ms && *e <= me).map(|(s, e)| (s, e, c)))
        .collect();
    // Capture 0 with patterns is retokenized like the others. Without, it only sets a scope.
    let zero = caps.iter().find(|(i, _)| *i == 0).map(|(_, c)| c).filter(|c| c.patterns.is_empty());
    ranges.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let base = scopes.len();
    if let Some(c) = zero {
        scopes.push(c.sc);
    }
    let mut active: Vec<usize> = vec![];
    let mut cur = ms;
    for (s, e, c) in ranges {
        while let Some(&end) = active.last() {
            if end > s {
                break;
            }
            sink.emit(text, cur, end, scopes);
            cur = end;
            active.pop();
            scopes.pop();
        }
        if s > cur {
            sink.emit(text, cur, s, scopes);
            cur = s;
        }
        if !c.patterns.is_empty() && depth < 6 {
            let mark = scopes.len();
            scopes.push(c.sc);
            let mut list = vec![];
            expand(lk, g, &c.patterns, &mut list, &mut HashSet::new());
            let sub = &text[..e];
            let mut frames: Vec<Frame> = vec![];
            scan(lk, g, sub, s, e, &mut frames, &list, scopes, sink, depth + 1);
            scopes.truncate(mark);
            cur = e;
            continue;
        }
        scopes.push(c.sc);
        active.push(e);
    }
    while let Some(end) = active.pop() {
        sink.emit(text, cur, end, scopes);
        cur = end;
        scopes.pop();
    }
    sink.emit(text, cur, me, scopes);
    scopes.truncate(base);
}

/// The main loop: finds the next thing that happens in the text, colors what comes before it
/// and applies it. `root` is the list of patterns used when no rule is open.
#[allow(clippy::too_many_arguments)]
fn scan(lk: &dyn Lookup, g0: usize, text: &str, from: usize, to: usize, frames: &mut Vec<Frame>, root: &[(usize, usize)], scopes: &mut Vec<Sc>, sink: &mut Sink, depth: usize) {
    let mut pos = from;
    // Rules carried over from the line before were not opened here.
    let mut enter: Vec<usize> = vec![usize::MAX; frames.len()];
    // Where `\G` matches: the end of the begin match of the innermost open rule.
    let mut anchor = usize::MAX;
    let mut anchors: Vec<usize> = vec![usize::MAX; frames.len()];
    let mut steps = 0;
    let mut region = Region::new();
    let mut cache: HashMap<(usize, usize), Option<(usize, Region)>> = HashMap::new();
    while pos <= to && steps < MAX_STEPS {
        steps += 1;
        let top = frames.last().cloned();
        let list: Arc<Vec<(usize, usize)>> = match &top {
            Some(f) => patterns_of(lk, f.g, f.rule),
            None => Arc::new(root.to_vec()),
        };
        let mut best: Option<Hit> = None;
        let consider = |best: &mut Option<Hit>, region: &Region, start: usize, what: What| {
            if best.as_ref().is_none_or(|b| start < b.start) {
                *best = Some(Hit { start, region: region.clone(), what });
            }
        };
        let mut end_last = false;
        let mut end_re: Option<Arc<Regex>> = None;
        if let Some(f) = &top {
            if let Some(gr) = lk.grammar(f.g) {
                if let (Kind::Begin { apply_end_last, .. }, Some(src)) = (&gr.rules[f.rule].kind, &f.end) {
                    end_last = *apply_end_last;
                    end_re = gr.end_regex(src, pos == anchor);
                }
            }
        }
        if !end_last {
            if let Some(re) = &end_re {
                if let Some(s) = run(re, text, pos, to, &mut region) {
                    consider(&mut best, &region, s, What::End);
                }
            }
        }
        for &(g, r) in list.iter() {
            let Some(gr) = lk.grammar(g) else { continue };
            let pat = match &gr.rules[r].kind {
                Kind::Match { re, .. } => re,
                Kind::Begin { begin, .. } | Kind::While { begin, .. } => begin,
                _ => continue,
            };
            // A search that found nothing, or something further on, stays true as the scan moves
            // forward, so it is not repeated. Patterns that use `\G` depend on where we are.
            let cacheable = !pat.has_g;
            let fresh = !cacheable || !matches!(cache.get(&(g, r)), Some(None) | Some(Some(_))) || matches!(cache.get(&(g, r)), Some(Some((s, _))) if *s < pos);
            if fresh {
                let Some(re) = pat.regex(pos == anchor) else { continue };
                let found = run(re, text, pos, to, &mut region).map(|s| (s, region.clone()));
                if !cacheable {
                    if let Some((s, reg)) = found {
                        if matches!(gr.rules[r].kind, Kind::Match { .. }) && reg.pos(0).is_some_and(|(a, b)| a == b) {
                            continue;
                        }
                        consider(&mut best, &reg, s, What::Rule(g, r));
                    }
                    continue;
                }
                cache.insert((g, r), found);
            }
            if let Some(Some((s, reg))) = cache.get(&(g, r)) {
                if matches!(gr.rules[r].kind, Kind::Match { .. }) && reg.pos(0).is_some_and(|(a, b)| a == b) {
                    continue;
                }
                consider(&mut best, reg, *s, What::Rule(g, r));
            }
        }
        if end_last {
            if let Some(re) = &end_re {
                if let Some(s) = run(re, text, pos, to, &mut region) {
                    consider(&mut best, &region, s, What::End);
                }
            }
        }
        let Some(hit) = best else {
            sink.emit(text, pos, to, scopes);
            return;
        };
        let (ms, me) = hit.region.pos(0).unwrap_or((hit.start, hit.start));
        sink.emit(text, pos, ms, scopes);
        match hit.what {
            What::End => {
                let f = top.expect("an end match needs an open rule");
                let gr = lk.grammar(f.g).expect("the grammar of an open rule exists");
                let Kind::Begin { end_caps, .. } = &gr.rules[f.rule].kind else { return };
                // Closing: the text of the end match belongs to the rule but not to its content.
                scopes.pop();
                captured(lk, f.g, text, &hit.region, end_caps, scopes, sink, depth);
                if end_caps.is_empty() {
                    sink.emit(text, ms, me, scopes);
                }
                scopes.pop();
                let opened = enter.pop().unwrap_or(pos);
                anchor = anchors.pop().unwrap_or(usize::MAX);
                frames.pop();
                if me == ms && opened == ms {
                    // An empty rule that opens and closes in place: stop here to avoid a loop.
                    sink.emit(text, me, to, scopes);
                    return;
                }
                pos = me;
            }
            What::Rule(g, r) => {
                let gr = lk.grammar(g).expect("the grammar of a matched rule exists");
                match &gr.rules[r].kind {
                    Kind::Match { caps, .. } => {
                        scopes.push(gr.rules[r].name);
                        if caps.is_empty() {
                            sink.emit(text, ms, me, scopes);
                        } else {
                            captured(lk, g, text, &hit.region, caps, scopes, sink, depth);
                        }
                        scopes.pop();
                        pos = me;
                    }
                    Kind::Begin { end, begin_caps, .. } | Kind::While { cond: end, begin_caps, .. } => {
                        let is_while = matches!(gr.rules[r].kind, Kind::While { .. });
                        scopes.push(gr.rules[r].name);
                        if begin_caps.is_empty() {
                            sink.emit(text, ms, me, scopes);
                        } else {
                            captured(lk, g, text, &hit.region, begin_caps, scopes, sink, depth);
                        }
                        scopes.push(gr.rules[r].content);
                        let src = if is_while { None } else { Some(backrefs(end, text, &hit.region)) };
                        frames.push(Frame { g, rule: r, end: src });
                        enter.push(ms);
                        anchors.push(anchor);
                        anchor = me;
                        pos = me;
                    }
                    _ => pos = me.max(pos + 1),
                }
            }
        }
        if pos == ms && pos == me && steps > 64 && pos < to {
            // No progress for a while: move on by one character.
            let step = text[pos..].chars().next().map_or(1, char::len_utf8);
            sink.emit(text, pos, pos + step, scopes);
            pos += step;
        }
        if pos > to {
            break;
        }
    }
    let _ = g0;
}

/// Interned stacks, so that a line's state is a small number the editor can keep.
#[derive(Default)]
pub struct States {
    ids: HashMap<Vec<Frame>, u32>,
    list: Vec<Arc<Vec<Frame>>>,
}

impl States {
    pub fn new() -> States {
        States { ids: HashMap::new(), list: vec![Arc::new(vec![])] }
    }

    fn get(&self, id: u32) -> Vec<Frame> {
        self.list.get(id as usize).map_or_else(Vec::new, |s| s.to_vec())
    }

    fn id_of(&mut self, stack: &[Frame]) -> u32 {
        if stack.is_empty() {
            return 0;
        }
        if let Some(id) = self.ids.get(stack) {
            return *id;
        }
        let id = self.list.len() as u32;
        self.list.push(Arc::new(stack.to_vec()));
        self.ids.insert(stack.to_vec(), id);
        id
    }
}

/// Whether a line state ends inside a block comment.
pub fn in_block_comment(lk: &dyn Lookup, states: &Mutex<States>, state: u32) -> bool {
    let frames = states.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(state);
    frames.last().is_some_and(|f| lk.grammar(f.g).is_some_and(|g| g.rules[f.rule].block))
}

/// Colors one line. `g` is the grammar of the language, `state` the one left by the line before.
pub fn tokenize(lk: &dyn Lookup, states: &Mutex<States>, g: usize, line: &str, state: u32, out: &mut Vec<Tok>) -> u32 {
    let mut frames = states.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(state);
    if line.len() > MAX_LINE {
        return state;
    }
    let Some(gr) = lk.grammar(g) else { return state };
    // A blank line outside of any rule has nothing to color and changes nothing.
    if frames.is_empty() && line.trim().is_empty() {
        return 0;
    }
    let text = format!("{line}\n");
    let mut scopes = vec![Sc::Skip];
    scopes.extend(scopes_of(lk, &frames, true));
    let root = patterns_of(lk, g, gr.root);
    let mut sink = Sink { out, limit: line.len() };
    // `while` rules must still hold at the start of the line, or they close.
    let mut keep = 0;
    let mut pos = 0;
    for (i, f) in frames.iter().enumerate() {
        let Some(fg) = lk.grammar(f.g) else { break };
        if let Kind::While { cond, .. } = &fg.rules[f.rule].kind {
            let ok = compile(cond, true).and_then(|re| {
                let mut region = Region::new();
                run(&re, &text, pos, text.len(), &mut region).filter(|s| *s == pos).and_then(|_| region.pos(0))
            });
            match ok {
                Some((_, e)) => pos = e,
                None => break,
            }
        }
        keep = i + 1;
    }
    if keep < frames.len() {
        frames.truncate(keep);
        scopes.truncate(1);
        scopes.extend(scopes_of(lk, &frames, true));
    }
    scan(lk, g, &text, pos, text.len(), &mut frames, &root, &mut scopes, &mut sink, 0);
    // The line's own newline can still close rules (`$` ends in it), so the search covers it.
    states.lock().unwrap_or_else(std::sync::PoisonError::into_inner).id_of(&frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::RwLock;

    struct One(Vec<Arc<Grammar>>);

    impl Lookup for One {
        fn grammar(&self, id: usize) -> Option<Arc<Grammar>> {
            self.0.get(id).cloned()
        }
        fn by_scope(&self, scope: &str) -> Option<usize> {
            self.0.iter().position(|g| g.scope == scope)
        }
    }

    fn lang(json: &str) -> One {
        One(vec![Arc::new(Grammar::parse(json).unwrap())])
    }

    fn lines(l: &One, src: &[&str]) -> Vec<Vec<(String, u8)>> {
        let states = Mutex::new(States::new());
        let mut st = 0;
        src.iter()
            .map(|line| {
                let mut out = vec![];
                st = tokenize(l, &states, 0, line, st, &mut out);
                out.iter().map(|t| (line[t.start as usize..t.end as usize].to_string(), t.tag)).collect()
            })
            .collect()
    }

    const TOY: &str = r##"{
      "scopeName": "source.toy",
      "patterns": [
        {"include": "#comment"}, {"include": "#string"}, {"include": "#number"}, {"include": "#word"}, {"include": "#fn"}
      ],
      "repository": {
        "comment": {"begin": "/\\*", "end": "\\*/", "name": "comment.block.toy"},
        "string": {"begin": "\"", "end": "\"", "name": "string.quoted.double.toy",
                   "patterns": [{"match": "\\\\.", "name": "constant.character.escape.toy"}]},
        "number": {"match": "\\b[0-9]+\\b", "name": "constant.numeric.toy"},
        "word": {"match": "\\b(let|if|else)\\b", "name": "keyword.control.toy"},
        "fn": {"match": "\\b([a-z_]+)\\s*\\(", "captures": {"1": {"name": "entity.name.function.toy"}}}
      }
    }"##;

    #[test]
    fn matches_captures_and_nested_patterns_are_colored() {
        let l = lang(TOY);
        let got = lines(&l, &["let x = 42 \"a\\nb\" foo(1)"]);
        let row = &got[0];
        assert!(row.contains(&("let".into(), KEYWORD)), "{row:?}");
        assert!(row.contains(&("42".into(), NUMBER)), "{row:?}");
        assert!(row.contains(&("\\n".into(), ESCAPE)), "{row:?}");
        assert!(row.contains(&("foo".into(), FUNCTION)), "{row:?}");
        assert!(row.iter().any(|(s, t)| s.starts_with("\"a") && *t == STRING), "{row:?}");
    }

    #[test]
    fn a_rule_that_spans_lines_carries_its_state() {
        let l = lang(TOY);
        let got = lines(&l, &["a /* open", "let inside", "close */ let"]);
        assert!(got[0].iter().any(|(s, t)| s == "/* open" && *t == COMMENT), "{:?}", got[0]);
        assert_eq!(got[1], vec![("let inside".to_string(), COMMENT)]);
        assert!(got[2].iter().any(|(s, t)| s == "close */" && *t == COMMENT), "{:?}", got[2]);
        assert!(got[2].iter().any(|(s, t)| s == "let" && *t == KEYWORD), "{:?}", got[2]);
    }

    #[test]
    fn the_end_pattern_can_use_what_the_begin_matched() {
        let l = lang(r#"{"scopeName":"source.q","patterns":[{"begin":"<<(\\w+)","end":"^\\1$","name":"string.unquoted.heredoc.q"}]}"#);
        let got = lines(&l, &["x <<EOT", "text", "EOT", "after"]);
        assert!(!got[0].is_empty() && got[1] == vec![("text".to_string(), STRING)]);
        assert!(got[2].iter().any(|(s, _)| s == "EOT"));
        assert!(got[3].is_empty());
    }

    #[test]
    fn includes_reach_other_grammars() {
        let outer = r#"{"scopeName":"source.outer","patterns":[{"begin":"<<","end":">>","patterns":[{"include":"source.inner"}]}]}"#;
        let inner = r#"{"scopeName":"source.inner","patterns":[{"match":"\\b\\d+\\b","name":"constant.numeric.inner"}]}"#;
        let l = One(vec![Arc::new(Grammar::parse(outer).unwrap()), Arc::new(Grammar::parse(inner).unwrap())]);
        let got = lines(&l, &["<< 12 >> 34"]);
        assert_eq!(got[0], vec![("12".to_string(), NUMBER)]);
    }

    #[test]
    fn capture_zero_can_hold_patterns() {
        let l = lang(r#"{"scopeName":"s","patterns":[{"match":"\\d+x","captures":{"0":{"patterns":[{"match":"\\d+","name":"constant.numeric"}]}}}]}"#);
        let got = lines(&l, &["a 42x b"]);
        assert_eq!(got[0], vec![("42".to_string(), NUMBER)]);
    }

    #[test]
    fn a_rule_that_ends_where_the_next_line_starts_does_not_stop_the_line() {
        let l = lang(r#"{"scopeName":"s","patterns":[{"begin":"//","end":"(?=^)","name":"comment.line"},{"match":"\\bkw\\b","name":"keyword.control"}]}"#);
        let got = lines(&l, &["// note", "kw rest"]);
        assert_eq!(got[0], vec![("// note".to_string(), COMMENT)]);
        assert_eq!(got[1], vec![("kw".to_string(), KEYWORD)]);
    }

    #[test]
    fn embedded_code_does_not_inherit_the_color_of_its_block() {
        assert_eq!(scope_class("meta.embedded.block.python"), Sc::Plain);
        assert_eq!(tag_of(&[scope_class("markup.fenced_code.block"), scope_class("meta.embedded.block.python")], "  "), 0);
    }

    #[test]
    fn operators_are_only_colored_when_they_are_words() {
        assert_eq!(scope_class("keyword.operator.arithmetic.c"), Sc::Op);
        assert_eq!(tag_of(&[Sc::Op], "+"), 0);
        assert_eq!(tag_of(&[Sc::Op], "typeof"), KEYWORD);
        assert_eq!(tag_of(&[scope_class("string.quoted"), scope_class("punctuation.definition.string.begin")], "\""), STRING);
    }

    #[test]
    fn scope_names_map_to_the_colors_of_the_themes() {
        for (name, tag) in [
            ("comment.line.double-slash.js", COMMENT),
            ("constant.language.boolean.true.js", CONSTANT),
            ("storage.type.function.js", KEYWORD),
            ("storage.type.c", TYPE),
            ("support.type.property-name.json", MEMBER),
            ("entity.name.type.class.python", USER_TYPE),
            ("meta.preprocessor.include.c", DIRECTIVE),
            ("entity.name.tag.html", KEYWORD),
            ("variable.parameter.python", VARIABLE),
        ] {
            assert_eq!(scope_class(name), Sc::Tag(tag), "{name}");
        }
        assert_eq!(scope_class("punctuation.separator"), Sc::Skip);
        let _ = RwLock::new(0);
    }
}
