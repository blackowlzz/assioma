use crate::langs::{self, Def};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Lang {
    Plain,
    C,
    Cpp,
    CMake,
    Diff,
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Json,
    Go,
    Java,
    CSharp,
    Php,
    Ruby,
    Shell,
    Yaml,
    Toml,
    Sql,
    Lua,
    Kotlin,
    Swift,
    Config,
    Css,
    Html,
    Markdown,
    /// A language from the grammar registry: built in, or installed from an extension.
    Grammar(u16),
}

impl Lang {
    pub fn from_path(p: &Path) -> Lang {
        if let Some(i) = crate::grammars::lang_for_path(p) {
            return match crate::grammars::lang_id(i) {
                Some("diff") => Lang::Diff,
                _ => Lang::Grammar(i),
            };
        }
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        match name {
            "CMakeLists.txt" => return Lang::CMake,
            "Dockerfile" | "Makefile" | "makefile" | "GNUmakefile" | ".gitignore" | ".dockerignore"
            | ".editorconfig" | ".env" | ".gitattributes" => return Lang::Config,
            ".bashrc" | ".zshrc" | ".profile" | ".bash_profile" => return Lang::Shell,
            _ => {}
        }
        match p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "c" => Lang::C,
            "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "ipp" | "inl" | "tpp" => Lang::Cpp,
            "cmake" => Lang::CMake,
            "rs" => Lang::Rust,
            "py" | "pyw" | "pyi" => Lang::Python,
            "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
            "ts" | "tsx" | "mts" | "cts" => Lang::TypeScript,
            "json" | "jsonc" | "json5" | "webmanifest" => Lang::Json,
            "go" => Lang::Go,
            "java" => Lang::Java,
            "cs" => Lang::CSharp,
            "php" => Lang::Php,
            "rb" | "rake" | "gemspec" => Lang::Ruby,
            "sh" | "bash" | "zsh" => Lang::Shell,
            "yml" | "yaml" => Lang::Yaml,
            "toml" => Lang::Toml,
            "sql" => Lang::Sql,
            "lua" => Lang::Lua,
            "kt" | "kts" => Lang::Kotlin,
            "swift" => Lang::Swift,
            "css" | "scss" | "less" => Lang::Css,
            "html" | "htm" | "xhtml" | "xml" | "svg" | "vue" => Lang::Html,
            "md" | "markdown" => Lang::Markdown,
            "ini" | "conf" | "cfg" | "properties" | "env" => Lang::Config,
            _ => Lang::Plain,
        }
    }

    /// A language by its id, for choosing one by hand.
    pub fn from_id(id: &str) -> Option<Lang> {
        Some(match id {
            "plaintext" => Lang::Plain,
            "kotlin" => Lang::Kotlin,
            "toml" => Lang::Toml,
            "cmake" => Lang::CMake,
            "config" => Lang::Config,
            other => Lang::Grammar(crate::grammars::lang_by_id(other)?),
        })
    }

    /// The name shown in the status bar.
    pub fn label(self) -> String {
        match self {
            Lang::Plain => "Plain Text".into(),
            Lang::Diff => "Diff".into(),
            Lang::Grammar(i) => crate::grammars::label(i).unwrap_or("Plain Text").into(),
            Lang::C => "C".into(),
            Lang::Cpp => "C++".into(),
            Lang::CMake => "CMake".into(),
            Lang::Css => "CSS".into(),
            Lang::Html => "HTML".into(),
            Lang::Markdown => "Markdown".into(),
            Lang::Json => "JSON".into(),
            Lang::Php => "PHP".into(),
            Lang::Sql => "SQL".into(),
            Lang::Toml => "TOML".into(),
            Lang::Yaml => "YAML".into(),
            Lang::CSharp => "C#".into(),
            other => other.def().map_or("Plain Text".into(), |d| {
                let mut c = d.id.chars();
                c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str())
            }),
        }
    }

    /// The built-in language that goes with a grammar language: its words feed completion.
    pub fn legacy(self) -> Lang {
        let Lang::Grammar(i) = self else { return self };
        match crate::grammars::lang_id(i) {
            Some("c") => Lang::C,
            Some("cpp") => Lang::Cpp,
            Some("python") => Lang::Python,
            Some("javascript" | "javascriptreact") => Lang::JavaScript,
            Some("typescript" | "typescriptreact") => Lang::TypeScript,
            Some("rust") => Lang::Rust,
            Some("go") => Lang::Go,
            Some("java") => Lang::Java,
            Some("csharp") => Lang::CSharp,
            Some("php") => Lang::Php,
            Some("ruby") => Lang::Ruby,
            Some("shellscript") => Lang::Shell,
            Some("sql") => Lang::Sql,
            Some("lua") => Lang::Lua,
            Some("swift") => Lang::Swift,
            Some("kotlin") => Lang::Kotlin,
            Some("json" | "jsonc") => Lang::Json,
            _ => self,
        }
    }

    /// The table that colors this language, for the ones that are table driven.
    pub fn def(self) -> Option<&'static Def> {
        Some(match self {
            Lang::Rust => &langs::RUST,
            Lang::Python => &langs::PYTHON,
            Lang::JavaScript => &langs::JAVASCRIPT,
            Lang::TypeScript => &langs::TYPESCRIPT,
            Lang::Json => &langs::JSON,
            Lang::Go => &langs::GO,
            Lang::Java => &langs::JAVA,
            Lang::CSharp => &langs::CSHARP,
            Lang::Php => &langs::PHP,
            Lang::Ruby => &langs::RUBY,
            Lang::Shell => &langs::SHELL,
            Lang::Yaml => &langs::YAML,
            Lang::Toml => &langs::TOML,
            Lang::Sql => &langs::SQL,
            Lang::Lua => &langs::LUA,
            Lang::Kotlin => &langs::KOTLIN,
            Lang::Swift => &langs::SWIFT,
            Lang::Config => &langs::CONFIG,
            _ => return None,
        })
    }

    /// Everything but plain text and diffs counts as code, for suggestions and auto pairs.
    pub fn is_code(self) -> bool {
        !matches!(self, Lang::Plain | Lang::Diff)
    }

    /// The language id, which also tells the AI what it is looking at.
    pub fn lsp_id(self) -> Option<&'static str> {
        match self {
            Lang::Plain | Lang::Diff => None,
            Lang::Grammar(i) => crate::grammars::lang_id(i),
            Lang::C => Some("c"),
            Lang::Cpp => Some("cpp"),
            Lang::CMake => Some("cmake"),
            Lang::Css => Some("css"),
            Lang::Html => Some("html"),
            Lang::Markdown => Some("markdown"),
            other => other.def().map(|d| d.id),
        }
    }

    /// The markers of a block comment, for the languages that have no line comment.
    pub fn block_comment(self) -> Option<(&'static str, &'static str)> {
        match self {
            Lang::Grammar(i) => crate::grammars::block_comment(i),
            Lang::Css => Some(("/*", "*/")),
            Lang::Html | Lang::Markdown => Some(("<!--", "-->")),
            _ => None,
        }
    }

    pub fn line_comment(self) -> Option<&'static str> {
        match self {
            Lang::Grammar(i) => crate::grammars::line_comment(i),
            Lang::C | Lang::Cpp => Some("//"),
            Lang::CMake => Some("#"),
            Lang::Plain | Lang::Diff | Lang::Css | Lang::Html | Lang::Markdown | Lang::Json => None,
            other => other.def().and_then(|d| d.line.first().copied()),
        }
    }
}

pub const KEYWORD: u8 = b'k';
pub const TYPE: u8 = b't';
pub const COMMENT: u8 = b'c';
pub const STRING: u8 = b'r';
pub const NUMBER: u8 = b'n';
pub const DIRECTIVE: u8 = b'd';
pub const FUNCTION: u8 = b'f';
pub const USER_TYPE: u8 = b'y';
pub const NAMESPACE: u8 = b'o';
pub const MACRO: u8 = b'w';
pub const MEMBER: u8 = b'v';
pub const ESCAPE: u8 = b'x';
pub const VARIABLE: u8 = b'l';
pub const CONSTANT: u8 = b'z';

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tok {
    pub start: u32,
    pub end: u32,
    pub tag: u8,
}

const NORMAL: u32 = 0;
pub const BLOCK: u32 = 1;
const STRING_CONT: u32 = 2;
const LINE_COMMENT_CONT: u32 = 3;
const RAW_BASE: u32 = 16;

const C_KEYWORDS: &[&str] = &[
    "_Alignas",
    "_Alignof",
    "_Atomic",
    "_Generic",
    "_Noreturn",
    "_Static_assert",
    "_Thread_local",
    "auto",
    "break",
    "case",
    "const",
    "continue",
    "default",
    "do",
    "else",
    "enum",
    "extern",
    "for",
    "goto",
    "if",
    "inline",
    "register",
    "restrict",
    "return",
    "sizeof",
    "static",
    "struct",
    "switch",
    "typedef",
    "union",
    "volatile",
    "while",
];

const CPP_KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "and",
    "asm",
    "auto",
    "break",
    "case",
    "catch",
    "class",
    "co_await",
    "co_return",
    "co_yield",
    "concept",
    "const",
    "const_cast",
    "consteval",
    "constexpr",
    "constinit",
    "continue",
    "decltype",
    "default",
    "delete",
    "do",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "final",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "not",
    "nullptr",
    "operator",
    "or",
    "override",
    "private",
    "protected",
    "public",
    "register",
    "reinterpret_cast",
    "requires",
    "return",
    "sizeof",
    "static",
    "static_assert",
    "static_cast",
    "struct",
    "switch",
    "template",
    "this",
    "thread_local",
    "throw",
    "true",
    "try",
    "typedef",
    "typeid",
    "typename",
    "union",
    "using",
    "virtual",
    "volatile",
    "while",
    "xor",
];

const STD_TYPES: &[&str] = &[
    "array",
    "bitset",
    "deque",
    "forward_list",
    "function",
    "list",
    "map",
    "multimap",
    "multiset",
    "optional",
    "pair",
    "priority_queue",
    "queue",
    "set",
    "shared_ptr",
    "span",
    "stack",
    "string",
    "string_view",
    "thread",
    "tuple",
    "unique_ptr",
    "unordered_map",
    "unordered_set",
    "variant",
    "vector",
    "weak_ptr",
];

const TYPES: &[&str] = &[
    "FILE", "bool", "char", "char16_t", "char32_t", "char8_t", "double", "float", "int", "long",
    "short", "signed", "unsigned", "void", "wchar_t",
];

/// Whether a line state ends inside a block comment, which can be folded.
pub fn in_block_comment(lang: Lang, state: u32) -> bool {
    match lang {
        Lang::Grammar(i) => crate::grammars::in_block_comment(i, state),
        _ => state == BLOCK,
    }
}

pub fn lex(line: &str, state: u32, lang: Lang, raws: &mut Vec<String>, out: &mut Vec<Tok>) -> u32 {
    match lang {
        Lang::Plain => NORMAL,
        Lang::Diff => {
            let tag = match line.as_bytes().first() {
                _ if line.starts_with("+++") || line.starts_with("---") => DIRECTIVE,
                Some(b'+') => KEYWORD,
                Some(b'-') => COMMENT,
                Some(b'@') => DIRECTIVE,
                _ => 0,
            };
            if tag != 0 {
                push(out, 0, line.len(), tag);
            }
            NORMAL
        }
        Lang::CMake => lex_cmake(line.as_bytes(), state, out),
        Lang::C => lex_c(line.as_bytes(), state, false, raws, out),
        Lang::Cpp => lex_c(line.as_bytes(), state, true, raws, out),
        Lang::Grammar(i) => crate::grammars::tokenize(i, line, state, out),
        Lang::Markdown => langs::lex_markdown(line.as_bytes(), state, out),
        Lang::Html => langs::lex_html(line.as_bytes(), state, out),
        Lang::Css => langs::lex_css(line.as_bytes(), state, out),
        other => other.def().map_or(NORMAL, |d| langs::lex_def(line.as_bytes(), state, d, out)),
    }
}

pub(crate) fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

pub fn vocabulary(lang: Lang) -> Vec<&'static str> {
    match lang.legacy() {
        Lang::C => [C_KEYWORDS, TYPES].concat(),
        Lang::Cpp => [CPP_KEYWORDS, TYPES, STD_TYPES].concat(),
        other => other
            .def()
            .map_or(vec![], |d| [d.keywords, d.types, d.constants].concat()),
    }
}

pub(crate) fn push(out: &mut Vec<Tok>, start: usize, end: usize, tag: u8) {
    if end > start {
        out.push(Tok {
            start: start as u32,
            end: end as u32,
            tag,
        });
    }
}

pub(crate) fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if b.len() < needle.len() {
        return None;
    }
    (from..=b.len() - needle.len()).find(|&i| &b[i..i + needle.len()] == needle)
}

pub(crate) fn scan_quoted(b: &[u8], mut i: usize, quote: u8) -> (usize, bool) {
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
        } else if b[i] == quote {
            return (i + 1, true);
        } else {
            i += 1;
        }
    }
    (b.len(), false)
}

/// Length of the valid escape sequence at the start of `s`, which begins with a backslash.
fn escape_len(s: &[u8]) -> usize {
    let hex_run = |max: usize| {
        s[2..]
            .iter()
            .take(max)
            .take_while(|c| c.is_ascii_hexdigit())
            .count()
    };
    match s.get(1) {
        Some(
            b'n' | b't' | b'r' | b'a' | b'b' | b'f' | b'v' | b'e' | b'?' | b'\\' | b'\'' | b'"',
        ) => 2,
        Some(b'0'..=b'7') => {
            1 + s[1..]
                .iter()
                .take(3)
                .take_while(|c| (b'0'..=b'7').contains(*c))
                .count()
        }
        Some(b'x') => match hex_run(usize::MAX) {
            0 => 0,
            digits => 2 + digits,
        },
        Some(b'u') if hex_run(4) == 4 => 6,
        Some(b'U') if hex_run(8) == 8 => 10,
        _ => 0,
    }
}

pub(crate) fn push_quoted(out: &mut Vec<Tok>, b: &[u8], start: usize, end: usize) {
    let (mut from, mut i) = (start, start);
    while i < end {
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        match escape_len(&b[i..end]) {
            0 => i += 2,
            len => {
                push(out, from, i, STRING);
                push(out, i, i + len, ESCAPE);
                i += len;
                from = i;
            }
        }
    }
    push(out, from, end, STRING);
}

fn ends_with_backslash(b: &[u8]) -> bool {
    b.last() == Some(&b'\\')
}

fn lex_c(b: &[u8], state: u32, cpp: bool, raws: &mut Vec<String>, out: &mut Vec<Tok>) -> u32 {
    let n = b.len();
    let mut i = 0;

    match state {
        BLOCK => match find(b, 0, b"*/") {
            Some(e) => {
                push(out, 0, e + 2, COMMENT);
                i = e + 2;
            }
            None => {
                push(out, 0, n, COMMENT);
                return BLOCK;
            }
        },
        STRING_CONT => {
            let (e, closed) = scan_quoted(b, 0, b'"');
            push_quoted(out, b, 0, e);
            if !closed {
                return if ends_with_backslash(b) {
                    STRING_CONT
                } else {
                    NORMAL
                };
            }
            i = e;
        }
        LINE_COMMENT_CONT => {
            push(out, 0, n, COMMENT);
            return if ends_with_backslash(b) {
                LINE_COMMENT_CONT
            } else {
                NORMAL
            };
        }
        s if s >= RAW_BASE => {
            let close = format!("){}\"", raws[(s - RAW_BASE) as usize]);
            match find(b, 0, close.as_bytes()) {
                Some(e) => {
                    push(out, 0, e + close.len(), STRING);
                    i = e + close.len();
                }
                None => {
                    push(out, 0, n, STRING);
                    return s;
                }
            }
        }
        _ => {}
    }

    let mut include = false;
    if state == NORMAL {
        let mut j = i;
        while j < n && (b[j] == b' ' || b[j] == b'\t') {
            j += 1;
        }
        if j < n && b[j] == b'#' {
            let start = j;
            j += 1;
            while j < n && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            let word = j;
            while j < n && is_ident(b[j]) {
                j += 1;
            }
            include = &b[word..j] == b"include" || &b[word..j] == b"include_next";
            push(out, start, j, DIRECTIVE);
            i = j;
        }
    }

    while i < n {
        let c = b[i];
        if c == b'/' && i + 1 < n && b[i + 1] == b'/' {
            push(out, i, n, COMMENT);
            return if ends_with_backslash(b) {
                LINE_COMMENT_CONT
            } else {
                NORMAL
            };
        }
        if c == b'/' && i + 1 < n && b[i + 1] == b'*' {
            match find(b, i + 2, b"*/") {
                Some(e) => {
                    push(out, i, e + 2, COMMENT);
                    i = e + 2;
                    continue;
                }
                None => {
                    push(out, i, n, COMMENT);
                    return BLOCK;
                }
            }
        }
        if include && c == b'<' {
            let e = find(b, i, b">").map_or(n, |e| e + 1);
            push(out, i, e, STRING);
            i = e;
            continue;
        }
        if c == b'"' {
            let (e, closed) = scan_quoted(b, i + 1, b'"');
            push_quoted(out, b, i, e);
            if !closed && ends_with_backslash(b) {
                return STRING_CONT;
            }
            i = e;
            continue;
        }
        if c == b'\'' {
            let (e, _) = scan_quoted(b, i + 1, b'\'');
            push_quoted(out, b, i, e);
            i = e;
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && i + 1 < n && b[i + 1].is_ascii_digit()) {
            let start = i;
            let hex = c == b'0' && i + 1 < n && (b[i + 1] | 32) == b'x';
            i += 1;
            while i < n {
                let d = b[i];
                let exponent = if hex {
                    (d | 32) == b'p'
                } else {
                    (d | 32) == b'e'
                };
                if exponent && i + 1 < n && (b[i + 1] == b'+' || b[i + 1] == b'-') {
                    i += 2;
                } else if d.is_ascii_alphanumeric() || d == b'.' || d == b'_' {
                    i += 1;
                } else if d == b'\'' && cpp && i + 1 < n && b[i + 1].is_ascii_alphanumeric() {
                    i += 2;
                } else {
                    break;
                }
            }
            push(out, start, i.min(n), NUMBER);
            i = i.min(n);
            continue;
        }
        if is_ident(c) {
            let start = i;
            while i < n && is_ident(b[i]) {
                i += 1;
            }
            let word = &b[start..i];
            let next = if i < n { b[i] } else { 0 };

            if next == b'"' && cpp && matches!(word, b"R" | b"LR" | b"uR" | b"UR" | b"u8R") {
                let open = i + 1;
                let Some(paren) = find(b, open, b"(") else {
                    push(out, start, n, STRING);
                    return NORMAL;
                };
                let delim = String::from_utf8_lossy(&b[open..paren]).into_owned();
                let close = format!("){delim}\"");
                match find(b, paren + 1, close.as_bytes()) {
                    Some(e) => {
                        push(out, start, e + close.len(), STRING);
                        i = e + close.len();
                        continue;
                    }
                    None => {
                        push(out, start, n, STRING);
                        let idx = raws.iter().position(|d| *d == delim).unwrap_or_else(|| {
                            raws.push(delim);
                            raws.len() - 1
                        });
                        return RAW_BASE + idx as u32;
                    }
                }
            }
            if (next == b'"' || next == b'\'') && matches!(word, b"L" | b"u" | b"U" | b"u8") {
                let (e, closed) = scan_quoted(b, i + 1, next);
                push_quoted(out, b, start, e);
                if !closed && next == b'"' && ends_with_backslash(b) {
                    return STRING_CONT;
                }
                i = e;
                continue;
            }

            let text = std::str::from_utf8(word).unwrap_or("");
            let keywords = if cpp { CPP_KEYWORDS } else { C_KEYWORDS };
            if matches!(text, "true" | "false" | "nullptr" | "NULL") {
                push(out, start, i, CONSTANT);
            } else if keywords.binary_search(&text).is_ok() {
                push(out, start, i, KEYWORD);
            } else if TYPES.binary_search(&text).is_ok() || text.ends_with("_t") {
                push(out, start, i, TYPE);
            } else {
                let mut j = i;
                while j < n && b[j] == b' ' {
                    j += 1;
                }
                let call = j < n && b[j] == b'(';
                let scope_after = i + 1 < n && b[i] == b':' && b[i + 1] == b':';
                let member = (start >= 1 && b[start - 1] == b'.')
                    || (start >= 2 && &b[start - 2..start] == b"->");
                let caps = word.len() >= 2
                    && word
                        .iter()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
                    && word.iter().any(u8::is_ascii_uppercase);
                let tag = if caps {
                    Some(MACRO)
                } else if call {
                    Some(FUNCTION)
                } else if scope_after {
                    Some(NAMESPACE)
                } else if word[0].is_ascii_uppercase()
                    || (cpp && STD_TYPES.binary_search(&text).is_ok())
                {
                    Some(USER_TYPE)
                } else if member {
                    Some(MEMBER)
                } else {
                    Some(VARIABLE)
                };
                if let Some(t) = tag {
                    push(out, start, i, t);
                }
            }
            continue;
        }
        i += 1;
    }
    NORMAL
}

fn lex_cmake(b: &[u8], state: u32, out: &mut Vec<Tok>) -> u32 {
    let n = b.len();
    let mut i = 0;
    if state == STRING_CONT {
        let (e, closed) = scan_quoted(b, 0, b'"');
        push(out, 0, e, STRING);
        if !closed {
            return STRING_CONT;
        }
        i = e;
    }
    while i < n {
        let c = b[i];
        if c == b'#' {
            push(out, i, n, COMMENT);
            return NORMAL;
        }
        if c == b'"' {
            let (e, closed) = scan_quoted(b, i + 1, b'"');
            push(out, i, e, STRING);
            if !closed {
                return STRING_CONT;
            }
            i = e;
            continue;
        }
        if c == b'$' && i + 1 < n && b[i + 1] == b'{' {
            let e = find(b, i, b"}").map_or(n, |e| e + 1);
            push(out, i, e, TYPE);
            i = e;
            continue;
        }
        if is_ident(c) {
            let start = i;
            while i < n && is_ident(b[i]) {
                i += 1;
            }
            let mut j = i;
            while j < n && b[j] == b' ' {
                j += 1;
            }
            if j < n && b[j] == b'(' {
                push(out, start, i, KEYWORD);
            }
            continue;
        }
        i += 1;
    }
    NORMAL
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(line: &str, state: u32, lang: Lang) -> (Vec<(String, u8)>, u32) {
        let mut raws = vec!["x".to_string()];
        let mut out = vec![];
        let next = lex(line, state, lang, &mut raws, &mut out);
        let t = out
            .iter()
            .map(|t| (line[t.start as usize..t.end as usize].to_string(), t.tag))
            .collect();
        (t, next)
    }

    fn string_parts(line: &str) -> Vec<(String, u8)> {
        let mut toks = vec![];
        lex(line, NORMAL, Lang::Cpp, &mut vec![], &mut toks);
        toks.iter()
            .map(|t| (line[t.start as usize..t.end as usize].to_string(), t.tag))
            .collect()
    }

    #[test]
    fn escape_sequences_get_their_own_token() {
        assert_eq!(
            string_parts(r#""a\n\x41\101\u00e8b""#),
            vec![
                ("\"a".to_string(), STRING),
                ("\\n".to_string(), ESCAPE),
                ("\\x41".to_string(), ESCAPE),
                ("\\101".to_string(), ESCAPE),
                ("\\u00e8".to_string(), ESCAPE),
                ("b\"".to_string(), STRING),
            ]
        );
        assert_eq!(
            string_parts(r"'\n'"),
            vec![
                ("'".to_string(), STRING),
                ("\\n".to_string(), ESCAPE),
                ("'".to_string(), STRING),
            ]
        );
    }

    #[test]
    fn invalid_escapes_and_raw_strings_stay_plain() {
        assert_eq!(
            string_parts(r#""\q\x\u12""#),
            vec![(r#""\q\x\u12""#.to_string(), STRING)]
        );
        assert_eq!(
            string_parts(r#"R"(a\n)""#),
            vec![(r#"R"(a\n)""#.to_string(), STRING)]
        );
    }

    #[test]
    fn keyword_tables_stay_sorted_for_binary_search() {
        for table in [C_KEYWORDS, CPP_KEYWORDS, TYPES, STD_TYPES] {
            assert!(
                table.windows(2).all(|w| w[0] < w[1]),
                "{:?}",
                table.windows(2).find(|w| w[0] >= w[1])
            );
        }
    }

    #[test]
    fn highlights_a_cpp_line() {
        let (t, st) = tags("int main() { return 0x1F; } // ok", 0, Lang::Cpp);
        assert_eq!(st, NORMAL);
        assert_eq!(
            t,
            vec![
                ("int".into(), TYPE),
                ("main".into(), FUNCTION),
                ("return".into(), KEYWORD),
                ("0x1F".into(), NUMBER),
                ("// ok".into(), COMMENT)
            ]
        );
    }

    #[test]
    fn block_comments_span_lines() {
        let (_, st) = tags("a /* open", 0, Lang::C);
        assert_eq!(st, BLOCK);
        let (t, st) = tags("still */ int", BLOCK, Lang::C);
        assert_eq!(st, NORMAL);
        assert_eq!(t, vec![("still */".into(), COMMENT), ("int".into(), TYPE)]);
    }

    #[test]
    fn comment_markers_inside_strings_are_ignored() {
        let (t, st) = tags(r#"s = "/* not // a comment";"#, 0, Lang::C);
        assert_eq!(st, NORMAL);
        assert_eq!(t.len(), 2);
        assert_eq!(t[1].1, STRING);
    }

    #[test]
    fn includes_and_raw_strings() {
        let (t, _) = tags("#include <vector>", 0, Lang::Cpp);
        assert_eq!(
            t,
            vec![("#include".into(), DIRECTIVE), ("<vector>".into(), STRING)]
        );

        let (_, st) = tags(r#"auto a = R"x(hello"#, 0, Lang::Cpp);
        assert_eq!(st, RAW_BASE);
        let (t, st) = tags(r#"world)x"; int b;"#, st, Lang::Cpp);
        assert_eq!(st, NORMAL);
        assert_eq!(t[0], (r#"world)x""#.to_string(), STRING));
    }

    #[test]
    fn cpp_digit_separators_are_one_number() {
        let (t, _) = tags("x = 1'000'000;", 0, Lang::Cpp);
        assert_eq!(
            t,
            vec![("x".into(), VARIABLE), ("1'000'000".into(), NUMBER)]
        );
    }

    #[test]
    fn utf8_does_not_break_token_offsets() {
        let (t, _) = tags("// perché è così", 0, Lang::C);
        assert_eq!(t[0].0, "// perché è così");
        let (t, _) = tags("città(1);", 0, Lang::C);
        assert_eq!(t[0], ("città".to_string(), FUNCTION));
    }

    #[test]
    fn identifier_classes_for_colouring() {
        let (t, _) = tags("std::vector<Point> v; p.x = MAX_N;", 0, Lang::Cpp);
        assert_eq!(
            t,
            vec![
                ("std".into(), NAMESPACE),
                ("vector".into(), USER_TYPE),
                ("Point".into(), USER_TYPE),
                ("v".into(), VARIABLE),
                ("p".into(), VARIABLE),
                ("x".into(), MEMBER),
                ("MAX_N".into(), MACRO)
            ]
        );
    }

    #[test]
    fn constants_have_their_own_token() {
        let (t, _) = tags("return true ? nullptr : NULL;", 0, Lang::Cpp);
        assert_eq!(
            t,
            vec![
                ("return".into(), KEYWORD),
                ("true".into(), CONSTANT),
                ("nullptr".into(), CONSTANT),
                ("NULL".into(), CONSTANT),
            ]
        );
    }

    #[test]
    fn cmake_basics() {
        let (t, st) = tags(r#"project(demo "x" ${V}) # c"#, 0, Lang::CMake);
        assert_eq!(st, NORMAL);
        assert_eq!(
            t,
            vec![
                ("project".into(), KEYWORD),
                ("\"x\"".into(), STRING),
                ("${V}".into(), TYPE),
                ("# c".into(), COMMENT)
            ]
        );
    }
}
