use crate::lexer::{self, Lang, Tok};
use ropey::Rope;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

pub const MAX_RENDER: usize = 4000;
/// Lines whose tokens are kept: a few screens' worth, plus what was scrolled through lately.
const TOK_CACHE: usize = 4096;
const CLIP_MAX: usize = 256 << 10;
const MAX_UNDO_BYTES: usize = 8 << 20;
const OVERSCAN: usize = 30;
const COALESCE_MS: u128 = 900;

/// Where a line wraps: for each row, the column it starts at and its first cell.
type Breaks = std::sync::Arc<[(usize, usize)]>;
/// A line, the text's revision, the wrap width and the tab size.
type BreaksKey = (usize, u64, usize, usize);

/// What soft wrapping knows about the lines: how many rows each takes and where each starts.
#[derive(Default)]
struct WrapCache {
    counts: Vec<u32>,
    starts: Vec<usize>,
    folds: Vec<(usize, usize)>,
    key: Option<(usize, usize, u64)>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Cmd {
    Type {
        s: String,
    },
    Insert {
        s: String,
    },
    Newline,
    Backspace,
    Delete,
    DeleteWordLeft,
    DeleteWordRight,
    Tab,
    Outdent,
    Move {
        to: Motion,
        ext: bool,
        rows: Option<usize>,
    },
    Click {
        line: usize,
        vcol: usize,
        ext: bool,
    },
    Word {
        line: usize,
        vcol: usize,
    },
    SelectLine {
        line: usize,
    },
    SelectAll,
    MoveSelection {
        line: usize,
        vcol: usize,
        copy: bool,
    },
    NewLineBelow,
    NewLineAbove,
    CompleteStatement,
    ToggleCase,
    GotoBracket,
    Undo,
    Redo,
    ToggleComment,
    /// Cleans up before saving: trailing spaces and the last newline.
    Tidy {
        trim: bool,
        newline: bool,
    },
    DuplicateLine,
    DeleteLine,
    CutSelection,
    Complete {
        s: String,
        #[serde(default)]
        back: usize,
    },
    /// Adds a caret where the pointer is (Alt+Click), or takes one away.
    AddCursor {
        line: usize,
        vcol: usize,
    },
    /// Selects the word, then the next place where the selected text appears, as another cursor.
    SelectNext,
    /// Puts a cursor on every place where the selected text (or the word) appears.
    SelectAllMatches,
    /// Adds a caret on the line above or below the cursors.
    AddCursorVertical {
        up: bool,
    },
    ClearCursors,
    /// Sets the width, in columns, where long lines wrap. 0 turns wrapping off.
    SetWrap {
        cols: usize,
    },
    /// A snippet in place of the word before the caret, with the ranges Tab goes through.
    Snippet {
        s: String,
        stops: Vec<[usize; 2]>,
    },
    Goto {
        line: usize,
        col: usize,
    },
    FindNext {
        back: bool,
    },
    ReplaceOne {
        s: String,
    },
    ReplaceAll {
        s: String,
    },
    Fresh,
    MoveLines {
        up: bool,
    },
    JoinLines,
    ExpandSelection,
    ShrinkSelection,
    ToggleFold {
        row: usize,
    },
    FoldCaret {
        fold: bool,
    },
    FoldAll {
        fold: bool,
    },
    ToggleBreakpoint {
        row: Option<usize>,
    },
    SetBreakpoints {
        lines: Vec<usize>,
    },
    SetExec {
        line: Option<usize>,
    },
}

impl Cmd {
    /// Commands that act on every cursor.
    fn multi(&self) -> bool {
        use Cmd::*;
        matches!(self, Type { .. } | Insert { .. } | Newline | Backspace | Delete | DeleteWordLeft | DeleteWordRight | Tab | Outdent | Move { .. })
    }

    /// Commands that leave the extra cursors as they are. Any other single-cursor command ends them.
    fn keeps_cursors(&self) -> bool {
        use Cmd::*;
        matches!(
            self,
            Fresh | ToggleFold { .. } | FoldAll { .. } | FoldCaret { .. } | SetBreakpoints { .. } | SetExec { .. } | ToggleBreakpoint { .. } | AddCursor { .. } | SelectNext | SelectAllMatches | AddCursorVertical { .. } | ClearCursors | SetWrap { .. }
        )
    }

    fn mutates(&self) -> bool {
        use Cmd::*;
        matches!(
            self,
            Type { .. }
                | Insert { .. }
                | Newline
                | Backspace
                | Delete
                | DeleteWordLeft
                | DeleteWordRight
                | Tab
                | Outdent
                | Undo
                | Redo
                | ToggleComment
                | Tidy { .. }
                | DuplicateLine
                | DeleteLine
                | CutSelection
                | Complete { .. }
                | Snippet { .. }
                | ReplaceOne { .. }
                | ReplaceAll { .. }
                | MoveLines { .. }
                | JoinLines
                | MoveSelection { .. }
                | NewLineBelow
                | NewLineAbove
                | CompleteStatement
                | ToggleCase
        )
    }
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    WordLeft,
    WordRight,
    PageUp,
    PageDown,
    DocStart,
    DocEnd,
}

#[derive(Serialize, Default)]
pub struct View {
    pub carets: Vec<[usize; 2]>,
    pub lang: String,
    pub pending: bool,
    pub rev: u64,
    pub n: usize,
    pub top: usize,
    pub first: usize,
    pub lines: Vec<String>,
    pub nums: Vec<usize>,
    /// Whether the row continues the line of the row above, for a wrapped line.
    pub cont: Vec<bool>,
    pub cur: [usize; 2],
    pub line: usize,
    pub col: usize,
    pub total: usize,
    pub crlf: bool,
    pub use_tabs: bool,
    pub tab: usize,
    pub readonly: bool,
    pub folds: Vec<[usize; 2]>,
    pub bps: Vec<usize>,
    pub git: Vec<[usize; 2]>,
    pub exec: Option<usize>,
    pub stripes: Vec<[usize; 2]>,
    pub occ: Vec<[usize; 3]>,
    pub pair: Vec<[usize; 2]>,
    pub all_bps: Option<Vec<usize>>,
    pub dirty: bool,
    pub sel: Vec<[usize; 3]>,
    pub marks: Vec<[usize; 3]>,
    pub diags: Vec<[usize; 4]>,
    pub msg: Option<String>,
    pub maxw: usize,
    pub clip: Option<String>,
    pub errors: usize,
    pub warnings: usize,
}

#[derive(Clone, Debug)]
pub struct Diag {
    pub sl: usize,
    pub sc: usize,
    pub el: usize,
    pub ec: usize,
    pub sev: usize,
    pub msg: String,
    pub raw: serde_json::Value,
}

type PairCache = ((usize, u64), Option<(usize, usize)>);

#[derive(Default, Clone)]
struct SemLine {
    toks: Vec<(u32, u32, u8)>,
    fresh: bool,
}

#[derive(Clone)]
struct Op {
    at: usize,
    del: String,
    ins: String,
}

struct Txn {
    ops: Vec<Op>,
    before: (usize, usize),
    after: (usize, usize),
    typing: bool,
    at: Instant,
}

pub struct Doc {
    pub path: PathBuf,
    pub rope: Rope,
    pub lang: Lang,
    pub rev: u64,
    pub saved_rev: u64,
    pub mtime: Option<SystemTime>,
    pub crlf: bool,
    pub diags: Vec<Diag>,
    pub lsp_rev: Option<u64>,
    pub lsp_ver: i32,
    pub tab: usize,
    pub use_tabs: bool,
    cur: usize,
    anchor: usize,
    want: Option<usize>,
    undo: Vec<Txn>,
    redo: Vec<Txn>,
    undo_bytes: usize,
    open_ops: Vec<Op>,
    before: (usize, usize),
    /// The state each line starts in, as far as it was worked out, kept across changes the way
    /// VS Code does (textModelTokens.ts). `check[k]` says line k - 1 changed since, so `states[k]`
    /// must be worked out again; the others follow from the one before. Only the first `valid`
    /// are sure. Working a state out again and finding it as it was stops there: the lines after
    /// are left as they are, however many changes there were and wherever they are.
    states: Vec<u32>,
    check: Vec<bool>,
    valid: usize,
    /// How many lines were gone through only to know the state of the next one, for the tests.
    #[cfg(test)]
    worked: usize,
    raws: Vec<String>,
    query: String,
    case: bool,
    sent_clip: Option<(usize, usize, u64)>,
    maxw: usize,
    pub readonly: bool,
    folds: Vec<(usize, usize)>,
    bps: Vec<usize>,
    bps_changed: bool,
    pub git_marks: Vec<(usize, usize, u8)>,
    exec: Option<usize>,
    sel_stack: Vec<(usize, usize)>,
    sem: Vec<SemLine>,
    pair_cache: Option<PairCache>,
    /// The tab stops still ahead in the snippet being filled in, as character ranges, and the
    /// one the caret is on.
    snip: Option<(Vec<[usize; 2]>, usize)>,
    /// Soft wrap: the width in columns, or 0 for no wrapping.
    wrap: usize,
    wrap_cache: std::cell::RefCell<WrapCache>,
    /// The wrapped rows of the line measured last, by (line, rev, wrap, tab): one long line
    /// fills many rows of a view, and each of them asks for it.
    breaks_memo: std::cell::RefCell<Option<(BreaksKey, Breaks)>>,
    /// Tokens of lines a grammar already colored, by the text and the state they start in. A
    /// grammar takes about a millisecond for a line, and between two keys most lines stay the same.
    tok_cache: std::collections::HashMap<(u64, u32), (Vec<Tok>, u32)>,
    /// The other cursors, as `[cursor, anchor]`. The main one is `cur` and `anchor`.
    extras: Vec<[usize; 2]>,
    /// A language the user chose by hand, by id. It wins over the file name.
    forced: Option<String>,
    /// Until when this view may spend on the line states of a grammar.
    state_deadline: Option<Instant>,
    /// Some lines of the last view could not be colored yet, so it should be asked for again.
    pending: bool,
    /// While drawing lines whose state is not known yet: the guessed state of the next one.
    guess: Option<(usize, u32)>,
}

/// The part of a line that is drawn (see `MAX_RENDER`). Marks further on would not show, and
/// looking for them on a line of megabytes costs a lot at every key.
fn drawn(text: &str) -> &str {
    text.char_indices().nth(MAX_RENDER + 64).map_or(text, |(b, _)| &text[..b])
}

fn text_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Whether `i` falls in one of `ranges`, which are in order and don't overlap.
fn in_ranges(ranges: &[(usize, usize)], i: usize) -> bool {
    let k = ranges.partition_point(|&(a, _)| a <= i);
    k > 0 && i < ranges[k - 1].1
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn closer(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        _ => None,
    }
}

impl Doc {
    /// Picks the language again, after extensions were installed or removed. The saved line
    /// states of a grammar belong to the registry that made them, so they are dropped too.
    pub fn relang(&mut self) -> bool {
        let lang = self.forced.as_deref().and_then(Lang::from_id).unwrap_or_else(|| Lang::from_path(&self.path));
        let changed = lang != self.lang || matches!(lang, Lang::Grammar(_));
        self.lang = lang;
        if changed {
            self.reset_states();
            self.raws.clear();
            self.tok_cache.clear();
        }
        changed
    }

    /// Chooses the language by hand, or back to the one the file name says with `None`.
    pub fn set_language(&mut self, id: Option<&str>) -> Result<(), String> {
        if let Some(id) = id {
            Lang::from_id(id).ok_or("Unknown language")?;
        }
        self.forced = id.map(String::from);
        let lang = id.and_then(Lang::from_id).unwrap_or_else(|| Lang::from_path(&self.path));
        self.lang = lang;
        self.reset_states();
        self.raws.clear();
        self.tok_cache.clear();
        Ok(())
    }

    pub fn new(path: PathBuf, text: &str, mtime: Option<SystemTime>) -> Doc {
        let crlf = text.contains("\r\n");
        let normalized: Cow<str> = if crlf {
            Cow::Owned(text.replace("\r\n", "\n"))
        } else {
            Cow::Borrowed(text)
        };
        let rope = Rope::from_str(&normalized);
        let lang = Lang::from_path(&path);
        let use_tabs = (0..rope.len_lines().min(400))
            .filter(|&i| rope.line(i).len_chars() > 1)
            .fold((0, 0), |(t, s), i| match rope.line(i).char(0) {
                '\t' => (t + 1, s),
                ' ' => (t, s + 1),
                _ => (t, s),
            });
        Doc {
            path,
            rope,
            lang,
            rev: 0,
            saved_rev: 0,
            mtime,
            crlf,
            diags: vec![],
            lsp_rev: None,
            lsp_ver: 0,
            tab: 4,
            use_tabs: use_tabs.0 > use_tabs.1,
            cur: 0,
            anchor: 0,
            want: None,
            undo: vec![],
            redo: vec![],
            undo_bytes: 0,
            open_ops: vec![],
            before: (0, 0),
            states: vec![0],
            check: vec![false],
            valid: 1,
            #[cfg(test)]
            worked: 0,
            raws: vec![],
            query: String::new(),
            case: false,
            sent_clip: None,
            maxw: 0,
            readonly: false,
            folds: vec![],
            bps: vec![],
            bps_changed: false,
            git_marks: vec![],
            exec: None,
            sel_stack: vec![],
            sem: vec![],
            pair_cache: None,
            state_deadline: None,
            pending: false,
            guess: None,
            forced: None,
            snip: None,
            extras: vec![],
            wrap: 0,
            wrap_cache: std::cell::RefCell::new(WrapCache::default()),
            breaks_memo: std::cell::RefCell::new(None),
            tok_cache: std::collections::HashMap::new(),
        }
    }

    /// What must hold between any two commands. The fuzz test checks it after each one.
    #[cfg(test)]
    pub fn check(&self) -> Result<(), String> {
        let n = self.rope.len_chars();
        let lines = self.rope.len_lines();
        if self.cur > n || self.anchor > n {
            return Err(format!("caret {} / anchor {} past the end ({n})", self.cur, self.anchor));
        }
        if let Some((s, e)) = self.folds.iter().find(|(s, e)| s >= e || *e >= lines) {
            return Err(format!("fold {s}..{e} with {lines} lines"));
        }
        if let Some((a, b)) = self.sel_stack.iter().find(|(a, b)| *a > n || *b > n) {
            return Err(format!("selection stack {a}..{b} past the end ({n})"));
        }
        if let Some(e) = self.extras.iter().find(|e| e[0] > n || e[1] > n) {
            return Err(format!("extra cursor {e:?} past the end ({n})"));
        }
        if let Some((stops, _)) = &self.snip {
            if let Some(r) = stops.iter().find(|r| r[0] > n || r[1] > n) {
                return Err(format!("snippet stop {r:?} past the end ({n})"));
            }
        }
        Ok(())
    }

    /// The file was renamed or its folder moved. The text, its unsaved changes and the undo
    /// history stay; the colors follow the new name, as a .txt renamed to .cpp should.
    pub fn moved_to(&mut self, path: PathBuf) {
        self.path = path;
        // A language picked by hand stays; otherwise the new name decides.
        self.relang();
        self.tok_cache.clear();
        self.reset_states();
        self.sem.clear();
        self.diags.clear();
        self.lsp_rev = None;
        self.pair_cache = None;
    }

    /// What Ctrl+C copies: the selection, or the caret's line when nothing is selected.
    pub fn clip_text(&self) -> String {
        let (a, b) = self.sel_range();
        if a != b {
            self.rope.slice(a..b).to_string()
        } else {
            format!("{}\n", self.line_text(self.pos(self.cur).0))
        }
    }

    fn clip_len(&self) -> usize {
        let (a, b) = self.sel_range();
        if a != b {
            self.rope.char_to_byte(b) - self.rope.char_to_byte(a)
        } else {
            self.rope.line(self.pos(self.cur).0).len_bytes() + 1
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    pub fn contents_for_disk(&self) -> String {
        let s = self.rope.to_string();
        if self.crlf {
            s.replace('\n', "\r\n")
        } else {
            s
        }
    }

    pub fn reload(&mut self, text: &str, mtime: Option<SystemTime>) {
        let (line, col) = self.pos(self.cur);
        self.crlf = text.contains("\r\n");
        self.rope = if self.crlf {
            Rope::from_str(&text.replace("\r\n", "\n"))
        } else {
            Rope::from_str(text)
        };
        self.mtime = mtime;
        self.undo.clear();
        self.redo.clear();
        self.undo_bytes = 0;
        self.folds.clear();
        self.sem.clear();
        self.reset_states();
        self.rev += 1;
        self.saved_rev = self.rev;
        self.sent_clip = None;
        self.sel_stack.clear();
        self.drop_positions();
        self.cur = self.at(line, col);
        self.anchor = self.cur;
    }

    /// The row table of the lines, made again when the text, the folds or the width changed.
    fn wrap_table(&self) -> std::cell::Ref<'_, WrapCache> {
        {
            let mut c = self.wrap_cache.borrow_mut();
            let n = self.rope.len_lines();
            let key = (self.wrap, self.tab, self.rev);
            if c.key != Some(key) || c.folds != self.folds || c.counts.len() != n || c.starts.len() != n + 1 {
                if c.counts.len() != n || c.key.is_none_or(|k| (k.0, k.1) != (key.0, key.1)) {
                    c.counts = vec![0; n];
                }
                let mut starts = Vec::with_capacity(n + 1);
                let mut row = 0;
                let mut f = 0;
                for l in 0..n {
                    while f < self.folds.len() && l > self.folds[f].1 {
                        f += 1;
                    }
                    let hidden = f < self.folds.len() && l > self.folds[f].0 && l <= self.folds[f].1;
                    starts.push(row);
                    if !hidden {
                        if c.counts[l] == 0 {
                            c.counts[l] = self.line_breaks(l).len() as u32;
                        }
                        row += c.counts[l] as usize;
                    }
                }
                starts.push(row);
                c.starts = starts;
                c.folds = self.folds.clone();
                c.key = Some(key);
            }
        }
        self.wrap_cache.borrow()
    }

    /// Where each part of a line starts when it wraps: the column and the cell of its first
    /// character. A line that does not wrap has one part, at the start. A line breaks after
    /// the last space that fits, or in the middle of a word that is too long.
    fn line_breaks(&self, line: usize) -> std::sync::Arc<[(usize, usize)]> {
        if self.wrap == 0 {
            return std::sync::Arc::new([(0, 0)]);
        }
        let key = (line, self.rev, self.wrap, self.tab);
        if let Some((k, br)) = &*self.breaks_memo.borrow() {
            if *k == key {
                return br.clone();
            }
        }
        let br: std::sync::Arc<[(usize, usize)]> = self.measure_breaks(line).into();
        *self.breaks_memo.borrow_mut() = Some((key, br.clone()));
        br
    }

    fn measure_breaks(&self, line: usize) -> Vec<(usize, usize)> {
        let mut out = vec![(0, 0)];
        let (mut v, mut seg_v) = (0, 0);
        let mut last_space: Option<(usize, usize)> = None;
        for (col, c) in self.rope.line(line).chars().enumerate() {
            if c == '\n' || c == '\r' {
                break;
            }
            let next = self.step(v, c);
            while next - seg_v > self.wrap && v > seg_v {
                let (bc, bv) = last_space.filter(|(_, bv)| *bv > seg_v).unwrap_or((col, v));
                out.push((bc, bv));
                seg_v = bv;
                last_space = None;
            }
            v = next;
            if c == ' ' || c == '\t' {
                last_space = Some((col + 1, v));
            }
        }
        out
    }

    /// The line a row shows, and which part of it.
    fn locate(&self, row: usize) -> (usize, usize) {
        if self.wrap == 0 {
            return (self.row_to_line(row), 0);
        }
        let line = self.row_to_line(row);
        let first = self.line_to_row(line);
        (line, row.saturating_sub(first))
    }

    /// The row and the cell in it where a position is shown.
    fn cell_of(&self, pos: usize) -> (usize, usize) {
        let (line, col) = self.pos(pos);
        let v = self.vcol(line, col);
        if self.wrap == 0 {
            return (self.line_to_row(line), v);
        }
        let br = self.line_breaks(line);
        let k = br.partition_point(|b| b.0 <= col).saturating_sub(1);
        (self.line_to_row(line) + k, v - br[k].1)
    }

    /// The position of a cell of a row, as the pointer or an arrow key means it.
    fn pos_at_cell(&self, row: usize, vcol: usize) -> usize {
        let (line, k) = self.locate(row);
        if self.wrap == 0 {
            return self.at(line, self.col_from_vcol(line, vcol));
        }
        let br = self.line_breaks(line);
        let k = k.min(br.len() - 1);
        let mut col = self.col_from_vcol(line, br[k].1 + vcol).max(br[k].0);
        if let Some(next) = br.get(k + 1) {
            col = col.min(next.0);
        }
        self.at(line, col)
    }

    fn line_to_row(&self, line: usize) -> usize {
        if self.wrap > 0 {
            let t = self.wrap_table();
            let n = t.starts.len() - 1;
            let line = line.min(n.saturating_sub(1));
            return match t.folds.iter().find(|&&(s, e)| line > s && line <= e) {
                Some(&(s, _)) => t.starts[s],
                None => t.starts[line],
            };
        }
        let mut hidden = 0;
        for &(s, e) in &self.folds {
            if line > e {
                hidden += e - s;
            } else if line > s {
                return s - hidden;
            } else {
                break;
            }
        }
        line - hidden
    }

    fn row_to_line(&self, row: usize) -> usize {
        if self.wrap > 0 {
            let t = self.wrap_table();
            let n = t.starts.len() - 1;
            return t.starts[..n].partition_point(|&s| s <= row).saturating_sub(1).min(n.saturating_sub(1));
        }
        let mut line = row;
        for &(s, e) in &self.folds {
            if line > s {
                line += e - s;
            } else {
                break;
            }
        }
        line.min(self.rope.len_lines() - 1)
    }

    fn row_count(&self) -> usize {
        if self.wrap > 0 {
            return self.wrap_table().starts.last().copied().unwrap_or(0).max(1);
        }
        self.rope.len_lines() - self.folds.iter().map(|(s, e)| e - s).sum::<usize>()
    }

    pub fn set_semantic(&mut self, tokens: Vec<(usize, usize, usize, u8)>) {
        let total = self.rope.len_lines();
        let mut sem = vec![
            SemLine {
                toks: vec![],
                fresh: true
            };
            total
        ];
        for (line, col, len, tag) in tokens {
            if line >= total {
                continue;
            }
            let ascii = {
                let l = self.rope.line(line);
                l.len_bytes() == l.len_chars()
            };
            let (start, end) = if ascii {
                (col, col + len)
            } else {
                (
                    self.col_from_utf16(line, col),
                    self.col_from_utf16(line, col + len),
                )
            };
            sem[line].toks.push((start as u32, end as u32, tag));
        }
        self.sem = sem;
    }

    fn shift_sem(
        &mut self,
        line: usize,
        col: usize,
        del: usize,
        ins: usize,
        removed: usize,
        added: usize,
    ) {
        if line >= self.sem.len() {
            return;
        }
        let (c, d, i) = (col as u32, del as u32, ins as u32);
        let sl = &mut self.sem[line];
        sl.fresh = false;
        if removed == 0 && added == 0 {
            sl.toks.retain_mut(|t| {
                if t.1 <= c {
                    true
                } else if t.0 >= c + d {
                    t.0 = t.0 + i - d;
                    t.1 = t.1 + i - d;
                    true
                } else {
                    false
                }
            });
        } else {
            sl.toks.retain(|t| t.1 <= c);
            let end = (line + removed).min(self.sem.len() - 1);
            self.sem.splice(
                line + 1..=end,
                std::iter::repeat_n(SemLine::default(), added),
            );
        }
    }

    fn overlay_semantic(&self, line: usize, text: &str, toks: &mut Vec<Tok>) {
        let Some(sl) = self.sem.get(line) else { return };
        let guess = |tag: u8| {
            matches!(
                tag,
                lexer::FUNCTION
                    | lexer::USER_TYPE
                    | lexer::NAMESPACE
                    | lexer::MACRO
                    | lexer::MEMBER
                    | lexer::VARIABLE
            )
        };
        if sl.fresh {
            toks.retain(|t| !guess(t.tag));
        }
        if sl.toks.is_empty() {
            return;
        }
        let ascii = text.is_ascii();
        let offsets: Vec<usize> = if ascii {
            vec![]
        } else {
            text.char_indices()
                .map(|(b, _)| b)
                .chain([text.len()])
                .collect()
        };
        let byte = |c: u32| {
            if ascii {
                (c as usize).min(text.len())
            } else {
                *offsets.get(c as usize).unwrap_or(&text.len())
            }
        };
        for &(cs, ce, tag) in &sl.toks {
            let (s, e) = (byte(cs) as u32, byte(ce) as u32);
            if s >= e {
                continue;
            }
            let hits = |t: &Tok| t.start < e && s < t.end;
            if tag != lexer::COMMENT
                && toks
                    .iter()
                    .any(|t| hits(t) && !guess(t.tag) && t.tag != lexer::TYPE)
            {
                continue;
            }
            toks.retain(|t| !hits(t));
            if tag != 0 {
                toks.push(Tok {
                    start: s,
                    end: e,
                    tag,
                });
            }
        }
        toks.sort_unstable_by_key(|t| t.start);
    }

    fn shift_marks(&mut self, line: usize, col0: bool, removed: usize, added: usize) {
        if removed == 0 && added == 0 {
            return;
        }
        let delta = added as isize - removed as isize;
        let move_line = |l: usize| {
            if l > line + removed || (l == line && col0 && removed == 0) {
                (l as isize + delta) as usize
            } else if l > line {
                line
            } else {
                l
            }
        };
        self.folds.retain_mut(|f| {
            if line + removed < f.0 {
                *f = (
                    (f.0 as isize + delta) as usize,
                    (f.1 as isize + delta) as usize,
                );
                true
            } else {
                line > f.1
            }
        });
        let before = self.bps.clone();
        self.bps.iter_mut().for_each(|l| *l = move_line(*l));
        self.bps.dedup();
        if before != self.bps {
            self.bps_changed = true;
        }
        for m in &mut self.git_marks {
            m.0 = move_line(m.0);
            m.1 = move_line(m.1);
        }
    }

    fn fold_end(&mut self, line: usize) -> Option<usize> {
        let total = self.rope.len_lines();
        let mut toks = Vec::new();
        let opening_comment = {
            let st = self.state_of(line).unwrap_or(0);
            let text = self.line_text(line).into_owned();
            toks.clear();
            st == 0 && {
                let next = lexer::lex(&text, st, self.lang, &mut self.raws, &mut toks);
                lexer::in_block_comment(self.lang, next)
            }
        };
        if opening_comment {
            for l in line + 1..total.min(line + 20_000) {
                let next = self.state_of(l + 1).unwrap_or(0);
                if !lexer::in_block_comment(self.lang, next) {
                    return Some(l);
                }
            }
            return None;
        }
        let mut depth = 0i32;
        for l in line..total.min(line + 100_000) {
            let state = self.state_of(l).unwrap_or(0);
            let text = self.line_text(l).into_owned();
            toks.clear();
            lexer::lex(&text, state, self.lang, &mut self.raws, &mut toks);
            let mut t = toks.iter().peekable();
            for (i, b) in text.bytes().enumerate() {
                while t.peek().is_some_and(|k| k.end as usize <= i) {
                    t.next();
                }
                if t.peek().is_some_and(|k| {
                    k.start as usize <= i
                        && matches!(k.tag, lexer::COMMENT | lexer::STRING | lexer::ESCAPE)
                }) {
                    continue;
                }
                match b {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            return (l > line).then_some(l);
                        }
                    }
                    _ => {}
                }
            }
        }
        None
    }

    fn set_fold(&mut self, line: usize, fold: bool) {
        if let Some(i) = self.folds.iter().position(|f| f.0 == line) {
            if !fold {
                self.folds.remove(i);
            }
            return;
        }
        if !fold {
            return;
        }
        if let Some(end) = self.fold_end(line) {
            self.folds.retain(|f| !(f.0 >= line && f.1 <= end));
            if self.folds.iter().any(|f| f.0 < line && f.1 >= end) {
                return;
            }
            self.folds.push((line, end));
            self.folds.sort_unstable();
        }
    }

    fn fold_caret(&mut self, fold: bool) {
        let (line, _) = self.pos(self.cur);
        if !fold {
            self.set_fold(line, false);
            return;
        }
        let start = (0..=line).rev().take(5000).find(|&l| {
            self.fold_candidate(l) && self.brace_block_end(l).is_some_and(|e| e >= line)
        });
        if let Some(l) = start {
            self.set_fold(l, true);
            let at = self.at(l, 0);
            self.set(at);
        }
    }

    fn brace_block_end(&self, line: usize) -> Option<usize> {
        let mut depth = 0i32;
        for l in line..self.rope.len_lines().min(line + 20_000) {
            for c in self.rope.line(l).chars() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return (l > line).then_some(l);
                        }
                    }
                    _ => {}
                }
            }
        }
        None
    }

    fn fold_candidate(&self, line: usize) -> bool {
        self.line_text(line).trim_end().ends_with('{')
    }

    fn fold_all(&mut self, fold: bool) {
        self.folds.clear();
        if fold {
            let mut l = 0;
            while l < self.rope.len_lines() {
                if self.fold_candidate(l) {
                    if let Some(e) = self.fold_end(l) {
                        self.folds.push((l, e));
                        l = e;
                    }
                }
                l += 1;
            }
        }
    }

    fn toggle_breakpoint(&mut self, line: usize) {
        match self.bps.binary_search(&line) {
            Ok(i) => {
                self.bps.remove(i);
            }
            Err(i) => self.bps.insert(i, line),
        }
        self.bps_changed = true;
    }

    fn word_range(&self, at: usize) -> Option<(usize, usize)> {
        let n = self.rope.len_chars();
        let on = |i: usize| i < n && is_word(self.rope.char(i));
        if !on(at) && !(at > 0 && on(at - 1)) {
            return None;
        }
        let mut a = at;
        while a > 0 && on(a - 1) {
            a -= 1;
        }
        let mut b = at;
        while on(b) {
            b += 1;
        }
        Some((a, b))
    }

    /// The line and column shown at a cell of the view (wrapped rows and folds counted).
    pub fn point_at(&self, row: usize, vcol: usize) -> (usize, usize) {
        self.pos(self.pos_at_cell(row, vcol))
    }

    pub fn word_at(&self, row: usize, vcol: usize) -> Option<String> {
        let at = self.pos_at_cell(row, vcol);
        let (a, b) = self.word_range(at)?;
        Some(self.rope.slice(a..b).to_string())
    }

    pub fn diag_at(&self, row: usize, vcol: usize) -> Option<String> {
        let (line, col) = self.pos(self.pos_at_cell(row, vcol));
        let hit =
            |d: &&Diag| (d.sl, d.sc) <= (line, col) && (line, col) <= (d.el, d.ec.max(d.sc + 1));
        self.diags
            .iter()
            .filter(hit)
            .min_by_key(|d| d.sev)
            .map(|d| d.msg.clone())
    }

    pub fn next_problem(&self) -> Option<(usize, usize)> {
        let here = self.pos(self.cur);
        let mut all: Vec<(usize, usize)> = self
            .diags
            .iter()
            .filter(|d| d.sev <= 2)
            .map(|d| (d.sl, d.sc))
            .collect();
        all.sort_unstable();
        all.iter().find(|p| **p > here).or(all.first()).copied()
    }

    pub fn goto_chars(&mut self, line: usize, col: usize) {
        let at = self.at(line, col);
        self.set(at);
    }

    pub fn pos(&self, ch: usize) -> (usize, usize) {
        let line = self.rope.char_to_line(ch);
        (line, ch - self.rope.line_to_char(line))
    }

    pub fn cursor_pos(&self) -> (usize, usize) {
        self.pos(self.cur)
    }

    /// The caret's line and column with the text before and after it, when a suggestion makes sense.
    pub fn line_context(&self) -> Option<(usize, usize, String, String)> {
        if self.has_sel() || self.readonly || self.lang.lsp_id().is_none() {
            return None;
        }
        let (line, col) = self.cursor_pos();
        let text = self.line_text(line);
        let split = text.char_indices().nth(col).map_or(text.len(), |(b, _)| b);
        Some((
            line,
            col,
            text[..split].to_string(),
            text[split..].to_string(),
        ))
    }

    /// What a code model needs: the start of the file when it is cut off, and the text around the caret.
    pub fn windows(&self, before: usize, after: usize) -> (String, String, String) {
        let total = self.rope.len_chars();
        let from = self.cur.saturating_sub(before);
        let to = (self.cur + after).min(total);
        let head = if from > 0 {
            let lines = self.rope.len_lines().min(14);
            let end = self.rope.line_to_char(lines).min(500);
            self.rope.slice(..end.min(from)).to_string()
        } else {
            String::new()
        };
        (
            head,
            self.rope.slice(from..self.cur).to_string(),
            self.rope.slice(self.cur..to).to_string(),
        )
    }

    pub fn lines_for_suggest(&self) -> impl Iterator<Item = (usize, Cow<'_, str>)> {
        self.rope
            .lines()
            .enumerate()
            .map(|(i, l)| (i, Cow::from(l)))
    }

    fn line_len(&self, line: usize) -> usize {
        let s = self.rope.line(line);
        let n = s.len_chars();
        if n > 0 && s.char(n - 1) == '\n' {
            n - 1
        } else {
            n
        }
    }

    pub fn at(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.rope.len_lines() - 1);
        self.rope.line_to_char(line) + col.min(self.line_len(line))
    }

    pub fn line_text(&self, line: usize) -> Cow<'_, str> {
        let s: Cow<str> = self.rope.line(line).into();
        match s {
            Cow::Borrowed(b) => Cow::Borrowed(b.strip_suffix('\n').unwrap_or(b)),
            Cow::Owned(mut o) => {
                if o.ends_with('\n') {
                    o.pop();
                }
                Cow::Owned(o)
            }
        }
    }

    fn step(&self, vcol: usize, c: char) -> usize {
        if c == '\t' {
            vcol + self.tab - vcol % self.tab
        } else {
            vcol + 1
        }
    }

    fn vcol(&self, line: usize, col: usize) -> usize {
        let s = self.rope.line(line);
        let s = s.slice(..col.min(s.len_chars()));
        // Without tabs every character is one column. Looking for a tab is much faster than
        // walking the characters, which matters on a minified file with megabytes on one line.
        if s.chunks().all(|c| !c.contains('\t')) {
            return s.len_chars();
        }
        s.chars().fold(0, |v, c| self.step(v, c))
    }

    fn col_from_vcol(&self, line: usize, target: usize) -> usize {
        let mut v = 0;
        for (i, c) in self.rope.line(line).chars().enumerate() {
            if c == '\n' {
                return i;
            }
            let next = self.step(v, c);
            if target < next {
                return if target - v > (next - v) / 2 {
                    i + 1
                } else {
                    i
                };
            }
            v = next;
        }
        self.line_len(line)
    }

    pub fn utf16_col(&self, line: usize, col: usize) -> usize {
        self.rope
            .line(line)
            .chars()
            .take(col)
            .map(char::len_utf16)
            .sum()
    }

    pub fn col_from_utf16(&self, line: usize, units: usize) -> usize {
        if line >= self.rope.len_lines() {
            return 0;
        }
        let mut u = 0;
        for (i, c) in self.rope.line(line).chars().enumerate() {
            if u >= units || c == '\n' {
                return i;
            }
            u += c.len_utf16();
        }
        self.line_len(line)
    }

    /// The text of the line the caret is on.
    pub fn line_start_text(&self) -> String {
        let (line, _) = self.pos(self.cur);
        self.line_text(line).into_owned()
    }

    pub fn prefix(&self) -> String {
        let mut start = self.cur;
        while start > 0 && is_word(self.rope.char(start - 1)) {
            start -= 1;
        }
        self.rope.slice(start..self.cur).to_string()
    }

    /// Whether the character just before the caret is in a comment or a string.
    pub fn after_comment_or_string(&mut self) -> bool {
        let (line, col) = self.pos(self.cur);
        if col == 0 {
            return false;
        }
        let state = self.state_of(line).unwrap_or(0);
        let text = self.line_text(line).into_owned();
        let byte = text.char_indices().nth(col - 1).map_or(0, |(b, _)| b) as u32;
        let mut toks = vec![];
        lexer::lex(
            &text,
            state,
            self.lang,
            &mut self.raws,
            &mut toks,
        );
        toks.iter().any(|t| {
            (t.start..t.end).contains(&byte)
                && matches!(t.tag, lexer::COMMENT | lexer::STRING | lexer::ESCAPE)
        })
    }

    pub fn before_prefix(&self) -> String {
        let start = self.cur - self.prefix().chars().count();
        self.rope.slice(start.saturating_sub(2)..start).to_string()
    }

    fn sel_range(&self) -> (usize, usize) {
        (self.cur.min(self.anchor), self.cur.max(self.anchor))
    }

    fn has_sel(&self) -> bool {
        self.cur != self.anchor
    }

    fn peek(&self, at: usize) -> Option<char> {
        (at < self.rope.len_chars()).then(|| self.rope.char(at))
    }

    fn set(&mut self, cur: usize) {
        self.cur = cur;
        self.anchor = cur;
    }

    fn begin(&mut self) {
        self.before = (self.cur, self.anchor);
    }

    fn edit(&mut self, a: usize, b: usize, ins: &str) {
        // A range that is backwards or past the end (a stale reply from clangd, say) must not
        // panic: that would close the app with the unsaved work in it.
        let n = self.rope.len_chars();
        let (a, b) = (a.min(b).min(n), a.max(b).min(n));
        if a == b && ins.is_empty() {
            return;
        }
        let del: String = self.rope.slice(a..b).into();
        let line = self.rope.char_to_line(a);
        let col0 = a == self.rope.line_to_char(line);
        if a != b {
            self.rope.remove(a..b);
        }
        if !ins.is_empty() {
            self.rope.insert(a, ins);
        }
        let col = a - self.rope.line_to_char(line);
        if !self.extras.is_empty() {
            let (n_del, n_ins) = (b - a, ins.chars().count());
            // Text typed right at the edge of another cursor does not become part of it.
            let shift = |p: usize| {
                if a == b {
                    if p > a { p + n_ins } else { p }
                } else if p >= b {
                    p + n_ins - n_del
                } else if p > a {
                    a
                } else {
                    p
                }
            };
            for c in &mut self.extras {
                *c = [shift(c[0]), shift(c[1])];
            }
        }
        if let Some((stops, _)) = &mut self.snip {
            let (n_del, n_ins) = (b - a, ins.chars().count());
            let shift = |p: usize| if p >= b { p + n_ins - n_del } else if p > a { a } else { p };
            for s in stops.iter_mut() {
                *s = [shift(s[0]), shift(s[1])];
            }
        }
        let (removed, added) = (del.matches('\n').count(), ins.matches('\n').count());
        self.shift_sem(
            line,
            col,
            del.chars().count(),
            ins.chars().count(),
            removed,
            added,
        );
        self.shift_marks(line, col0, removed, added);
        self.note_wrap(line, removed, added);
        self.forget_states(line, removed, added);
        // Expand and shrink selection remember ranges of the old text.
        self.sel_stack.clear();
        self.rev += 1;
        self.open_ops.push(Op {
            at: a,
            del,
            ins: ins.to_string(),
        });
    }

    fn commit(&mut self, typing: bool) {
        if self.open_ops.is_empty() {
            return;
        }
        let ops = std::mem::take(&mut self.open_ops);
        self.undo_bytes += ops
            .iter()
            .map(|o| o.del.len() + o.ins.len() + 32)
            .sum::<usize>();
        self.redo.clear();
        let after = (self.cur, self.anchor);
        let word_break = ops.iter().any(|o| o.ins.chars().any(char::is_whitespace));
        if let Some(last) = self.undo.last_mut() {
            if typing
                && last.typing
                && !word_break
                && last.after == self.before
                && last.at.elapsed().as_millis() < COALESCE_MS
            {
                last.ops.extend(ops);
                last.after = after;
                last.at = Instant::now();
                return;
            }
        }
        self.undo.push(Txn {
            ops,
            before: self.before,
            after,
            typing,
            at: Instant::now(),
        });
        while self.undo_bytes > MAX_UNDO_BYTES && self.undo.len() > 1 {
            let old = self.undo.remove(0);
            self.undo_bytes -= old
                .ops
                .iter()
                .map(|o| o.del.len() + o.ins.len() + 32)
                .sum::<usize>()
                .min(self.undo_bytes);
        }
    }

    /// Forgets how many rows the lines of an edit took.
    fn note_wrap(&mut self, line: usize, removed: usize, added: usize) {
        let c = self.wrap_cache.get_mut();
        if line < c.counts.len() {
            let end = (line + removed + 1).min(c.counts.len());
            c.counts.splice(line..end, std::iter::repeat_n(0, added + 1));
        }
    }

    fn splice(&mut self, op_at: usize, remove: usize, text: &str) {
        let line = self.rope.char_to_line(op_at);
        let col0 = op_at == self.rope.line_to_char(line);
        let removed = self
            .rope
            .slice(op_at..op_at + remove)
            .chars()
            .filter(|c| *c == '\n')
            .count();
        let col = op_at - self.rope.line_to_char(line);
        self.shift_sem(
            line,
            col,
            remove,
            text.chars().count(),
            removed,
            text.matches('\n').count(),
        );
        self.shift_marks(line, col0, removed, text.matches('\n').count());
        self.note_wrap(line, removed, text.matches('\n').count());
        if remove > 0 {
            self.rope.remove(op_at..op_at + remove);
        }
        if !text.is_empty() {
            self.rope.insert(op_at, text);
        }
        self.forget_states(line, removed, text.matches('\n').count());
        self.rev += 1;
    }

    /// Extra carets and snippet stops point into the text as it was; after undo, redo or a
    /// reload they would land anywhere, even past the end.
    fn drop_positions(&mut self) {
        self.extras.clear();
        self.snip = None;
    }

    fn undo(&mut self) {
        self.drop_positions();
        if let Some(t) = self.undo.pop() {
            for op in t.ops.iter().rev() {
                self.splice(op.at, op.ins.chars().count(), &op.del);
            }
            self.cur = t.before.0;
            self.anchor = t.before.1;
            self.redo.push(t);
        }
    }

    fn redo(&mut self) {
        self.drop_positions();
        if let Some(t) = self.redo.pop() {
            for op in &t.ops {
                self.splice(op.at, op.del.chars().count(), &op.ins);
            }
            self.cur = t.after.0;
            self.anchor = t.after.1;
            self.undo.push(t);
        }
    }

    fn replace_sel(&mut self, s: &str) {
        let (a, b) = self.sel_range();
        self.edit(a, b, s);
        self.set(a + s.chars().count());
    }

    pub fn indent_unit(&self) -> String {
        if self.use_tabs {
            "\t".into()
        } else {
            " ".repeat(self.tab)
        }
    }

    fn leading_ws(&self, line: usize) -> String {
        self.rope
            .line(line)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect()
    }

    fn type_text(&mut self, s: &str) {
        let mut it = s.chars();
        let (Some(c), None) = (it.next(), it.next()) else {
            self.replace_sel(s);
            return;
        };
        let next = self.peek(self.cur);
        let prev = (self.cur > 0).then(|| self.rope.char(self.cur - 1));

        if self.lang != Lang::Plain {
            if self.has_sel() && (closer(c).is_some() || c == '"' || c == '\'') {
                let (a, b) = self.sel_range();
                let close = closer(c).unwrap_or(c);
                self.edit(b, b, &close.to_string());
                self.edit(a, a, &c.to_string());
                self.anchor = a + 1;
                self.cur = b + 1;
                return;
            }
            if !self.has_sel() {
                if next == Some(c) && matches!(c, ')' | ']' | '}' | '"' | '\'') {
                    self.set(self.cur + 1);
                    return;
                }
                let free_after = next
                    .is_none_or(|n| n.is_whitespace() || matches!(n, ')' | ']' | '}' | ',' | ';'));
                if let Some(close) = closer(c) {
                    if free_after {
                        let at = self.cur;
                        self.edit(at, at, &format!("{c}{close}"));
                        self.set(at + 1);
                        return;
                    }
                }
                if (c == '"' || c == '\'') && free_after && !prev.is_some_and(is_word) {
                    let at = self.cur;
                    self.edit(at, at, &format!("{c}{c}"));
                    self.set(at + 1);
                    return;
                }
                if c == '}' {
                    let (line, col) = self.pos(self.cur);
                    let start = self.rope.line_to_char(line);
                    let only_ws = self
                        .rope
                        .slice(start..self.cur)
                        .chars()
                        .all(|c| c == ' ' || c == '\t');
                    if only_ws && col > 0 {
                        let drop = if self.rope.char(self.cur - 1) == '\t' {
                            1
                        } else {
                            col.min(self.tab)
                        };
                        let at = self.cur - drop;
                        self.edit(at, self.cur, "}");
                        self.set(at + 1);
                        return;
                    }
                }
            }
        }
        self.replace_sel(s);
    }

    fn newline(&mut self) {
        let (a, b) = self.sel_range();
        let (line, col) = self.pos(a);
        let mut indent = self.leading_ws(line);
        indent.truncate(indent.chars().take(col).map(char::len_utf8).sum());
        let unit = self.indent_unit();
        let prev = (a > 0).then(|| self.rope.char(a - 1));
        let next = self.peek(b);
        if self.lang != Lang::Plain && prev == Some('{') {
            if next == Some('}') {
                let text = format!("\n{indent}{unit}\n{indent}");
                self.edit(a, b, &text);
                self.set(a + 1 + indent.chars().count() + unit.chars().count());
            } else {
                self.replace_sel(&format!("\n{indent}{unit}"));
            }
        } else {
            self.replace_sel(&format!("\n{indent}"));
        }
    }

    fn move_selection(&mut self, line: usize, vcol: usize, copy: bool) {
        if !self.has_sel() {
            return;
        }
        let (a, b) = self.sel_range();
        let at = self.pos_at_cell(line, vcol);
        if at > a && at < b || (!copy && (at == a || at == b)) {
            return;
        }
        let text: String = self.rope.slice(a..b).into();
        let len = b - a;
        let start = if copy {
            self.edit(at, at, &text);
            at
        } else if at >= b {
            self.edit(at, at, &text);
            self.edit(a, b, "");
            at - len
        } else {
            self.edit(a, b, "");
            self.edit(at, at, &text);
            at
        };
        self.anchor = start;
        self.cur = start + len;
    }

    fn line_end(&self, line: usize) -> usize {
        self.rope.line_to_char(line) + self.line_len(line)
    }

    fn new_line_below(&mut self) {
        let (line, _) = self.pos(self.cur);
        self.set(self.line_end(line));
        self.newline();
    }

    fn new_line_above(&mut self) {
        let (line, _) = self.pos(self.cur);
        let start = self.rope.line_to_char(line);
        let indent = self.leading_ws(line);
        self.edit(start, start, &format!("{indent}\n"));
        self.set(start + indent.chars().count());
    }

    fn complete_statement(&mut self) {
        let (line, _) = self.pos(self.cur);
        let text = self.line_text(line).into_owned();
        let code = text.trim();
        let end = self.rope.line_to_char(line) + text.trim_end().chars().count();
        self.set(end);
        let finished = code.is_empty()
            || code.starts_with('#')
            || code.contains("//")
            || code.ends_with(['}', '{', ';', ',', '(', ':', '\\']);
        if self.lang == Lang::Plain || finished {
            self.new_line_below();
            return;
        }
        let keyword = code
            .split(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or("");
        if matches!(keyword, "if" | "for" | "while" | "switch") && code.ends_with(')') {
            let indent = self.leading_ws(line);
            let unit = self.indent_unit();
            self.edit(end, end, &format!(" {{\n{indent}{unit}\n{indent}}}"));
            self.set(end + 3 + indent.chars().count() + unit.chars().count());
            return;
        }
        self.edit(end, end, ";");
        self.set(end + 1);
        self.new_line_below();
    }

    fn toggle_case(&mut self) {
        let keep_selection = self.has_sel();
        let (a, b) = if keep_selection {
            self.sel_range()
        } else {
            match self.word_range(self.cur) {
                Some(range) => range,
                None => return,
            }
        };
        let text: String = self.rope.slice(a..b).into();
        let next = if text.chars().any(char::is_lowercase) {
            text.to_uppercase()
        } else {
            text.to_lowercase()
        };
        if next == text {
            return;
        }
        let cur = self.cur;
        self.edit(a, b, &next);
        let end = a + next.chars().count();
        if keep_selection {
            self.anchor = a;
            self.cur = end;
        } else {
            self.set(cur.min(end));
        }
    }

    fn goto_bracket(&mut self) {
        let Some((a, b)) = self.matching_bracket(None) else {
            return;
        };
        let target = if self.cur == a || self.cur == a + 1 {
            b
        } else {
            a
        };
        self.set(target);
    }

    fn backspace(&mut self) {
        if self.has_sel() {
            self.replace_sel("");
            return;
        }
        if self.cur == 0 {
            return;
        }
        let prev = self.rope.char(self.cur - 1);
        let next = self.peek(self.cur);
        if self.lang != Lang::Plain
            && next.is_some()
            && (closer(prev) == next || (matches!(prev, '"' | '\'') && next == Some(prev)))
        {
            let a = self.cur - 1;
            self.edit(a, a + 2, "");
            self.set(a);
            return;
        }
        let (line, col) = self.pos(self.cur);
        let start = self.rope.line_to_char(line);
        if prev == ' '
            && !self.use_tabs
            && self.rope.slice(start..self.cur).chars().all(|c| c == ' ')
        {
            let drop = match col % self.tab {
                0 => self.tab,
                r => r,
            };
            let a = self.cur - drop;
            self.edit(a, self.cur, "");
            self.set(a);
            return;
        }
        let a = self.cur - 1;
        self.edit(a, self.cur, "");
        self.set(a);
    }

    fn sel_lines(&self) -> (usize, usize) {
        let (a, b) = self.sel_range();
        let (first, _) = self.pos(a);
        let (mut last, col) = self.pos(b);
        if last > first && col == 0 {
            last -= 1;
        }
        (first, last)
    }

    fn indent_lines(&mut self, remove: bool) {
        let (first, last) = self.sel_lines();
        let unit = self.indent_unit();
        let (mut cur, mut anchor) = (self.cur, self.anchor);
        for line in (first..=last).rev() {
            let start = self.rope.line_to_char(line);
            let (a, b, text): (usize, usize, &str) = if remove {
                let chars: Vec<char> = self.rope.line(line).chars().take(self.tab).collect();
                let n = if chars.first() == Some(&'\t') {
                    1
                } else {
                    chars.iter().take_while(|c| **c == ' ').count()
                };
                (start, start + n, "")
            } else {
                if self.line_len(line) == 0 {
                    continue;
                }
                (start, start, &unit)
            };
            self.edit(a, b, text);
            let delta = text.chars().count() as isize - (b - a) as isize;
            let shift = |p: usize| {
                if p >= b {
                    (p as isize + delta) as usize
                } else if p > a {
                    a
                } else {
                    p
                }
            };
            cur = shift(cur);
            anchor = shift(anchor);
        }
        self.cur = cur;
        self.anchor = anchor;
    }

    /// Comments each line out as its own block comment, for languages without a line comment.
    fn toggle_block_comment(&mut self, (open, close): (&str, &str)) {
        let (first, last) = self.sel_lines();
        let lines: Vec<usize> = (first..=last).filter(|&l| !self.line_text(l).trim().is_empty()).collect();
        if lines.is_empty() {
            return;
        }
        let wrapped = |t: &str| t.trim().starts_with(open) && t.trim_end().ends_with(close) && t.trim().len() >= open.len() + close.len();
        let all = lines.iter().all(|&l| wrapped(&self.line_text(l)));
        let (mut cur, mut anchor) = (self.cur, self.anchor);
        for &line in lines.iter().rev() {
            let text = self.line_text(line).into_owned();
            let start = self.rope.line_to_char(line);
            let lead = text.chars().take_while(|c| c.is_whitespace()).count();
            let body_end = text.trim_end().chars().count();
            // The closing mark first, so the opening one does not move it.
            let edits: Vec<(usize, usize, String)> = if all {
                let inner = &text.trim()[open.len()..text.trim().len() - close.len()];
                let pre = inner.starts_with(' ') as usize;
                let post = inner.ends_with(' ') as usize;
                let c = close.chars().count();
                vec![
                    (start + body_end - c - post, start + body_end, String::new()),
                    (start + lead, start + lead + open.chars().count() + pre, String::new()),
                ]
            } else {
                vec![(start + body_end, start + body_end, format!(" {close}")), (start + lead, start + lead, format!("{open} "))]
            };
            for (a, b, ins) in edits {
                self.edit(a, b, &ins);
                let delta = ins.chars().count() as isize - (b - a) as isize;
                let shift = |p: usize| if p >= b { (p as isize + delta) as usize } else if p > a { a } else { p };
                cur = shift(cur);
                anchor = shift(anchor);
            }
        }
        self.cur = cur;
        self.anchor = anchor;
    }

    fn toggle_comment(&mut self) {
        let Some(mark) = self.lang.line_comment() else {
            if let Some(pair) = self.lang.block_comment() {
                self.toggle_block_comment(pair);
            }
            return;
        };
        let (first, last) = self.sel_lines();
        let lines: Vec<usize> = (first..=last)
            .filter(|&l| !self.line_text(l).trim().is_empty())
            .collect();
        if lines.is_empty() {
            return;
        }
        let all = lines
            .iter()
            .all(|&l| self.line_text(l).trim_start().starts_with(mark));
        let col = lines
            .iter()
            .map(|&l| self.leading_ws(l).chars().count())
            .min()
            .unwrap_or(0);
        let (mut cur, mut anchor) = (self.cur, self.anchor);
        for &line in lines.iter().rev() {
            let start = self.rope.line_to_char(line);
            let (a, b, ins) = if all {
                let text = self.line_text(line);
                let at = text.chars().take_while(|c| c.is_whitespace()).count();
                let spaced = text.trim_start()[mark.len()..].starts_with(' ');
                (
                    start + at,
                    start + at + mark.len() + spaced as usize,
                    String::new(),
                )
            } else {
                (start + col, start + col, format!("{mark} "))
            };
            self.edit(a, b, &ins);
            let delta = ins.chars().count() as isize - (b - a) as isize;
            let shift = |p: usize| {
                if p >= b {
                    (p as isize + delta) as usize
                } else if p > a {
                    a
                } else {
                    p
                }
            };
            cur = shift(cur);
            anchor = shift(anchor);
        }
        self.cur = cur;
        self.anchor = anchor;
    }

    fn duplicate(&mut self) {
        if self.has_sel() {
            let (a, b) = self.sel_range();
            let text: String = self.rope.slice(a..b).into();
            self.edit(b, b, &text);
            self.anchor = b;
            self.cur = b + text.chars().count();
            return;
        }
        let (line, col) = self.pos(self.cur);
        let text = self.line_text(line).into_owned();
        let end = self.rope.line_to_char(line) + self.line_len(line);
        self.edit(end, end, &format!("\n{text}"));
        let next = self.at(line + 1, col);
        self.set(next);
    }

    fn delete_lines(&mut self) {
        let (first, last) = self.sel_lines();
        let a = self.rope.line_to_char(first);
        let total = self.rope.len_lines();
        let b = if last + 1 < total {
            self.rope.line_to_char(last + 1)
        } else {
            self.rope.len_chars()
        };
        let (a, keep) = if last + 1 >= total && a > 0 {
            (a - 1, first - 1)
        } else {
            (a, first)
        };
        self.edit(a, b, "");
        let to = self.at(keep, 0);
        self.set(to);
    }

    fn move_lines(&mut self, up: bool) {
        let (first, last) = self.sel_lines();
        let total = self.rope.len_lines();
        if (up && first == 0) || (!up && last + 1 >= total) {
            return;
        }
        let (a_line, b_line) = if up {
            (first - 1, last)
        } else {
            (first, last + 1)
        };
        let start = self.rope.line_to_char(a_line);
        let end = self.rope.line_to_char(b_line) + self.line_len(b_line);
        let lines: Vec<String> = (a_line..=b_line)
            .map(|l| self.line_text(l).into_owned())
            .collect();
        let mut out = lines.clone();
        out.rotate_left(if up { 1 } else { lines.len() - 1 });
        let hop = if up {
            lines[0].chars().count() as isize + 1
        } else {
            lines[lines.len() - 1].chars().count() as isize + 1
        };
        let delta = if up { -hop } else { hop };
        let (cur, anchor) = (self.cur, self.anchor);
        self.edit(start, end, &out.join("\n"));
        // The end of a selection can sit at the start of the neighbour line; it may not move past the text.
        let n = self.rope.len_chars() as isize;
        self.cur = (cur as isize + delta).clamp(0, n) as usize;
        self.anchor = (anchor as isize + delta).clamp(0, n) as usize;
    }

    fn join_lines(&mut self) {
        let (line, _) = self.pos(self.cur);
        if line + 1 >= self.rope.len_lines() {
            return;
        }
        let end = self.rope.line_to_char(line) + self.line_len(line);
        let ws = self.leading_ws(line + 1).chars().count();
        self.edit(end, end + 1 + ws, " ");
        self.set(end + 1);
    }

    fn expand_selection(&mut self) {
        let (a, b) = self.sel_range();
        self.sel_stack.push((self.cur, self.anchor));
        if a == b {
            if let Some((wa, wb)) = self.word_range(a) {
                self.anchor = wa;
                self.cur = wb;
                return;
            }
        }
        let n = self.rope.len_chars();
        let mut depth = 0;
        let mut i = a;
        let mut open = None;
        while i > 0 && a - i < 200_000 {
            i -= 1;
            match self.rope.char(i) {
                ')' | ']' | '}' => depth += 1,
                '(' | '[' | '{' => {
                    if depth == 0 {
                        open = Some(i);
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        let close = open.and_then(|o| {
            let mut depth = 0;
            for i in b..n.min(b + 200_000) {
                match self.rope.char(i) {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => {
                        if depth == 0 {
                            return Some((o, i));
                        }
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            None
        });
        let (na, nb) = match close {
            Some((o, c)) if (o + 1, c) != (a, b) => (o + 1, c),
            Some((o, c)) => (o, c + 1),
            None => (0, n),
        };
        self.anchor = na;
        self.cur = nb;
    }

    /// The bracket at the caret (or just before it) and its partner. Brackets in strings and
    /// comments don't count, as in VS Code. With a `budget`, the search gives up after that long
    /// (VS Code gives the highlight 20 ms); a jump asks without one.
    fn matching_bracket(&mut self, budget: Option<std::time::Duration>) -> Option<(usize, usize)> {
        let key = (self.cur, self.rev);
        if let Some((k, v)) = self.pair_cache {
            if k == key {
                return v;
            }
        }
        let deadline = budget.map(|b| Instant::now() + b);
        let n = self.rope.len_chars();
        let opposite = |c: char| match c {
            ')' => Some('('),
            ']' => Some('['),
            '}' => Some('{'),
            _ => None,
        };
        let mut found = Some(None);
        for start in [Some(self.cur), self.cur.checked_sub(1)].into_iter().flatten().filter(|&i| i < n) {
            let c = self.rope.char(start);
            let (open, close, forward) = match (closer(c), opposite(c)) {
                (Some(close), _) => (c, close, true),
                (_, Some(open)) => (open, c, false),
                _ => continue,
            };
            let line = self.rope.char_to_line(start);
            if in_ranges(&self.quiet_ranges(line), start) {
                continue;
            }
            found = self.scan_bracket(start, open, close, forward, deadline).map(|m| m.map(|m| if forward { (start, m) } else { (m, start) }));
            break;
        }
        // Out of time: not remembered, so that a jump still looks for it in full.
        let found = found?;
        self.pair_cache = Some((key, found));
        found
    }

    /// The partner of the bracket at `start`, at most `LIMIT` characters away. `None` when the
    /// time ran out.
    fn scan_bracket(&mut self, start: usize, open: char, close: char, forward: bool, deadline: Option<Instant>) -> Option<Option<usize>> {
        const LIMIT: usize = 20_000;
        let n = self.rope.len_chars();
        let mut at: Box<dyn Iterator<Item = usize>> = if forward {
            Box::new(start + 1..n.min(start + LIMIT))
        } else {
            Box::new((start.saturating_sub(LIMIT)..start).rev())
        };
        let (mut line, mut quiet) = (usize::MAX, vec![]);
        let mut depth = 0;
        at.find_map(|i| {
            let d = self.rope.char(i);
            if d != open && d != close {
                return None;
            }
            let l = self.rope.char_to_line(i);
            if l != line {
                if deadline.is_some_and(|t| Instant::now() > t) {
                    return Some(None);
                }
                line = l;
                quiet = self.quiet_ranges(l);
            }
            if in_ranges(&quiet, i) {
                return None;
            }
            if (d == close) == forward {
                if depth == 0 {
                    return Some(Some(i));
                }
                depth -= 1;
            } else {
                depth += 1;
            }
            None
        })
        .map_or(Some(None), |r| r.map(Some))
    }

    /// Where `line` has strings and comments, as character positions in the whole text. Nothing
    /// when the line's state is not known yet: its brackets then all count.
    fn quiet_ranges(&mut self, line: usize) -> Vec<(usize, usize)> {
        let Some(st) = self.state_of(line) else { return vec![] };
        let text = self.line_text(line).into_owned();
        let mut toks = vec![];
        self.lex_cached(&text, st, &mut toks);
        let slice = self.rope.line(line);
        let base = self.rope.line_to_char(line);
        toks.iter()
            .filter(|t| matches!(t.tag, lexer::COMMENT | lexer::STRING | lexer::ESCAPE))
            .map(|t| (base + slice.byte_to_char(t.start as usize), base + slice.byte_to_char((t.end as usize).min(text.len()))))
            .collect()
    }

    pub fn selection_lines(&self) -> Option<(usize, usize)> {
        self.has_sel().then(|| self.sel_lines())
    }

    pub fn apply_byte_replacements(&mut self, mut edits: Vec<(usize, usize, String)>) -> usize {
        edits.sort_by_key(|e| std::cmp::Reverse(e.0));
        let count = edits.len();
        let mut cur = self.cur;
        // Applied from the end, so each one must stop before the start of the one applied before it.
        let mut limit = self.rope.len_bytes();
        self.begin();
        for (offset, length, text) in edits {
            if offset.saturating_add(length) > limit {
                continue;
            }
            let a = self.rope.byte_to_char(offset);
            let b = self.rope.byte_to_char(offset + length);
            // An offset inside a character starts at that character, so it reaches a little further.
            limit = self.rope.char_to_byte(a);
            self.edit(a, b, &text);
            let inserted = text.chars().count();
            if cur >= b {
                cur = cur + inserted - (b - a);
            } else if cur > a {
                cur = a + inserted.min(cur - a);
            }
        }
        self.set(cur);
        self.commit(false);
        count
    }

    pub fn apply_utf16_edits(&mut self, mut edits: Vec<(usize, usize, usize, usize, String)>) {
        edits.sort_by_key(|e| std::cmp::Reverse((e.0, e.1)));
        let (line, col) = self.pos(self.cur);
        self.begin();
        for (sl, sc, el, ec, text) in edits {
            let a = self.at(sl, self.col_from_utf16(sl, sc));
            let b = self.at(el, self.col_from_utf16(el, ec));
            self.edit(a, b, &text);
        }
        let to = self.at(line, col);
        self.set(to);
        self.commit(false);
    }

    fn word_left(&self) -> usize {
        let mut i = self.cur;
        while i > 0 && self.rope.char(i - 1).is_whitespace() && self.rope.char(i - 1) != '\n' {
            i -= 1;
        }
        if i == self.cur && i > 0 && self.rope.char(i - 1) == '\n' {
            return i - 1;
        }
        if i > 0 {
            let w = is_word(self.rope.char(i - 1));
            while i > 0 {
                let c = self.rope.char(i - 1);
                if c.is_whitespace() || is_word(c) != w {
                    break;
                }
                i -= 1;
            }
        }
        i
    }

    fn word_right(&self) -> usize {
        let n = self.rope.len_chars();
        let mut i = self.cur;
        if i < n && self.rope.char(i) == '\n' {
            return i + 1;
        }
        if i < n && !self.rope.char(i).is_whitespace() {
            let w = is_word(self.rope.char(i));
            while i < n {
                let c = self.rope.char(i);
                if c.is_whitespace() || is_word(c) != w {
                    break;
                }
                i += 1;
            }
        }
        while i < n && self.rope.char(i).is_whitespace() && self.rope.char(i) != '\n' {
            i += 1;
        }
        i
    }

    fn motion(&mut self, to: Motion, ext: bool, rows: usize) {
        use Motion::*;
        let (line, col) = self.pos(self.cur);
        let last = self.rope.len_lines() - 1;
        let vertical = matches!(to, Up | Down | PageUp | PageDown);
        // Only up and down need the row and the column to aim for; on a very long line they cost a full walk.
        let (row, want) = if vertical {
            let (row, cell) = self.cell_of(self.cur);
            (row, self.want.unwrap_or(cell))
        } else {
            (0, 0)
        };
        let wrapped = self.wrap > 0;
        let target = match to {
            Left if self.has_sel() && !ext => self.sel_range().0,
            Right if self.has_sel() && !ext => self.sel_range().1,
            Left => self.cur.saturating_sub(1),
            Right => (self.cur + 1).min(self.rope.len_chars()),
            Up if (!wrapped && line == 0) || (wrapped && row == 0) => 0,
            Down if (!wrapped && line == last) || (wrapped && row + 1 >= self.row_count()) => self.rope.len_chars(),
            Up | PageUp => {
                let r = row.saturating_sub(if to == Up { 1 } else { rows });
                self.pos_at_cell(r, want)
            }
            Down | PageDown => {
                let r = row + if to == Down { 1 } else { rows };
                self.pos_at_cell(r.min(self.row_count() - 1), want)
            }
            Home => {
                let first = self.leading_ws(line).chars().count();
                self.at(line, if col == first { 0 } else { first })
            }
            End => self.at(line, usize::MAX),
            WordLeft => self.word_left(),
            WordRight => self.word_right(),
            DocStart => 0,
            DocEnd => self.rope.len_chars(),
        };
        self.cur = target;
        if !ext {
            self.anchor = target;
        }
        self.want = vertical.then_some(want);
    }

    fn select_word(&mut self, at: usize) {
        let n = self.rope.len_chars();
        let on = |i: usize| i < n && is_word(self.rope.char(i));
        if !on(at) && !(at > 0 && on(at - 1)) {
            self.set(at);
            return;
        }
        let mut a = at;
        while a > 0 && on(a - 1) {
            a -= 1;
        }
        let mut b = at;
        while on(b) {
            b += 1;
        }
        self.anchor = a;
        self.cur = b;
    }

    /// Where the query is in `text`, as byte offsets, without overlaps. Ignoring case is for
    /// ASCII letters only; lowering them keeps every byte where it was.
    fn matches_in(&self, text: &str) -> Vec<usize> {
        let q = &self.query;
        if q.is_empty() || q.len() > text.len() {
            return vec![];
        }
        if self.case {
            return text.match_indices(q.as_str()).map(|(i, _)| i).collect();
        }
        let hay = text.to_ascii_lowercase();
        hay.match_indices(q.to_ascii_lowercase().as_str()).map(|(i, _)| i).collect()
    }

    pub fn set_search(&mut self, q: &str, case: bool) -> usize {
        self.query = q.lines().next().unwrap_or("").to_string();
        self.case = case;
        if self.query.is_empty() {
            return 0;
        }
        // The query has no line break, so no match crosses one: counting over the whole text
        // gives the same number as line by line, at a fraction of the cost.
        let mut text = self.rope.to_string();
        if case {
            return text.matches(self.query.as_str()).count();
        }
        text.make_ascii_lowercase();
        text.matches(self.query.to_ascii_lowercase().as_str()).count()
    }

    fn select_match(&mut self, line: usize, byte: usize, text: &str) {
        let a = self.rope.line_to_char(line) + text[..byte].chars().count();
        self.anchor = a;
        self.cur = a + self.query.chars().count();
    }

    fn find_next(&mut self, back: bool) -> bool {
        let total = self.rope.len_lines();
        let (a, b) = self.sel_range();
        let from = if back { a } else { b };
        let (line, col) = self.pos(from);
        for step in 0..=total {
            let l = if back {
                (line + total * 2 - step) % total
            } else {
                (line + step) % total
            };
            let text = self.line_text(l).into_owned();
            let at_col = |m: usize| text[..m].chars().count();
            let found = self.matches_in(&text);
            let pick = if back {
                found.iter().rev().find(|&&m| step > 0 || at_col(m) < col)
            } else {
                found.iter().find(|&&m| step > 0 || at_col(m) >= col)
            };
            if let Some(&m) = pick {
                self.select_match(l, m, &text);
                return true;
            }
        }
        false
    }

    fn sel_is_match(&self) -> bool {
        if !self.has_sel() {
            return false;
        }
        let (a, b) = self.sel_range();
        let s: String = self.rope.slice(a..b).into();
        if self.case {
            s == self.query
        } else {
            s.eq_ignore_ascii_case(&self.query)
        }
    }

    fn swap_cursor(&mut self, k: usize) {
        let other = self.extras[k];
        self.extras[k] = [self.cur, self.anchor];
        self.cur = other[0];
        self.anchor = other[1];
    }

    /// Runs a command once for every cursor, from the last one in the text to the first, so that
    /// an edit never moves a cursor that is still waiting.
    fn each_cursor(&mut self, cmd: &Cmd, typing: &mut bool) -> Result<(), String> {
        self.snip = None;
        let start = |d: &Doc, i: usize| {
            let c = if i == 0 { [d.cur, d.anchor] } else { d.extras[i - 1] };
            c[0].min(c[1])
        };
        let mut order: Vec<usize> = (0..=self.extras.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(start(self, i)));
        for i in order {
            if i > 0 {
                self.swap_cursor(i - 1);
            }
            self.want = None;
            let done = self.run_one(cmd.clone(), typing);
            if i > 0 {
                self.swap_cursor(i - 1);
            }
            done?;
        }
        self.merge_cursors();
        Ok(())
    }

    /// Joins cursors that touch or overlap, and keeps the one the caret was in as the main one.
    fn merge_cursors(&mut self) {
        let len = self.rope.len_chars();
        let main = self.cur.min(len);
        let mut all: Vec<[usize; 2]> = vec![[self.cur, self.anchor]];
        all.extend(self.extras.iter().copied());
        for c in &mut all {
            *c = [c[0].min(len), c[1].min(len)];
        }
        all.sort_by_key(|c| (c[0].min(c[1]), c[0].max(c[1])));
        let mut out: Vec<[usize; 2]> = vec![];
        for c in all {
            if let Some(last) = out.last_mut() {
                let (ls, le) = (last[0].min(last[1]), last[0].max(last[1]));
                let (cs, ce) = (c[0].min(c[1]), c[0].max(c[1]));
                if cs < le || cs == ls {
                    let end = le.max(ce);
                    *last = if last[0] >= last[1] { [end, ls] } else { [ls, end] };
                    continue;
                }
            }
            out.push(c);
        }
        let idx = out.iter().position(|c| main >= c[0].min(c[1]) && main <= c[0].max(c[1])).unwrap_or(0);
        let m = out.remove(idx);
        self.cur = m[0];
        self.anchor = m[1];
        self.extras = out;
    }

    /// Where the text of `[a, b)` appears next, after char `from`, going round the end once.
    fn find_text(&self, needle: &str, from: usize) -> Option<(usize, usize)> {
        let hay = self.rope.to_string();
        let from_byte = self.rope.char_to_byte(from.min(self.rope.len_chars()));
        let found = hay[from_byte..].find(needle).map(|i| from_byte + i).or_else(|| hay[..from_byte].find(needle))?;
        let a = self.rope.byte_to_char(found);
        Some((a, a + needle.chars().count()))
    }

    fn select_next(&mut self) {
        let (a, b) = self.sel_range();
        if a == b {
            if let Some((s, e)) = self.word_range(self.cur) {
                self.anchor = s;
                self.cur = e;
            }
            return;
        }
        let needle: String = self.rope.slice(a..b).into();
        let Some((s, e)) = self.find_text(&needle, b) else { return };
        let taken = std::iter::once([self.cur, self.anchor]).chain(self.extras.iter().copied()).any(|c| c[0].min(c[1]) < e && s < c[0].max(c[1]));
        if taken {
            return;
        }
        self.extras.push([self.cur, self.anchor]);
        self.anchor = s;
        self.cur = e;
    }

    fn select_all_matches(&mut self) {
        let (mut a, mut b) = self.sel_range();
        if a == b {
            let Some((s, e)) = self.word_range(self.cur) else { return };
            (a, b) = (s, e);
        }
        let needle: String = self.rope.slice(a..b).into();
        if needle.is_empty() || needle.contains('\n') {
            return;
        }
        let hay = self.rope.to_string();
        let mut found: Vec<[usize; 2]> = hay.match_indices(&needle).take(10_000).map(|(i, m)| {
            let s = self.rope.byte_to_char(i);
            [s + m.chars().count(), s]
        }).collect();
        let Some(main) = found.pop() else { return };
        self.cur = main[0];
        self.anchor = main[1];
        self.extras = found;
    }

    fn add_cursor_vertical(&mut self, up: bool) {
        let all: Vec<[usize; 2]> = std::iter::once([self.cur, self.anchor]).chain(self.extras.iter().copied()).collect();
        let edge = if up { all.iter().map(|c| c[0]).min() } else { all.iter().map(|c| c[0]).max() }.unwrap_or(self.cur);
        let (line, col) = self.pos(edge);
        let target = if up { line.checked_sub(1) } else { Some(line + 1).filter(|l| *l < self.rope.len_lines()) };
        let Some(target) = target else { return };
        let v = self.want.unwrap_or_else(|| self.vcol(line, col));
        let at = self.at(target, self.col_from_vcol(target, v));
        self.extras.push([self.cur, self.anchor]);
        self.cur = at;
        self.anchor = at;
        self.merge_cursors();
    }

    /// Goes to the next (or previous) tab stop of a snippet. Says whether there was one.
    fn next_stop(&mut self, back: bool) -> bool {
        let Some((stops, at)) = self.snip.take() else { return false };
        let len = self.rope.len_chars();
        let here = stops[at];
        // Leaving the snippet by moving the caret away ends it.
        if self.cur < here[0].min(len) || self.cur > here[1].min(len) + 1 {
            return false;
        }
        let to = if back { at.checked_sub(1) } else { Some(at + 1).filter(|n| *n < stops.len()) };
        let Some(to) = to else {
            // Shift+Tab on the first stop stays there.
            if back {
                self.snip = Some((stops, at));
            }
            return back;
        };
        let target = stops[to];
        self.anchor = target[0].min(len);
        self.cur = target[1].min(len);
        // The last stop is where the snippet ends.
        if to + 1 < stops.len() {
            self.snip = Some((stops, to));
        }
        true
    }

    fn tidy(&mut self, trim: bool, newline: bool) {
        let (mut cur, mut anchor) = (self.cur, self.anchor);
        // Two spaces at the end of a line mean a line break in Markdown.
        if trim && self.lang.label() != "Markdown" {
            for l in (0..self.rope.len_lines()).rev() {
                let text = self.line_text(l).into_owned();
                let kept = text.trim_end_matches([' ', '\t']).chars().count();
                let all = text.chars().count();
                if kept == all {
                    continue;
                }
                let start = self.rope.line_to_char(l);
                let (a, b) = (start + kept, start + all);
                self.edit(a, b, "");
                let shift = |p: usize| if p >= b { p - (b - a) } else if p > a { a } else { p };
                cur = shift(cur);
                anchor = shift(anchor);
            }
        }
        self.cur = cur;
        self.anchor = anchor;
        let len = self.rope.len_chars();
        if newline && len > 0 && self.rope.char(len - 1) != '\n' {
            self.edit(len, len, "\n");
        }
    }

    fn replace_all(&mut self, with: &str) {
        for l in (0..self.rope.len_lines()).rev() {
            let text = self.line_text(l).into_owned();
            let qlen = self.query.chars().count();
            for &m in self.matches_in(&text).iter().rev() {
                let a = self.rope.line_to_char(l) + text[..m].chars().count();
                self.edit(a, a + qlen, with);
            }
        }
        let c = self.cur.min(self.rope.len_chars());
        self.set(c);
    }

    pub fn run(&mut self, cmd: Cmd) -> Result<(), String> {
        use Cmd::*;
        if let SetWrap { cols } = cmd {
            self.wrap = cols.clamp(0, 10_000);
            return Ok(());
        }
        if self.readonly && cmd.mutates() {
            return Ok(());
        }
        let keeps_want = matches!(cmd, Move { .. });
        if !matches!(cmd, ExpandSelection | ShrinkSelection) {
            self.sel_stack.clear();
        }
        let mut typing = false;
        self.begin();
        if self.extras.is_empty() {
            self.run_one(cmd, &mut typing)?;
        } else if cmd.multi() {
            self.each_cursor(&cmd, &mut typing)?;
        } else {
            if !cmd.keeps_cursors() {
                self.extras.clear();
            }
            self.run_one(cmd, &mut typing)?;
        }
        self.commit(typing);
        if !keeps_want {
            self.want = None;
        }
        Ok(())
    }

    fn run_one(&mut self, cmd: Cmd, typing: &mut bool) -> Result<(), String> {
        use Cmd::*;
        match cmd {
            Type { s } => {
                self.type_text(&s);
                *typing = true;
            }
            Insert { s } => {
                let s = s.replace("\r\n", "\n");
                self.replace_sel(&s);
            }
            Newline => self.newline(),
            Backspace => {
                self.backspace();
                *typing = true;
            }
            Delete => {
                if self.has_sel() {
                    self.replace_sel("");
                } else if self.cur < self.rope.len_chars() {
                    self.edit(self.cur, self.cur + 1, "");
                }
                *typing = true;
            }
            DeleteWordLeft => {
                if !self.has_sel() {
                    self.anchor = self.word_left();
                }
                self.replace_sel("");
            }
            DeleteWordRight => {
                if !self.has_sel() {
                    self.anchor = self.word_right();
                }
                self.replace_sel("");
            }
            Tab if self.next_stop(false) => {}
            Outdent if self.next_stop(true) => {}
            Tab => {
                if self.has_sel() && self.pos(self.cur).0 != self.pos(self.anchor).0 {
                    self.indent_lines(false);
                } else if self.use_tabs {
                    self.replace_sel("\t");
                } else {
                    let (a, _) = self.sel_range();
                    let (line, col) = self.pos(a);
                    let v = self.vcol(line, col);
                    self.replace_sel(&" ".repeat(self.tab - v % self.tab));
                }
            }
            Outdent => self.indent_lines(true),
            Move { to, ext, rows } => self.motion(to, ext, rows.unwrap_or(30)),
            Click { line, vcol, ext } => {
                self.cur = self.pos_at_cell(line, vcol);
                if !ext {
                    self.anchor = self.cur;
                }
            }
            Word { line, vcol } => {
                let at = self.pos_at_cell(line, vcol);
                self.select_word(at);
            }
            SelectLine { line } => {
                let line = self.row_to_line(line);
                self.anchor = self.rope.line_to_char(line);
                self.cur = if line + 1 < self.rope.len_lines() {
                    self.rope.line_to_char(line + 1)
                } else {
                    self.rope.len_chars()
                };
            }
            SelectAll => {
                self.anchor = 0;
                self.cur = self.rope.len_chars();
            }
            MoveSelection { line, vcol, copy } => self.move_selection(line, vcol, copy),
            NewLineBelow => self.new_line_below(),
            NewLineAbove => self.new_line_above(),
            CompleteStatement => self.complete_statement(),
            ToggleCase => self.toggle_case(),
            GotoBracket => self.goto_bracket(),
            Undo => self.undo(),
            Redo => self.redo(),
            ToggleComment => self.toggle_comment(),
            Tidy { trim, newline } => self.tidy(trim, newline),
            DuplicateLine => self.duplicate(),
            DeleteLine => self.delete_lines(),
            CutSelection => {
                if self.has_sel() {
                    self.replace_sel("");
                } else {
                    self.delete_lines();
                }
            }
            AddCursor { line, vcol } => {
                let at = self.pos_at_cell(line, vcol);
                let main_is_caret = self.cur == self.anchor;
                if let Some(k) = self.extras.iter().position(|c| c[0] == at && c[1] == at) {
                    self.extras.remove(k);
                } else if at == self.cur && main_is_caret {
                    if let Some(m) = self.extras.pop() {
                        self.cur = m[0];
                        self.anchor = m[1];
                    }
                } else {
                    self.extras.push([self.cur, self.anchor]);
                    self.cur = at;
                    self.anchor = at;
                }
                self.merge_cursors();
            }
            SelectNext => self.select_next(),
            SelectAllMatches => self.select_all_matches(),
            AddCursorVertical { up } => self.add_cursor_vertical(up),
            ClearCursors => self.extras.clear(),
            SetWrap { .. } => {}
            Snippet { s, stops } => {
                // As with a completion: the prefix ends at the caret, either end of a selection.
                let n = self.prefix().chars().count();
                let (a, b) = self.sel_range();
                let from = (self.cur - n).min(a);
                self.edit(from, b.max(self.cur), &s);
                // Stops come from the snippet's text; one past its end would put the caret outside.
                let len = s.chars().count();
                let absolute: Vec<[usize; 2]> = stops.iter().map(|r| [from + r[0].min(len), from + r[1].min(len)]).collect();
                self.snip = None;
                match absolute.first() {
                    Some(first) => {
                        self.anchor = first[0];
                        self.cur = first[1];
                        if absolute.len() > 1 {
                            self.snip = Some((absolute, 0));
                        }
                    }
                    None => self.set(from + s.chars().count()),
                }
            }
            Complete { s, back } => {
                // The typed prefix ends at the caret, which may be either end of a selection.
                let n = self.prefix().chars().count();
                let (a, b) = self.sel_range();
                let start = (self.cur - n).min(a);
                self.edit(start, b.max(self.cur), &s);
                self.set((start + s.chars().count()).saturating_sub(back));
            }
            Goto { line, col } => {
                let c = self.col_from_utf16(line, col);
                let at = self.at(line, c);
                self.set(at);
            }
            FindNext { back } => {
                self.find_next(back);
            }
            ReplaceOne { s } => {
                if self.sel_is_match() {
                    self.replace_sel(&s);
                }
                self.find_next(false);
            }
            ReplaceAll { s } => self.replace_all(&s),
            Fresh => self.sent_clip = None,
            MoveLines { up } => self.move_lines(up),
            JoinLines => self.join_lines(),
            ExpandSelection => self.expand_selection(),
            ShrinkSelection => {
                if let Some((c, a)) = self.sel_stack.pop() {
                    self.cur = c;
                    self.anchor = a;
                }
            }
            ToggleFold { row } => {
                let line = self.row_to_line(row);
                let folded = self.folds.iter().any(|f| f.0 == line);
                self.set_fold(line, !folded);
            }
            FoldCaret { fold } => self.fold_caret(fold),
            FoldAll { fold } => self.fold_all(fold),
            ToggleBreakpoint { row } => {
                let line = row.map_or_else(|| self.pos(self.cur).0, |r| self.row_to_line(r));
                self.toggle_breakpoint(line);
            }
            SetBreakpoints { lines } => {
                self.bps = lines;
                self.bps.sort_unstable();
                self.bps.dedup();
            }
            SetExec { line } => self.exec = line,
        }
        Ok(())
    }

    /// Computes the line states up to `upto`. Grammars are slower than the hand-written lexers,
    /// so on a jump far down a huge file this stops when the time is up and says `false`. The
    /// next call goes on from there, so nothing is ever colored from a wrong state.
    fn ensure_states(&mut self, upto: usize) -> bool {
        let mut scratch = Vec::new();
        let timed = matches!(self.lang, Lang::Grammar(_));
        let deadline = self.state_deadline.unwrap_or_else(|| Instant::now() + std::time::Duration::from_millis(150));
        while self.valid <= upto {
            let k = self.valid;
            if k < self.states.len() && !self.check[k] {
                self.valid += 1;
                continue;
            }
            if timed && Instant::now() > deadline {
                return false;
            }
            let st = self.states[k - 1];
            scratch.clear();
            let text: Cow<str> = self.rope.line(k - 1).into();
            let text = text.trim_end_matches('\n');
            // Lines drawn lately are known already. Only looked up: filling the cache from here,
            // while going through a whole file, would push the lines on screen out of it.
            let next = match self.cached_next(text, st) {
                Some(next) => next,
                None => lexer::lex(text, st, self.lang, &mut self.raws, &mut scratch),
            };
            #[cfg(test)]
            {
                self.worked += 1;
            }
            self.settle(k, next);
        }
        true
    }

    fn reset_states(&mut self) {
        self.states.truncate(1);
        self.check.truncate(1);
        self.valid = 1;
    }

    /// Lines `line..=line + removed` became `line..=line + added`. The states of the new lines
    /// are not known, and the one after them must be checked: the line before it changed.
    fn forget_states(&mut self, line: usize, removed: usize, added: usize) {
        self.valid = self.valid.min(line + 1);
        let len = self.states.len();
        let from = (line + 1).min(len);
        let to = (line + removed + 1).min(len);
        let new = if to < len || from < len { added } else { 0 };
        self.states.splice(from..to, std::iter::repeat_n(u32::MAX, new));
        self.check.splice(from..to, std::iter::repeat_n(true, new));
        if let Some(c) = self.check.get_mut(from + new) {
            *c = true;
        }
    }

    /// Line `k` starts in `next`: worked out from the line before, which is sure. When that
    /// differs from what was kept, the line after it must be checked too.
    fn settle(&mut self, k: usize, next: u32) {
        debug_assert_eq!(k, self.valid);
        if k < self.states.len() {
            if self.states[k] != next {
                self.states[k] = next;
                if let Some(c) = self.check.get_mut(k + 1) {
                    *c = true;
                }
            }
            self.check[k] = false;
        } else {
            self.states.push(next);
            self.check.push(false);
        }
        self.valid = k + 1;
    }

    /// The states worked out again from the first line, to compare with the kept ones.
    #[cfg(test)]
    pub fn states_from_scratch(&mut self) -> Result<(), String> {
        let total = self.rope.len_lines();
        self.state_deadline = Some(Instant::now() + std::time::Duration::from_secs(60));
        self.ensure_states(total);
        self.state_deadline = None;
        let (mut st, mut scratch, mut raws) = (0, vec![], self.raws.clone());
        for i in 0..total.min(self.valid) {
            if self.states[i] != st {
                return Err(format!("line {i} starts in state {} but should be {st}", self.states[i]));
            }
            scratch.clear();
            let text: Cow<str> = self.rope.line(i).into();
            st = lexer::lex(text.trim_end_matches('\n'), st, self.lang, &mut raws, &mut scratch);
        }
        Ok(())
    }

    /// Colors the lines in the background: works out the states of the lines that are not known
    /// yet for a few milliseconds, so that a jump far down the file finds them ready. Says whether
    /// there was anything left to do.
    pub fn warm(&mut self, ms: u64) -> bool {
        if !matches!(self.lang, Lang::Grammar(_)) {
            return false;
        }
        let total = self.rope.len_lines();
        if self.valid > total || total > 400_000 {
            return false;
        }
        self.state_deadline = Some(Instant::now() + std::time::Duration::from_millis(ms));
        self.ensure_states(total);
        self.state_deadline = None;
        true
    }

    /// The state a line starts in, when it is known.
    fn state_of(&mut self, line: usize) -> Option<u32> {
        self.ensure_states(line);
        (line < self.valid).then(|| self.states[line])
    }

    fn emit(
        &self,
        s: &str,
        tag: Option<u8>,
        vcol: &mut usize,
        out: &mut String,
        budget: &mut usize,
    ) {
        if s.is_empty() || *budget == 0 {
            return;
        }
        if let Some(t) = tag {
            out.push('<');
            out.push(t as char);
            out.push('>');
        }
        for c in s.chars() {
            if *budget == 0 {
                break;
            }
            *budget -= 1;
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '\t' => {
                    let next = self.step(*vcol, c);
                    out.extend(std::iter::repeat_n(' ', next - *vcol));
                    *vcol = next;
                    continue;
                }
                c if (c as u32) < 0x20 => out.push('\u{fffd}'),
                c => out.push(c),
            }
            *vcol += 1;
        }
        if let Some(t) = tag {
            out.push_str("</");
            out.push(t as char);
            out.push('>');
        }
    }

    fn neighbour_indent(&self, line: usize, up: bool) -> usize {
        let total = self.rope.len_lines();
        let mut l = line;
        for _ in 0..16 {
            if up {
                if l == 0 {
                    return 0;
                }
                l -= 1;
            } else {
                l += 1;
                if l >= total {
                    return 0;
                }
            }
            let text = self.line_text(l);
            if !text.trim().is_empty() {
                return text
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .fold(0, |v, c| self.step(v, c));
            }
        }
        0
    }

    fn push_guides(&self, out: &mut String, vcols: usize) {
        for unit in 0..vcols / self.tab {
            if unit > 0 {
                out.push_str("<g>");
            }
            out.extend(std::iter::repeat_n(' ', self.tab));
            if unit > 0 {
                out.push_str("</g>");
            }
        }
        out.extend(std::iter::repeat_n(' ', vcols % self.tab));
    }

    /// What a folded block shows after its first line: the line where it ends, without its
    /// indentation, so that `int main() {` and a `}` below it read `int main() { … }`.
    fn render_tail(&mut self, line: usize) -> String {
        let (mut toks, mut text) = (Vec::new(), String::new());
        self.render_line(line, &mut toks, 0, false, &mut text);
        let lead = text.len() - text.trim_start().len();
        let end = text.trim_end().len();
        let (mut out, mut pos, mut vcol, mut budget) = (String::new(), lead, 0, MAX_RENDER);
        for t in &toks {
            let (s, e) = ((t.start as usize).max(pos), (t.end as usize).min(end));
            if s >= e {
                continue;
            }
            self.emit(&text[pos..s], None, &mut vcol, &mut out, &mut budget);
            self.emit(&text[s..e], Some(t.tag), &mut vcol, &mut out, &mut budget);
            pos = e;
        }
        if pos < end {
            self.emit(&text[pos..end], None, &mut vcol, &mut out, &mut budget);
        }
        out
    }

    /// Like `lexer::lex`, remembering what a grammar found. The other languages have quick
    /// lexers of their own (and the C one keeps raw-string delimiters aside), so they skip it.
    fn lex_cached(&mut self, text: &str, st: u32, toks: &mut Vec<Tok>) -> u32 {
        // Grammars leave very long lines alone (see tm::MAX_LINE): hashing them would be the only cost.
        if !matches!(self.lang, Lang::Grammar(_)) || text.len() > 4096 {
            return lexer::lex(text, st, self.lang, &mut self.raws, toks);
        }
        let key = (text_hash(text), st);
        if let Some((t, next)) = self.tok_cache.get(&key) {
            toks.extend_from_slice(t);
            return *next;
        }
        let start = toks.len();
        let next = lexer::lex(text, st, self.lang, &mut self.raws, toks);
        if self.tok_cache.len() >= TOK_CACHE {
            self.tok_cache.clear();
        }
        self.tok_cache.insert(key, (toks[start..].to_vec(), next));
        next
    }

    fn cached_next(&self, text: &str, st: u32) -> Option<u32> {
        if !matches!(self.lang, Lang::Grammar(_)) || self.tok_cache.is_empty() || text.len() > 4096 {
            return None;
        }
        self.tok_cache.get(&(text_hash(text), st)).map(|c| c.1)
    }

    /// A likely state for the start of `line`, from the lines above that indent less (the
    /// function, class or block it is in), going back to one whose state is known. As in VS Code's
    /// guessStartState: right for most code, and never kept.
    fn guess_state(&mut self, line: usize) -> u32 {
        let indent = |d: &Doc, l: usize| d.rope.line(l).chars().position(|c| !c.is_whitespace());
        let mut want = indent(self, line).unwrap_or(0);
        let (mut chain, mut st) = (vec![], 0);
        let mut l = line;
        let floor = line.saturating_sub(20_000);
        while want > 0 && l > floor {
            l -= 1;
            let Some(ind) = indent(self, l) else { continue };
            if ind < want {
                chain.push(l);
                want = ind;
                if l < self.valid {
                    st = self.states[l];
                    break;
                }
            }
        }
        let mut scratch = vec![];
        for &l in chain.iter().rev() {
            let text = self.line_text(l).into_owned();
            scratch.clear();
            st = self.lex_cached(&text, st, &mut scratch);
        }
        st
    }

    /// The HTML of one row: part `seg` of line `i`.
    /// With `reuse`, `toks` already holds this line's tokens, from the row drawn just before.
    fn render_line(&mut self, i: usize, toks: &mut Vec<Tok>, seg: usize, reuse: bool, text: &mut String) -> (String, bool) {
        if !reuse {
            text.clear();
            text.push_str(&self.line_text(i));
        }
        let text: &str = text;
        let br = self.line_breaks(i);
        let seg = seg.min(br.len() - 1);
        // Through the rope, not by walking the characters: a row far into a long line is quick too.
        let slice = self.rope.line(i);
        let byte_of = |col: usize| slice.char_to_byte(col.min(slice.len_chars())).min(text.len());
        let (lo, hi) = (byte_of(br[seg].0), br.get(seg + 1).map_or(text.len(), |b| byte_of(b.0)));
        let (mut st, mut next) = (0, 0);
        if !reuse {
            toks.clear();
            if let Some(known) = self.state_of(i) {
                st = known;
                next = self.lex_cached(text, st, toks);
                if self.valid == i + 1 {
                    self.settle(i + 1, next);
                }
            } else {
                // Not worked out this far yet. Like VS Code (tokenizeHeuristically), the state is
                // guessed from the lines above that indent less, so the text shows colored at
                // once; `pending` has the view drawn again once the states are known.
                self.pending = true;
                st = match self.guess {
                    Some((l, g)) if l == i => g,
                    _ => self.guess_state(i),
                };
                next = self.lex_cached(text, st, toks);
                self.guess = Some((i + 1, next));
            }
            self.overlay_semantic(i, text, toks);
        }
        let code_lang = self.lang.is_code();
        // Sized for what one row can show, not for the whole line.
        let shown = (hi - lo).min(MAX_RENDER * 4);
        let mut out = String::with_capacity(shown + 16 * toks.len().min(shown));
        let (mut pos, mut vcol) = (lo, br[seg].1);
        let mut budget = MAX_RENDER;
        if code_lang && seg == 0 {
            if text.is_empty() {
                let indent = self
                    .neighbour_indent(i, true)
                    .min(self.neighbour_indent(i, false));
                self.push_guides(&mut out, indent);
            } else {
                let first_tok = toks.first().map_or(text.len(), |t| t.start as usize);
                let lead = text
                    .bytes()
                    .take(first_tok)
                    .take_while(|b| *b == b' ' || *b == b'\t')
                    .count();
                if lead <= hi {
                    let v = text[..lead].chars().fold(0, |v, c| self.step(v, c));
                    self.push_guides(&mut out, v);
                    pos = lead;
                    vcol = v;
                }
            }
        }
        // Only the tokens of this row: a long line has hundreds of thousands of them.
        let from = toks.partition_point(|t| (t.end as usize) <= pos);
        for t in toks[from..].iter().take_while(|t| (t.start as usize) < hi) {
            let (s, e) = ((t.start as usize).max(pos), (t.end as usize).min(hi));
            if s >= e {
                continue;
            }
            self.emit(&text[pos..s], None, &mut vcol, &mut out, &mut budget);
            self.emit(&text[s..e], Some(t.tag), &mut vcol, &mut out, &mut budget);
            pos = e;
        }
        self.emit(&text[pos..hi], None, &mut vcol, &mut out, &mut budget);
        if self.wrap == 0 {
            self.maxw = self.maxw.max(vcol);
        }

        // The tokens are in order, so the one holding a byte is found by halving, not by trying
        // them all for every byte (that was quadratic on a long line ending in a comment).
        let in_comment = |i: usize| {
            let k = toks.partition_point(|t| t.start as usize <= i);
            k > 0 && {
                let t = &toks[k - 1];
                (t.start as usize..t.end as usize).contains(&i) && t.tag == lexer::COMMENT
            }
        };
        let last_code = text.bytes().enumerate().rev().find(|&(i, b)| !b.is_ascii_whitespace() && !in_comment(i));
        let foldable = code_lang
            && seg == 0
            && (last_code.is_some_and(|(_, b)| b == b'{') || (st == 0 && lexer::in_block_comment(self.lang, next)));
        (out, foldable)
    }

    /// The visual columns of several byte offsets of `text`, sorted, in a single walk: one walk
    /// per offset was quadratic on a long line with many matches.
    fn vcols_of_bytes(&self, text: &str, bytes: &[usize]) -> Vec<usize> {
        let (mut v, mut at) = (0, 0);
        bytes
            .iter()
            .map(|&b| {
                v = text[at..b].chars().fold(v, |v, c| self.step(v, c));
                at = b;
                v
            })
            .collect()
    }

    pub fn view(&mut self, top: usize, rows: usize, reveal: bool) -> View {
        self.pending = false;
        self.guess = None;
        // Drawing runs on the window's thread, so it may not wait for the states: lines past
        // what is known are colored from a guess (see guess_state) and the background warmer
        // works the rest out, as in VS Code. A few milliseconds still cover a short jump.
        self.state_deadline = Some(Instant::now() + std::time::Duration::from_millis(4));
        let view = self.build_view(top, rows, reveal);
        self.state_deadline = None;
        View { pending: self.pending, ..view }
    }

    fn build_view(&mut self, top: usize, rows: usize, reveal: bool) -> View {
        let (cl, cc) = self.pos(self.cur);
        self.folds.retain(|f| !(cl > f.0 && cl <= f.1));
        let n = self.row_count();
        let total = self.rope.len_lines();
        let (cr, cell) = self.cell_of(self.cur);
        let mut top = top.min(n.saturating_sub(1));
        if reveal {
            if cr < top {
                top = cr;
            } else if rows > 0 && cr >= top + rows {
                top = cr + 1 - rows;
            }
        }
        let first = top.saturating_sub(OVERSCAN);
        let end = (top + rows + OVERSCAN).min(n);

        let mut toks = Vec::new();
        let mut lines = Vec::with_capacity(end - first);
        let mut nums = Vec::with_capacity(end - first);
        let mut cont = Vec::with_capacity(end - first);
        // For each row: its line, and the cells of the line that it shows, from and up to.
        let mut vis: Vec<(usize, usize, usize)> = Vec::with_capacity(end - first);
        let mut folds = vec![];
        let (mut lexed, mut text) = (None, String::new());
        for row in first..end {
            let (line, k) = self.locate(row);
            let br = self.line_breaks(line);
            let k = k.min(br.len() - 1);
            // The rows of a wrapped line follow each other: its tokens are worked out once.
            let (mut html, foldable) = self.render_line(line, &mut toks, k, lexed == Some(line), &mut text);
            lexed = Some(line);
            let folded = self.folds.iter().any(|f| f.0 == line);
            if folded && k == br.len() - 1 {
                html.push_str("<e> \u{2026} </e>");
                if let Some(end_line) = self.folds.iter().find(|f| f.0 == line).map(|f| f.1) {
                    html.push_str(&self.render_tail(end_line));
                }
            }
            if k == 0 && (folded || foldable) {
                folds.push([row, if folded { 2 } else { 1 }]);
            }
            lines.push(html);
            nums.push(line);
            cont.push(k > 0);
            vis.push((line, br[k].1, br.get(k + 1).map_or(usize::MAX, |b| b.1)));
        }
        // The row where a line starts, when that row is in the view.
        let in_view = |nums: &[usize], l: usize| {
            let lo = nums.partition_point(|&x| x < l);
            (lo < nums.len() && nums[lo] == l && vis[lo].1 == 0).then_some(first + lo)
        };
        // The part of the cells vs..ve of row i that falls in that row, as [row, from, to].
        let clip = |i: usize, vs: usize, ve: usize| -> Option<[usize; 3]> {
            let (_, s, e) = vis[i];
            let (a, b) = (vs.max(s), ve.min(e));
            (a < b).then(|| [first + i, a - s, b - s])
        };
        // Where a single cell of a line is shown.
        let point = |l: usize, v: usize| -> Option<[usize; 2]> {
            let lo = nums.partition_point(|&x| x < l);
            (lo..nums.len()).take_while(|&i| nums[i] == l).find(|&i| vis[i].1 <= v && v < vis[i].2).map(|i| [first + i, v - vis[i].1])
        };

        let (sa, sb) = self.sel_range();
        let mut sel = vec![];
        let mut carets = vec![];
        let ranges: Vec<(usize, usize)> = std::iter::once((sa, sb)).chain(self.extras.iter().map(|c| (c[0].min(c[1]), c[0].max(c[1])))).collect();
        for (k, &(ra, rb)) in ranges.iter().enumerate() {
            if ra != rb {
                let (al, ac) = self.pos(ra);
                let (bl, bc) = self.pos(rb);
                for (i, &l) in nums.iter().enumerate() {
                    if l < al || l > bl {
                        continue;
                    }
                    let vs = if l == al { self.vcol(l, ac) } else { 0 };
                    let ve = if l == bl { self.vcol(l, bc) } else { self.vcol(l, self.line_len(l)) + 1 };
                    sel.extend(clip(i, vs, ve));
                }
            }
            if k > 0 {
                let c = self.extras[k - 1][0];
                let (l, col) = self.pos(c);
                carets.extend(point(l, self.vcol(l, col)));
            }
        }

        // A wrapped line fills several rows of the view: it is searched once, and what is found
        // goes to the rows it falls in. Without wrapping, only the drawn part of a line counts.
        let spread = |found: &[usize], width: usize, i: usize, j: usize, out: &mut Vec<[usize; 3]>| {
            for (r, &(_, s, e)) in vis.iter().enumerate().take(j).skip(i) {
                let lo = found.partition_point(|&v| v + width <= s);
                for &v in found[lo..].iter().take_while(|&&v| v < e) {
                    out.extend(clip(r, v, v + width));
                }
            }
        };
        // Each line in the view with the rows i..j it fills.
        let mut groups: Vec<(usize, usize, usize)> = vec![];
        for (i, &l) in nums.iter().enumerate() {
            match groups.last_mut() {
                Some(g) if g.0 == l => g.2 = i + 1,
                _ => groups.push((l, i, i + 1)),
            }
        }

        let mut marks = vec![];
        if !self.query.is_empty() {
            let qw = self.query.chars().count();
            for &(l, i, j) in &groups {
                let full = self.line_text(l);
                let text = if self.wrap == 0 { drawn(&full) } else { &full };
                let found = self.vcols_of_bytes(text, &self.matches_in(text));
                spread(&found, qw, i, j, &mut marks);
            }
        }

        let mut occ = vec![];
        if sa == sb && self.lang != Lang::Diff {
            if let Some((a, b)) = self.word_range(self.cur).filter(|(a, b)| b - a >= 2) {
                let word: String = self.rope.slice(a..b).into();
                let chars = word.chars().count();
                for &(l, i, j) in &groups {
                    let full = self.line_text(l);
                    let text = if self.wrap == 0 { drawn(&full) } else { &full };
                    let whole: Vec<usize> = text
                        .match_indices(&word)
                        .map(|(m, _)| m)
                        .filter(|&m| !text[..m].chars().next_back().is_some_and(is_word) && !text[m + word.len()..].chars().next().is_some_and(is_word))
                        .collect();
                    let found = self.vcols_of_bytes(text, &whole);
                    spread(&found, chars, i, j, &mut occ);
                }
            }
        }

        let mut pair = vec![];
        if let Some((a, b)) = self.matching_bracket(Some(std::time::Duration::from_millis(20))) {
            for p in [a, b] {
                let (l, c) = self.pos(p);
                pair.extend(point(l, self.vcol(l, c)));
            }
        }

        let mut diags = vec![];
        let (mut errors, mut warnings) = (0, 0);
        let mut msg = None;
        for d in &self.diags {
            match d.sev {
                1 => errors += 1,
                2 => warnings += 1,
                _ => {}
            }
            let here = (d.sl, d.sc) <= (cl, cc) && (cl, cc) <= (d.el, d.ec);
            if here && msg.is_none() {
                msg = Some(d.msg.clone());
            }
            if d.sl >= total {
                continue;
            }
            for (i, &l) in nums.iter().enumerate() {
                if l < d.sl || l > d.el {
                    continue;
                }
                let len = self.line_len(l);
                let vs = if l == d.sl { self.vcol(l, d.sc.min(len)) } else { 0 };
                let ve = if l == d.el { self.vcol(l, d.ec.min(len)) } else { self.vcol(l, len) };
                if let Some([r, a, b]) = clip(i, vs, ve.max(vs + 1)) {
                    diags.push([r, a, b, d.sev]);
                }
            }
        }

        let bps = self.bps.iter().filter_map(|&l| in_view(&nums, l)).collect();
        let mut git = vec![];
        for &(a, b, kind) in &self.git_marks {
            for (i, &l) in nums.iter().enumerate() {
                if l >= a && l <= b {
                    git.push([first + i, kind as usize]);
                }
            }
        }
        let stripes = self
            .diags
            .iter()
            .filter(|d| d.sev <= 2 && d.sl < total)
            .take(400)
            .map(|d| [self.line_to_row(d.sl), d.sev])
            .collect();
        let exec = self.exec.and_then(|l| in_view(&nums, l));
        let all_bps = std::mem::take(&mut self.bps_changed).then(|| self.bps.clone());

        let key = (self.cur, self.anchor, self.rev);
        let clip = if self.sent_clip != Some(key) {
            self.sent_clip = Some(key);
            // Sent with every move, so the copy event can fill the clipboard at once. A huge one
            // is left out (empty means "ask for it"): copying it is rare, moving is not.
            Some(if self.clip_len() > CLIP_MAX { String::new() } else { self.clip_text() })
        } else {
            None
        };

        View {
            carets,
            lang: self.lang.label(),
            pending: false,
            rev: self.rev,
            n,
            top,
            first,
            lines,
            nums,
            cont,
            cur: [cr, cell],
            line: cl,
            col: cc + 1,
            total,
            crlf: self.crlf,
            use_tabs: self.use_tabs,
            tab: self.tab,
            readonly: self.readonly,
            folds,
            bps,
            git,
            exec,
            stripes,
            occ,
            pair,
            all_bps,
            dirty: self.is_dirty(),
            sel,
            marks,
            diags,
            msg,
            maxw: if self.wrap > 0 { self.wrap } else { self.maxw },
            clip,
            errors,
            warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpp(text: &str) -> Doc {
        Doc::new("/x/a.cpp".into(), text, None)
    }
    fn txt(d: &Doc) -> String {
        d.rope.to_string()
    }
    fn typed(d: &mut Doc, s: &str) {
        for c in s.chars() {
            if c == '\n' {
                d.run(Cmd::Newline).unwrap();
            } else {
                d.run(Cmd::Type { s: c.to_string() }).unwrap();
            }
        }
    }
    fn mv(d: &mut Doc, to: Motion, ext: bool) {
        d.run(Cmd::Move {
            to,
            ext,
            rows: None,
        })
        .unwrap();
    }

    fn select(d: &mut Doc, from: usize, to: usize) {
        d.anchor = from;
        d.cur = to;
    }

    fn drop_at(d: &mut Doc, vcol: usize, copy: bool) {
        d.run(Cmd::MoveSelection {
            line: 0,
            vcol,
            copy,
        })
        .unwrap();
    }

    #[test]
    fn dragging_a_selection_moves_it_in_one_undo_step() {
        let mut d = cpp("one two three");
        select(&mut d, 4, 8);
        drop_at(&mut d, 13, false);
        assert_eq!(txt(&d), "one threetwo ");
        assert_eq!(d.sel_range(), (9, 13));
        d.run(Cmd::Undo).unwrap();
        assert_eq!(txt(&d), "one two three");

        select(&mut d, 4, 8);
        drop_at(&mut d, 0, false);
        assert_eq!(txt(&d), "two one three");
        assert_eq!(d.sel_range(), (0, 4));
    }

    #[test]
    fn dropping_inside_the_selection_changes_nothing() {
        let mut d = cpp("one two");
        select(&mut d, 0, 4);
        drop_at(&mut d, 2, false);
        drop_at(&mut d, 4, false);
        drop_at(&mut d, 2, true);
        assert_eq!(txt(&d), "one two");
    }

    #[test]
    fn dragging_with_copy_keeps_the_original() {
        let mut d = cpp("ab cd");
        select(&mut d, 0, 2);
        drop_at(&mut d, 5, true);
        assert_eq!(txt(&d), "ab cdab");
        assert_eq!(d.sel_range(), (5, 7));
    }

    #[test]
    fn new_lines_above_and_below_keep_the_indent() {
        let mut d = cpp("    int a;");
        d.run(Cmd::Goto { line: 0, col: 6 }).unwrap();
        d.run(Cmd::NewLineBelow).unwrap();
        assert_eq!(txt(&d), "    int a;\n    ");
        assert_eq!(d.cursor_pos(), (1, 4));
        d.run(Cmd::NewLineAbove).unwrap();
        assert_eq!(txt(&d), "    int a;\n    \n    ");
        assert_eq!(d.cursor_pos(), (1, 4));
    }

    #[test]
    fn complete_statement_adds_what_is_missing() {
        let mut d = cpp("    foo(1)");
        d.run(Cmd::Goto { line: 0, col: 5 }).unwrap();
        d.run(Cmd::CompleteStatement).unwrap();
        assert_eq!(txt(&d), "    foo(1);\n    ");

        let mut d = cpp("if (x)");
        d.run(Cmd::CompleteStatement).unwrap();
        assert_eq!(txt(&d), "if (x) {\n    \n}");
        assert_eq!(d.cursor_pos(), (1, 4));

        let mut d = cpp("int a;");
        d.run(Cmd::CompleteStatement).unwrap();
        assert_eq!(txt(&d), "int a;\n");
    }

    #[test]
    fn toggle_case_flips_the_word_or_selection() {
        let mut d = cpp("foo Bar");
        d.run(Cmd::ToggleCase).unwrap();
        assert_eq!(txt(&d), "FOO Bar");
        d.run(Cmd::ToggleCase).unwrap();
        assert_eq!(txt(&d), "foo Bar");
        select(&mut d, 4, 7);
        d.run(Cmd::ToggleCase).unwrap();
        assert_eq!(txt(&d), "foo BAR");
        assert_eq!(d.sel_range(), (4, 7));
    }

    #[test]
    fn goto_bracket_jumps_back_and_forth() {
        let mut d = cpp("f(a, b)");
        d.run(Cmd::Goto { line: 0, col: 1 }).unwrap();
        d.run(Cmd::GotoBracket).unwrap();
        assert_eq!(d.cursor_pos(), (0, 6));
        d.run(Cmd::GotoBracket).unwrap();
        assert_eq!(d.cursor_pos(), (0, 1));
    }

    #[test]
    fn comments_and_strings_are_detected_at_the_caret() {
        let mut d = cpp("int a; // note");
        d.run(Cmd::Goto { line: 0, col: 5 }).unwrap();
        assert!(!d.after_comment_or_string());
        d.run(Cmd::Goto { line: 0, col: 12 }).unwrap();
        assert!(d.after_comment_or_string());

        let mut d = cpp("f(\"abc\");");
        d.run(Cmd::Goto { line: 0, col: 5 }).unwrap();
        assert!(d.after_comment_or_string());
    }

    #[test]
    fn auto_pairs_and_type_over() {
        let mut d = cpp("");
        typed(&mut d, "f(");
        assert_eq!(txt(&d), "f()");
        typed(&mut d, ")");
        assert_eq!(txt(&d), "f()");
        assert_eq!(d.cur, 3);
    }

    #[test]
    fn enter_between_braces_opens_a_block() {
        let mut d = cpp("");
        typed(&mut d, "int f() {\n");
        assert_eq!(txt(&d), "int f() {\n    \n}");
        assert_eq!(d.cursor_pos(), (1, 4));
        typed(&mut d, "x;");
        assert_eq!(txt(&d), "int f() {\n    x;\n}");
    }

    #[test]
    fn typing_coalesces_into_one_undo_step_until_a_space() {
        let mut d = cpp("");
        typed(&mut d, "foo bar");
        assert_eq!(txt(&d), "foo bar");
        d.run(Cmd::Undo).unwrap();
        assert_eq!(txt(&d), "foo");
        d.run(Cmd::Undo).unwrap();
        assert_eq!(txt(&d), "");
        d.run(Cmd::Redo).unwrap();
        d.run(Cmd::Redo).unwrap();
        assert_eq!(txt(&d), "foo bar");
    }

    #[test]
    fn undo_restores_multi_op_edits_and_selection() {
        let mut d = cpp("a\nb\nc\n");
        d.run(Cmd::SelectAll).unwrap();
        d.run(Cmd::ToggleComment).unwrap();
        assert_eq!(txt(&d), "// a\n// b\n// c\n");
        d.run(Cmd::ToggleComment).unwrap();
        assert_eq!(txt(&d), "a\nb\nc\n");
        d.run(Cmd::Undo).unwrap();
        d.run(Cmd::Undo).unwrap();
        assert_eq!(txt(&d), "a\nb\nc\n");
        assert!(d.has_sel());
    }

    #[test]
    fn closing_brace_on_a_blank_line_dedents() {
        let mut d = cpp("{\n    ");
        d.run(Cmd::Move {
            to: Motion::DocEnd,
            ext: false,
            rows: None,
        })
        .unwrap();
        typed(&mut d, "}");
        assert_eq!(txt(&d), "{\n}");
    }

    #[test]
    fn dirty_tracks_save_point() {
        let mut d = cpp("x");
        assert!(!d.is_dirty());
        typed(&mut d, "y");
        assert!(d.is_dirty());
        d.saved_rev = d.rev;
        assert!(!d.is_dirty());
    }

    #[test]
    fn vertical_motion_keeps_the_column_through_short_lines() {
        let mut d = cpp("abcdef\nx\nabcdef");
        d.run(Cmd::Goto { line: 0, col: 5 }).unwrap();
        mv(&mut d, Motion::Down, false);
        assert_eq!(d.cursor_pos(), (1, 1));
        mv(&mut d, Motion::Down, false);
        assert_eq!(d.cursor_pos(), (2, 5));
    }

    #[test]
    fn tabs_map_to_visual_columns() {
        let mut d = cpp("\tx");
        d.run(Cmd::Click {
            line: 0,
            vcol: 4,
            ext: false,
        })
        .unwrap();
        assert_eq!(d.cursor_pos(), (0, 1));
        let v = d.view(0, 10, false);
        assert_eq!(v.cur, [0, 4]);
        assert_eq!(v.lines[0], "    <l>x</l>");
    }

    #[test]
    fn crlf_round_trips() {
        let d = Doc::new("/x/a.c".into(), "a\r\nb\r\n", None);
        assert_eq!(txt(&d), "a\nb\n");
        assert_eq!(d.contents_for_disk(), "a\r\nb\r\n");
    }

    #[test]
    fn html_is_escaped() {
        let mut d = cpp("a<b>&c");
        let v = d.view(0, 5, false);
        assert_eq!(v.lines[0], "<l>a</l>&lt;<l>b</l>&gt;&amp;<l>c</l>");
    }

    #[test]
    fn multiline_state_invalidates_after_an_edit() {
        let mut d = cpp("int a;\nint b;");
        d.view(0, 5, false);
        d.run(Cmd::Goto { line: 0, col: 0 }).unwrap();
        d.run(Cmd::Insert { s: "/*".into() }).unwrap();
        let v = d.view(0, 5, false);
        assert_eq!(v.lines[1], "<c>int b;</c>");
    }

    #[test]
    fn search_wraps_and_replaces() {
        let mut d = cpp("foo x\nFOO foo\n");
        assert_eq!(d.set_search("foo", false), 3);
        d.run(Cmd::Goto { line: 1, col: 4 }).unwrap();
        d.run(Cmd::FindNext { back: false }).unwrap();
        assert_eq!(d.cursor_pos(), (1, 7));
        d.run(Cmd::FindNext { back: false }).unwrap();
        assert_eq!(d.cursor_pos(), (0, 3));
        d.run(Cmd::FindNext { back: true }).unwrap();
        assert_eq!(d.cursor_pos(), (1, 7));
        d.run(Cmd::ReplaceAll { s: "z".into() }).unwrap();
        assert_eq!(txt(&d), "z x\nz z\n");
        d.run(Cmd::Undo).unwrap();
        assert_eq!(txt(&d), "foo x\nFOO foo\n");
    }

    #[test]
    fn shrink_selection_after_the_file_changed_on_disk() {
        let mut d = cpp("int main() { return value; }\n");
        d.run(Cmd::Goto { line: 0, col: 20 }).unwrap();
        d.run(Cmd::ExpandSelection).unwrap();
        d.run(Cmd::ExpandSelection).unwrap();
        d.reload("x", None);
        d.run(Cmd::ShrinkSelection).unwrap();
        d.check().unwrap();
        d.view(0, 10, true);
    }

    #[test]
    fn completion_with_a_selection_before_the_caret() {
        let mut d = cpp("ab foo");
        d.run(Cmd::Goto { line: 0, col: 6 }).unwrap();
        d.run(Cmd::Move { to: Motion::Home, ext: true, rows: None }).unwrap();
        d.run(Cmd::Move { to: Motion::End, ext: true, rows: None }).unwrap();
        d.run(Cmd::Complete { s: "foobar".into(), back: 0 }).unwrap();
        d.check().unwrap();
        assert_eq!(txt(&d), "ab foobar");
    }

    #[test]
    fn replacements_that_start_inside_a_character() {
        let mut d = cpp("a😀b");
        d.apply_byte_replacements(vec![(3, 1, "x".into()), (2, 0, "y".into())]);
        d.check().unwrap();
    }

    #[test]
    fn moving_lines_down_with_the_selection_ending_on_the_last_line() {
        let mut d = cpp("a\nb");
        d.run(Cmd::SelectAll).unwrap();
        d.run(Cmd::MoveLines { up: false }).unwrap();
        d.check().unwrap();
    }

    #[test]
    fn counting_matches_agrees_with_the_matches_on_each_line() {
        let texts = ["Return x;\nreturn RETURN\nreturnreturn\n", "ÉÉé éÉ\nAaAa aa\n", "😀a😀A\n\n😀", "aaaa\naaa\n"];
        for text in texts {
            let mut d = cpp(text);
            for q in ["return", "a", "aa", "é", "😀a", "Éé", "\n"] {
                for case in [false, true] {
                    let n = d.set_search(q, case);
                    let by_line: usize = (0..d.rope.len_lines()).map(|l| d.matches_in(&d.line_text(l)).len()).sum();
                    assert_eq!(n, by_line, "{text:?} {q:?} {case}");
                }
            }
        }
    }

    #[test]
    fn visual_column_with_and_without_tabs() {
        let d = cpp("\tab\n  c");
        assert_eq!(d.vcol(0, 1), 4);
        assert_eq!(d.vcol(0, 3), 6);
        assert_eq!(d.vcol(1, 3), 3);
        assert_eq!(d.vcol(0, 99), 7);
    }

    #[test]
    fn a_huge_line_is_not_sent_ahead_for_copying() {
        let mut d = cpp(&"x".repeat(CLIP_MAX + 10));
        assert_eq!(d.view(0, 10, true).clip.as_deref(), Some(""));
        assert_eq!(d.clip_text().len(), CLIP_MAX + 11);
        let mut d = cpp("short");
        assert_eq!(d.view(0, 10, true).clip.as_deref(), Some("short\n"));
    }

    #[test]
    fn a_change_at_the_top_keeps_the_colors_further_down() {
        let text: String = (0..800).map(|i| format!("int f{i}() {{ return {i}; }} /* c */\n")).collect();
        let mut d = cpp(&text);
        d.states_from_scratch().unwrap();
        d.run(Cmd::Goto { line: 0, col: 0 }).unwrap();
        d.run(Cmd::Type { s: "x".into() }).unwrap();
        // Drawing the end needs the states down there: the first line shows they did not change,
        // so nothing past it is left to check, and the lines in between are not worked out again.
        assert_eq!(d.check.iter().filter(|c| **c).count(), 1);
        let v = d.view(750, 40, false);
        assert!(d.check.iter().all(|c| !c), "taken back, not worked out again");
        assert!(plain_text(&v).iter().any(|l| l.contains("f760")));
        // A change that does reach every line below is still followed.
        d.run(Cmd::Type { s: "/*".into() }).unwrap();
        d.states_from_scratch().unwrap();
    }

    /// Replace All changes lines all over the file at once: only those are worked out again.
    #[test]
    fn changes_all_over_the_file_work_out_only_the_lines_they_touch() {
        let text: String = (0..900).map(|i| if i % 300 == 10 { "int MARK = 1;\n".to_string() } else { format!("int f{i}() {{ return {i}; }}\n") }).collect();
        let mut d = cpp(&text);
        d.states_from_scratch().unwrap();
        d.set_search("MARK", true);
        d.run(Cmd::ReplaceAll { s: "OTHER".into() }).unwrap();
        d.worked = 0;
        d.view(850, 40, false);
        assert!(d.worked <= 6, "{} lines worked out again", d.worked);
        d.states_from_scratch().unwrap();
    }

    /// Far down a big file, before the states get there, the view is still colored.
    #[test]
    fn lines_not_worked_out_yet_are_colored_from_a_guess() {
        let text: String = (0..400).map(|i| format!("int f{i}() {{\n    return {i}; // c\n}}\n")).collect();
        let mut fresh = cpp(&text);
        fresh.states_from_scratch().unwrap();
        let right = fresh.view(1150, 30, false).lines;
        let mut d = cpp(&text);
        // No time at all to work the states out: everything below the top is guessed.
        d.state_deadline = Some(Instant::now());
        let v = d.build_view(1150, 30, false);
        assert!(d.pending, "the right colors are still to come");
        assert!(d.valid < 100);
        assert_eq!(v.lines, right, "for plain code the guess is right");
    }

    fn plain_text(v: &View) -> Vec<String> {
        v.lines.iter().map(|l| l.replace("&amp;", "&").split('<').map(|p| p.split_once('>').map_or(p, |x| x.1)).collect::<String>()).collect()
    }

    #[test]
    fn utf16_columns_handle_astral_characters() {
        let d = cpp("😀x");
        assert_eq!(d.col_from_utf16(0, 2), 1);
        assert_eq!(d.utf16_col(0, 1), 2);
    }

    #[test]
    fn delete_line_at_end_of_file_takes_the_previous_newline() {
        let mut d = cpp("a\nb");
        d.run(Cmd::Goto { line: 1, col: 0 }).unwrap();
        d.run(Cmd::DeleteLine).unwrap();
        assert_eq!(txt(&d), "a");
    }

    #[test]
    fn complete_replaces_the_prefix() {
        let mut d = cpp("std::vec");
        d.run(Cmd::Move {
            to: Motion::DocEnd,
            ext: false,
            rows: None,
        })
        .unwrap();
        assert_eq!(d.prefix(), "vec");
        assert_eq!(d.before_prefix(), "::");
        d.run(Cmd::Complete { s: "vector".into(), back: 0 }).unwrap();
        assert_eq!(txt(&d), "std::vector");
    }

    #[test]
    fn lexing_is_lazy_on_huge_files() {
        let text = "int x;\n".repeat(200_000);
        let mut d = cpp(&text);
        let v = d.view(0, 40, false);
        assert_eq!(v.lines.len(), 40 + OVERSCAN);
        assert!(d.valid < 40 + 2 * OVERSCAN + 40);
    }

    #[test]
    fn a_far_jump_in_a_grammar_file_is_colored_in_steps_and_ends_exact() {
        let text = format!("int f() {{\n{}}}\n", "  x = 1;\n".repeat(1500));
        let mut d = Doc::new("/x/a.cpp".into(), &text, None);
        assert!(matches!(d.lang, Lang::Grammar(_)));
        let mut asks = 0;
        let v = loop {
            let v = d.view(1400, 10, false);
            asks += 1;
            if !v.pending || asks > 400 {
                break v;
            }
        };
        assert!(!v.pending, "still pending after {asks} views");
        assert!(d.valid > 1400);
        assert!(v.lines[0].contains("<n>1</n>"), "{}", v.lines[0]);
    }

    #[test]
    fn comments_toggle_with_block_markers_where_there_is_no_line_comment() {
        let mut d = Doc::new("/x/a.html".into(), "<div>\n  <p>hi</p>\n</div>\n", None);
        d.run(Cmd::SelectAll).unwrap();
        d.run(Cmd::ToggleComment).unwrap();
        assert_eq!(txt(&d), "<!-- <div> -->\n  <!-- <p>hi</p> -->\n<!-- </div> -->\n");
        d.run(Cmd::ToggleComment).unwrap();
        assert_eq!(txt(&d), "<div>\n  <p>hi</p>\n</div>\n");
        let mut c = Doc::new("/x/a.css".into(), "a { x: y }\n", None);
        c.run(Cmd::ToggleComment).unwrap();
        assert_eq!(txt(&c), "/* a { x: y } */\n");
    }

    #[test]
    fn the_language_can_be_chosen_by_hand() {
        let mut d = Doc::new("/x/notes.txt".into(), "def f(): pass\n", None);
        assert_eq!(d.lang.label(), "Plain Text");
        d.set_language(Some("python")).unwrap();
        assert_eq!(d.lang.label(), "Python");
        assert!(d.relang());
        assert_eq!(d.lang.label(), "Python");
        assert!(d.set_language(Some("nope")).is_err());
        d.set_language(None).unwrap();
        assert_eq!(d.lang.label(), "Plain Text");
    }

    #[test]
    fn tidy_trims_trailing_spaces_and_adds_the_last_newline() {
        let mut d = cpp("int a;  \n\tb;\t\n  \nend");
        d.run(Cmd::Goto { line: 0, col: 6 }).unwrap();
        d.run(Cmd::Tidy { trim: true, newline: true }).unwrap();
        assert_eq!(txt(&d), "int a;\n\tb;\n\nend\n");
        assert_eq!(d.pos(d.cur), (0, 6));
        d.run(Cmd::Undo).unwrap();
        d.run(Cmd::Tidy { trim: false, newline: false }).unwrap();
        let mut m = Doc::new("/x/a.md".into(), "line  \n", None);
        m.run(Cmd::Tidy { trim: true, newline: true }).unwrap();
        assert_eq!(txt(&m), "line  \n");
    }

    #[test]
    fn a_snippet_is_filled_in_with_tab_going_from_stop_to_stop() {
        let mut d = Doc::new("/x/a.py".into(), "for\n", None);
        d.run(Cmd::Goto { line: 0, col: 3 }).unwrap();
        // for ${1:item} in ${2:items}:\n    $0
        let text = "for item in items:\n    ".to_string();
        d.run(Cmd::Snippet { s: text, stops: vec![[4, 8], [12, 17], [23, 23]] }).unwrap();
        assert_eq!(txt(&d), "for item in items:\n    \n");
        assert_eq!(d.sel_range(), (4, 8));
        d.run(Cmd::Type { s: "x".into() }).unwrap();
        assert_eq!(txt(&d), "for x in items:\n    \n");
        d.run(Cmd::Tab).unwrap();
        // The second stop moved with the edit and is selected.
        assert_eq!(d.sel_range(), (9, 14));
        d.run(Cmd::Outdent).unwrap();
        assert_eq!(d.sel_range(), (4, 5));
        d.run(Cmd::Tab).unwrap();
        d.run(Cmd::Tab).unwrap();
        // The last stop ends the snippet, and Tab is a plain Tab again.
        assert_eq!(d.sel_range(), (20, 20));
        assert!(d.snip.is_none());
        d.run(Cmd::Tab).unwrap();
        assert!(txt(&d).contains("    \n") || txt(&d).len() > 22);
    }

    #[test]
    fn typing_goes_to_every_cursor_and_one_undo_takes_it_all_back() {
        let mut d = cpp("foo bar foo baz foo\n");
        d.run(Cmd::Goto { line: 0, col: 0 }).unwrap();
        for _ in 0..3 {
            d.run(Cmd::SelectNext).unwrap();
        }
        assert_eq!(d.extras.len(), 2);
        d.run(Cmd::Type { s: "x".into() }).unwrap();
        assert_eq!(txt(&d), "x bar x baz x\n");
        d.run(Cmd::Type { s: "y".into() }).unwrap();
        assert_eq!(txt(&d), "xy bar xy baz xy\n");
        d.run(Cmd::Backspace).unwrap();
        assert_eq!(txt(&d), "x bar x baz x\n");
        d.run(Cmd::Undo).unwrap();
        d.run(Cmd::Undo).unwrap();
        assert_eq!(txt(&d), "foo bar foo baz foo\n");
        assert!(d.extras.is_empty());
    }

    #[test]
    fn carets_can_be_added_on_the_lines_above_and_below() {
        let mut d = cpp("abc\nabc\nabc");
        d.run(Cmd::Goto { line: 0, col: 1 }).unwrap();
        d.run(Cmd::AddCursorVertical { up: false }).unwrap();
        d.run(Cmd::AddCursorVertical { up: false }).unwrap();
        d.run(Cmd::AddCursorVertical { up: false }).unwrap();
        assert_eq!(d.extras.len(), 2, "no line below the last one");
        d.run(Cmd::Type { s: "X".into() }).unwrap();
        assert_eq!(txt(&d), "aXbc\naXbc\naXbc");
        d.run(Cmd::Newline).unwrap();
        assert_eq!(txt(&d), "aX\nbc\naX\nbc\naX\nbc");
        d.run(Cmd::Move { to: Motion::Left, ext: false, rows: None }).unwrap();
        d.run(Cmd::Delete).unwrap();
        assert_eq!(txt(&d), "aXbc\naXbc\naXbc");
        d.run(Cmd::ClearCursors).unwrap();
        assert!(d.extras.is_empty());
    }

    #[test]
    fn cursors_that_meet_become_one_and_alt_click_toggles_them() {
        let mut d = cpp("ab\ncd\n");
        d.run(Cmd::Goto { line: 0, col: 1 }).unwrap();
        d.run(Cmd::AddCursor { line: 1, vcol: 1 }).unwrap();
        assert_eq!(d.extras.len(), 1);
        d.run(Cmd::AddCursor { line: 1, vcol: 1 }).unwrap();
        assert!(d.extras.is_empty());
        d.run(Cmd::AddCursor { line: 1, vcol: 1 }).unwrap();
        // Backspace twice on "ab|" and "c|d" leaves the carets at the start of their lines.
        d.run(Cmd::Move { to: Motion::Home, ext: false, rows: None }).unwrap();
        assert_eq!(d.extras.len(), 1);
        d.run(Cmd::Move { to: Motion::DocStart, ext: false, rows: None }).unwrap();
        assert!(d.extras.is_empty() || d.pos(d.cur) == (0, 0));
    }

    #[test]
    fn every_match_can_get_a_cursor() {
        let mut d = cpp("ab ab\nab\n");
        d.run(Cmd::Goto { line: 0, col: 0 }).unwrap();
        d.run(Cmd::SelectAllMatches).unwrap();
        assert_eq!(d.extras.len(), 2);
        d.run(Cmd::Type { s: "Z".into() }).unwrap();
        assert_eq!(txt(&d), "Z Z\nZ\n");
        let v = d.view(0, 5, false);
        assert_eq!(v.carets.len(), 2);
    }

    #[test]
    fn remembered_tokens_color_the_same_and_follow_a_change_of_state() {
        let text: String = (0..40).map(|i| format!("int f{i}() {{ return \"s\" + {i}; }} // c\n")).collect();
        let mut d = cpp(&text);
        assert!(matches!(d.lang, Lang::Grammar(_)), "C++ is colored by its grammar");
        let first = d.view(0, 30, false).lines;
        assert!(!d.tok_cache.is_empty());
        assert_eq!(d.view(0, 30, false).lines, first, "from the cache");
        let mut fresh = cpp(&text);
        assert_eq!(fresh.view(0, 30, false).lines, first, "without a cache");
        // Opening a block comment on the first line changes every line below.
        d.run(Cmd::Goto { line: 0, col: 0 }).unwrap();
        d.run(Cmd::Type { s: "/*".into() }).unwrap();
        let mut plain = cpp(&format!("/*{text}"));
        assert_eq!(d.view(0, 30, false).lines, plain.view(0, 30, false).lines);
    }

    #[test]
    fn a_wrapped_long_line_draws_the_same_rows_as_before() {
        let line: String = (0..400).map(|i| format!("word{i} ")).collect();
        let mut d = wrapped(&format!("{line}\nend\n"), 30);
        let v = d.view(0, 200, false);
        let joined: String = plain(&v).iter().take_while(|l| !l.starts_with("end")).cloned().collect();
        assert_eq!(joined.trim_end(), line.trim_end());
    }

    fn wrapped(text: &str, cols: usize) -> Doc {
        let mut d = cpp(text);
        d.run(Cmd::SetWrap { cols }).unwrap();
        d
    }

    fn plain(v: &View) -> Vec<String> {
        v.lines.iter().map(|l| l.replace("&amp;", "&").split('<').map(|p| p.split_once('>').map_or(p, |x| x.1)).collect::<String>()).collect()
    }

    #[test]
    fn long_lines_wrap_after_the_last_space_that_fits() {
        let mut d = wrapped("aaaa bbbb cccc dddd\nxy\n", 10);
        assert_eq!(&*d.line_breaks(0), &[(0, 0), (10, 10)]);
        assert_eq!(d.row_count(), 4, "two rows, one, and the empty last line");
        let v = d.view(0, 10, false);
        assert_eq!(plain(&v)[..3], ["aaaa bbbb ", "cccc dddd", "xy"]);
        assert_eq!(&v.cont[..3], &[false, true, false]);
        assert_eq!(&v.nums[..3], &[0, 0, 1]);
        // A word that is longer than a row is broken in the middle.
        let d = wrapped("abcdefghijklmno\n", 5);
        assert_eq!(&*d.line_breaks(0), &[(0, 0), (5, 5), (10, 10)]);
        // Off again: one row per line.
        let mut d = wrapped("aaaa bbbb cccc dddd\n", 10);
        d.run(Cmd::SetWrap { cols: 0 }).unwrap();
        assert_eq!(d.row_count(), 2);
    }

    #[test]
    fn the_pointer_and_the_arrow_keys_work_on_rows_of_a_wrapped_line() {
        let mut d = wrapped("aaaa bbbb cccc dddd\nxy\n", 10);
        d.run(Cmd::Click { line: 1, vcol: 2, ext: false }).unwrap();
        assert_eq!(d.pos(d.cur), (0, 12));
        assert_eq!(d.cell_of(d.cur), (1, 2));
        d.run(Cmd::Move { to: Motion::Up, ext: false, rows: None }).unwrap();
        assert_eq!(d.pos(d.cur), (0, 2));
        d.run(Cmd::Move { to: Motion::Down, ext: false, rows: None }).unwrap();
        d.run(Cmd::Move { to: Motion::Down, ext: false, rows: None }).unwrap();
        assert_eq!(d.pos(d.cur), (1, 2));
        // The view puts the caret on the right row and cell.
        d.run(Cmd::Click { line: 1, vcol: 4, ext: false }).unwrap();
        let v = d.view(0, 10, false);
        assert_eq!(v.cur, [1, 4]);
        // Clicking past the end of a row stays on that row's line.
        d.run(Cmd::Click { line: 0, vcol: 40, ext: false }).unwrap();
        assert_eq!(d.pos(d.cur).0, 0);
    }

    #[test]
    fn selections_and_overlays_are_split_over_the_rows() {
        let mut d = wrapped("aaaa bbbb cccc dddd\n", 10);
        d.anchor = 7;
        d.cur = 13;
        let v = d.view(0, 10, false);
        assert_eq!(v.sel, vec![[0, 7, 10], [1, 0, 3]]);
        d.run(Cmd::Goto { line: 0, col: 12 }).unwrap();
        d.cur = 12;
        d.anchor = 12;
        let v = d.view(0, 10, false);
        assert_eq!(v.cur, [1, 2]);
    }

    #[test]
    fn rows_follow_edits_and_folds_when_wrapping() {
        let mut d = wrapped("one two three four\nb {\n  x\n}\nlast\n", 8);
        let before = d.row_count();
        d.run(Cmd::Goto { line: 4, col: 0 }).unwrap();
        d.run(Cmd::Insert { s: "more words to wrap here ".into() }).unwrap();
        assert!(d.row_count() > before);
        assert_eq!(d.row_to_line(d.row_count() - 1), 5);
        d.run(Cmd::ToggleFold { row: d.line_to_row(1) }).unwrap();
        let rows = d.row_count();
        d.run(Cmd::ToggleFold { row: d.line_to_row(1) }).unwrap();
        assert!(d.row_count() > rows);
        d.run(Cmd::Undo).unwrap();
        d.run(Cmd::Undo).unwrap();
        assert_eq!(d.row_count(), before);
        // Every row maps to a line, and each line's first row maps back to it.
        for r in 0..d.row_count() {
            let (l, k) = d.locate(r);
            assert_eq!(d.line_to_row(l) + k, r);
        }
    }

    #[test]
    fn long_lines_are_capped() {
        let mut d = cpp(&"+".repeat(100_000));
        let v = d.view(0, 1, false);
        assert_eq!(v.lines[0].len(), MAX_RENDER);
    }

    #[test]
    fn a_folded_block_shows_where_it_ends_on_the_same_row() {
        let mut d = cpp("void f() {\n    if (x) {\n        a;\n    } else {\n        b;\n    }\n}\n");
        d.run(Cmd::ToggleFold { row: 1 }).unwrap();
        let v = d.view(0, 10, false);
        let plain = |l: &str| l.split('<').map(|p| p.split_once('>').map_or(p, |x| x.1)).collect::<String>();
        assert_eq!(plain(&v.lines[1]), "    if (x) { \u{2026} } else {");
        d.run(Cmd::ToggleFold { row: 0 }).unwrap();
        let v = d.view(0, 10, false);
        assert_eq!(plain(&v.lines[0]), "void f() { \u{2026} }");
        assert_eq!(v.n, 2);
    }

    #[test]
    fn folding_hides_rows_and_keeps_numbers() {
        let mut d = cpp("int f() {\n  a;\n  b;\n}\nint x;\n");
        assert_eq!(d.view(0, 10, false).folds, vec![[0, 1]]);
        d.run(Cmd::ToggleFold { row: 0 }).unwrap();
        let v = d.view(0, 10, false);
        assert_eq!(v.n, 3);
        assert_eq!(v.nums, vec![0, 4, 5]);
        assert!(v.lines[0].ends_with("<e> \u{2026} </e>}"), "{}", v.lines[0]);
        d.run(Cmd::Goto { line: 4, col: 0 }).unwrap();
        d.run(Cmd::Move {
            to: Motion::Up,
            ext: false,
            rows: None,
        })
        .unwrap();
        assert_eq!(d.cursor_pos().0, 0);
    }

    #[test]
    fn caret_inside_a_fold_opens_it() {
        let mut d = cpp("f() {\n a;\n}\n");
        d.run(Cmd::ToggleFold { row: 0 }).unwrap();
        d.run(Cmd::Goto { line: 1, col: 0 }).unwrap();
        assert_eq!(d.view(0, 10, false).n, 4);
    }

    #[test]
    fn braces_in_strings_do_not_end_a_fold() {
        let mut d = cpp("f() {\n s = \"}\";\n // }\n}\nnext;\n");
        d.run(Cmd::ToggleFold { row: 0 }).unwrap();
        assert_eq!(d.view(0, 10, false).nums, vec![0, 4, 5]);
    }

    #[test]
    fn editing_above_shifts_folds_and_breakpoints() {
        let mut d = cpp("a\nf() {\n x;\n}\nend\n");
        d.run(Cmd::ToggleFold { row: 1 }).unwrap();
        d.run(Cmd::SetBreakpoints { lines: vec![4] }).unwrap();
        d.run(Cmd::Goto { line: 0, col: 0 }).unwrap();
        d.run(Cmd::Newline).unwrap();
        let v = d.view(0, 10, false);
        assert_eq!(v.nums, vec![0, 1, 2, 5, 6]);
        assert_eq!(v.bps, vec![3]);
        assert_eq!(v.all_bps, Some(vec![5]));
    }

    #[test]
    fn moving_lines_carries_the_caret() {
        let mut d = cpp("one\ntwo\nthree");
        d.run(Cmd::Goto { line: 1, col: 1 }).unwrap();
        d.run(Cmd::MoveLines { up: true }).unwrap();
        assert_eq!(txt(&d), "two\none\nthree");
        assert_eq!(d.cursor_pos(), (0, 1));
        d.run(Cmd::MoveLines { up: false }).unwrap();
        d.run(Cmd::MoveLines { up: false }).unwrap();
        assert_eq!(txt(&d), "one\nthree\ntwo");
        assert_eq!(d.cursor_pos(), (2, 1));
        d.run(Cmd::Undo).unwrap();
        assert_eq!(txt(&d), "one\ntwo\nthree");
    }

    #[test]
    fn expand_selection_walks_outwards_and_back() {
        let mut d = cpp("f(a, g(bcd))");
        d.run(Cmd::Goto { line: 0, col: 8 }).unwrap();
        let sel = |d: &Doc| d.rope.slice(d.sel_range().0..d.sel_range().1).to_string();
        d.run(Cmd::ExpandSelection).unwrap();
        assert_eq!(sel(&d), "bcd");
        d.run(Cmd::ExpandSelection).unwrap();
        assert_eq!(sel(&d), "(bcd)");
        d.run(Cmd::ExpandSelection).unwrap();
        assert_eq!(sel(&d), "a, g(bcd)");
        d.run(Cmd::ShrinkSelection).unwrap();
        assert_eq!(sel(&d), "(bcd)");
    }

    #[test]
    fn read_only_documents_ignore_edits_but_move() {
        let mut d = cpp("abc");
        d.readonly = true;
        d.run(Cmd::Type { s: "x".into() }).unwrap();
        assert_eq!(txt(&d), "abc");
        d.run(Cmd::Move {
            to: Motion::DocEnd,
            ext: false,
            rows: None,
        })
        .unwrap();
        assert_eq!(d.cursor_pos(), (0, 3));
    }

    #[test]
    fn brackets_in_strings_and_comments_do_not_count() {
        let mut d = cpp("if (c == ')') { f(\"(\"); } // )\nx");
        d.run(Cmd::Goto { line: 0, col: 3 }).unwrap();
        assert_eq!(d.matching_bracket(None), Some((3, 12)), "not the one in ')'");
        d.run(Cmd::Goto { line: 0, col: 14 }).unwrap();
        assert_eq!(d.matching_bracket(None), Some((14, 24)), "past the \"(\"");
        // A bracket that is itself in a string has no partner.
        d.run(Cmd::Goto { line: 0, col: 19 }).unwrap();
        assert_eq!(d.matching_bracket(None), None);
    }

    #[test]
    fn matching_bracket_and_occurrences() {
        let mut d = cpp("foo(foo, xfoo)");
        d.run(Cmd::Goto { line: 0, col: 3 }).unwrap();
        let v = d.view(0, 5, false);
        assert_eq!(v.pair, vec![[0, 3], [0, 13]]);
        assert_eq!(v.occ, vec![[0, 0, 3], [0, 4, 7]]);
    }

    #[test]
    fn indent_guides_skip_the_first_column_and_keep_widths() {
        let mut d = cpp("        x");
        let v = d.view(0, 5, false);
        assert_eq!(v.lines[0], "    <g>    </g><l>x</l>");
        assert_eq!(v.cur, [0, 0]);
    }

    #[test]
    fn clangd_tokens_replace_guesses_but_not_keywords() {
        let mut d = cpp("Foo bar; return Baz;\n");
        d.set_semantic(vec![(0, 4, 3, lexer::MEMBER), (0, 9, 6, lexer::MEMBER)]);
        assert_eq!(
            d.view(0, 5, false).lines[0],
            "Foo <v>bar</v>; <k>return</k> Baz;"
        );
    }

    #[test]
    fn clangd_tokens_follow_edits_on_their_line() {
        let mut d = cpp("int value;\nint other;\n");
        d.set_semantic(vec![(0, 4, 5, lexer::MEMBER), (1, 4, 5, lexer::MEMBER)]);
        d.run(Cmd::Goto { line: 0, col: 0 }).unwrap();
        d.run(Cmd::Insert { s: "x ".into() }).unwrap();
        assert_eq!(
            d.view(0, 5, false).lines[0],
            "<l>x</l> <t>int</t> <v>value</v>;"
        );
        d.run(Cmd::Newline).unwrap();
        assert_eq!(d.view(0, 5, false).lines[2], "<t>int</t> <v>other</v>;");
    }

    #[test]
    fn semantic_columns_convert_from_utf16() {
        let mut d = cpp("/*😀*/ abc;\n");
        d.set_semantic(vec![(0, 7, 3, lexer::MEMBER)]);
        assert_eq!(d.view(0, 5, false).lines[0], "<c>/*😀*/</c> <v>abc</v>;");
    }

    #[test]
    fn byte_replacements_are_one_undo_and_keep_the_caret() {
        let mut d = cpp("int  a;\nint b;\n");
        d.run(Cmd::Goto { line: 1, col: 4 }).unwrap();
        d.run(Cmd::ToggleBreakpoint { row: Some(1) }).unwrap();
        assert_eq!(d.apply_byte_replacements(vec![(3, 2, " ".into())]), 1);
        assert_eq!(txt(&d), "int a;\nint b;\n");
        assert_eq!(d.cursor_pos(), (1, 4));
        assert_eq!(d.view(0, 5, false).bps, vec![1]);
        d.run(Cmd::Undo).unwrap();
        assert_eq!(txt(&d), "int  a;\nint b;\n");
    }

    #[test]
    fn clipboard_is_only_resent_when_it_changes() {
        let mut d = cpp("hello\n");
        assert_eq!(d.view(0, 5, false).clip.as_deref(), Some("hello\n"));
        assert!(d.view(0, 5, false).clip.is_none());
        d.run(Cmd::SelectAll).unwrap();
        assert_eq!(d.view(0, 5, false).clip.as_deref(), Some("hello\n"));
    }
}
