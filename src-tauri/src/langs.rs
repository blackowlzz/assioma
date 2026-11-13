//! Syntax coloring for many languages, driven by small tables.

use crate::lexer::{
    find, is_ident, push, push_quoted, scan_quoted, Tok, COMMENT, CONSTANT, DIRECTIVE, ESCAPE,
    FUNCTION, KEYWORD, MACRO, MEMBER, NUMBER, STRING, TYPE, USER_TYPE, VARIABLE,
};

pub const BLOCK: u32 = 1;
const TRIPLE_D: u32 = 3;
const TRIPLE_S: u32 = 4;
const TICK: u32 = 5;
const FENCE: u32 = 6;
const TAG: u32 = 7;
const INSIDE: u32 = 8;

pub struct Def {
    pub id: &'static str,
    pub line: &'static [&'static str],
    pub block: Option<(&'static str, &'static str)>,
    pub quotes: &'static [u8],
    pub triple: bool,
    pub tick_multiline: bool,
    pub keywords: &'static [&'static str],
    pub types: &'static [&'static str],
    pub constants: &'static [&'static str],
    pub decorator: Option<u8>,
    pub sigils: &'static [u8],
    pub keys_colon: bool,
    pub keys_eq: bool,
    pub nocase: bool,
    pub bang_macros: bool,
    pub sections: bool,
}

const BASE: Def = Def {
    id: "",
    line: &[],
    block: None,
    quotes: b"\"'",
    triple: false,
    tick_multiline: false,
    keywords: &[],
    types: &[],
    constants: &[],
    decorator: None,
    sigils: &[],
    keys_colon: false,
    keys_eq: false,
    nocase: false,
    bang_macros: false,
    sections: false,
};

const SLASH_BLOCK: Option<(&str, &str)> = Some(("/*", "*/"));

pub static RUST: Def = Def {
    id: "rust",
    line: &["//"],
    block: SLASH_BLOCK,
    quotes: b"\"",
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "type",
        "unsafe", "use", "where", "while",
    ],
    types: &[
        "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
        "f32", "f64", "bool", "char", "str",
    ],
    constants: &["true", "false", "None"],
    decorator: Some(b'#'),
    bang_macros: true,
    ..BASE
};

pub static PYTHON: Def = Def {
    id: "python",
    line: &["#"],
    triple: true,
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is",
        "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with",
        "yield", "match", "case",
    ],
    types: &["int", "float", "str", "bool", "list", "dict", "set", "tuple", "bytes", "object"],
    constants: &["True", "False", "None"],
    decorator: Some(b'@'),
    ..BASE
};

const JS_KEYWORDS: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger",
    "default", "delete", "do", "else", "export", "extends", "finally", "for", "from", "function",
    "if", "import", "in", "instanceof", "let", "new", "of", "return", "static", "super",
    "switch", "this", "throw", "try", "typeof", "var", "void", "while", "with", "yield",
];

pub static JAVASCRIPT: Def = Def {
    id: "javascript",
    line: &["//"],
    block: SLASH_BLOCK,
    quotes: b"\"'`",
    tick_multiline: true,
    keywords: JS_KEYWORDS,
    constants: &["true", "false", "null", "undefined", "NaN", "Infinity"],
    decorator: Some(b'@'),
    ..BASE
};

pub static TYPESCRIPT: Def = Def {
    id: "typescript",
    keywords: &[
        "abstract", "as", "async", "await", "break", "case", "catch", "class", "const",
        "continue", "declare", "default", "delete", "do", "else", "enum", "export", "extends",
        "finally", "for", "from", "function", "if", "implements", "import", "in", "infer",
        "instanceof", "interface", "is", "keyof", "let", "namespace", "new", "of", "private",
        "protected", "public", "readonly", "return", "satisfies", "static", "super", "switch",
        "this", "throw", "try", "type", "typeof", "var", "void", "while", "yield",
    ],
    types: &[
        "any", "bigint", "boolean", "never", "number", "object", "string", "symbol", "unknown",
    ],
    ..JAVASCRIPT
};

pub static JSON: Def = Def {
    id: "json",
    line: &["//"],
    block: SLASH_BLOCK,
    quotes: b"\"",
    constants: &["true", "false", "null"],
    keys_colon: false,
    ..BASE
};

pub static GO: Def = Def {
    id: "go",
    line: &["//"],
    block: SLASH_BLOCK,
    quotes: b"\"'`",
    tick_multiline: true,
    keywords: &[
        "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough",
        "for", "func", "go", "goto", "if", "import", "interface", "map", "package", "range",
        "return", "select", "struct", "switch", "type", "var",
    ],
    types: &[
        "any", "bool", "byte", "complex128", "complex64", "error", "float32", "float64", "int",
        "int16", "int32", "int64", "int8", "rune", "string", "uint", "uint16", "uint32",
        "uint64", "uint8", "uintptr",
    ],
    constants: &["true", "false", "nil", "iota"],
    ..BASE
};

pub static JAVA: Def = Def {
    id: "java",
    line: &["//"],
    block: SLASH_BLOCK,
    triple: true,
    keywords: &[
        "abstract", "assert", "break", "case", "catch", "class", "continue", "default", "do",
        "else", "enum", "extends", "final", "finally", "for", "if", "implements", "import",
        "instanceof", "interface", "native", "new", "package", "private", "protected", "public",
        "return", "static", "super", "switch", "synchronized", "this", "throw", "throws",
        "try", "var", "volatile", "while", "record", "sealed", "permits",
    ],
    types: &["boolean", "byte", "char", "double", "float", "int", "long", "short", "void"],
    constants: &["true", "false", "null"],
    decorator: Some(b'@'),
    ..BASE
};

pub static CSHARP: Def = Def {
    id: "csharp",
    line: &["//"],
    block: SLASH_BLOCK,
    keywords: &[
        "abstract", "as", "async", "await", "base", "break", "case", "catch", "class", "const",
        "continue", "default", "do", "else", "enum", "event", "explicit", "extern", "finally",
        "fixed", "for", "foreach", "get", "if", "implicit", "in", "interface", "internal", "is",
        "lock", "namespace", "new", "operator", "out", "override", "params", "private",
        "protected", "public", "readonly", "ref", "return", "sealed", "set", "static",
        "struct", "switch", "this", "throw", "try", "typeof", "using", "virtual", "volatile",
        "while", "yield", "record", "var",
    ],
    types: &[
        "bool", "byte", "char", "decimal", "double", "dynamic", "float", "int", "long", "object",
        "sbyte", "short", "string", "uint", "ulong", "ushort", "void",
    ],
    constants: &["true", "false", "null"],
    ..BASE
};

pub static PHP: Def = Def {
    id: "php",
    line: &["//", "#"],
    block: SLASH_BLOCK,
    keywords: &[
        "abstract", "and", "as", "break", "case", "catch", "class", "clone", "const",
        "continue", "declare", "default", "do", "echo", "else", "elseif", "empty", "extends",
        "final", "finally", "fn", "for", "foreach", "function", "global", "if", "implements",
        "include", "include_once", "instanceof", "interface", "isset", "list", "match",
        "namespace", "new", "or", "print", "private", "protected", "public", "readonly",
        "require", "require_once", "return", "static", "switch", "throw", "trait", "try",
        "unset", "use", "var", "while", "xor", "yield",
    ],
    types: &["array", "bool", "float", "int", "mixed", "string", "void"],
    constants: &["true", "false", "null", "TRUE", "FALSE", "NULL"],
    sigils: b"$",
    ..BASE
};

pub static RUBY: Def = Def {
    id: "ruby",
    line: &["#"],
    keywords: &[
        "alias", "and", "begin", "break", "case", "class", "def", "do", "else", "elsif", "end",
        "ensure", "for", "if", "in", "module", "next", "not", "or", "redo", "rescue", "retry",
        "return", "self", "super", "then", "undef", "unless", "until", "when", "while", "yield",
        "require", "include", "attr_accessor", "attr_reader", "attr_writer",
    ],
    constants: &["true", "false", "nil"],
    sigils: b"@$",
    ..BASE
};

pub static SHELL: Def = Def {
    id: "shellscript",
    line: &["#"],
    quotes: b"\"'`",
    keywords: &[
        "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in",
        "select", "then", "time", "until", "while",
    ],
    types: &[
        "alias", "cd", "echo", "eval", "exec", "exit", "export", "read", "return", "set",
        "shift", "source", "test", "trap", "unset",
    ],
    sigils: b"$",
    ..BASE
};

pub static YAML: Def = Def {
    id: "yaml",
    line: &["#"],
    constants: &["true", "false", "null", "yes", "no", "True", "False", "Null"],
    keys_colon: true,
    ..BASE
};

pub static TOML: Def = Def {
    id: "toml",
    line: &["#"],
    constants: &["true", "false"],
    keys_eq: true,
    sections: true,
    ..BASE
};

pub static SQL: Def = Def {
    id: "sql",
    line: &["--"],
    block: SLASH_BLOCK,
    nocase: true,
    keywords: &[
        "all", "alter", "and", "as", "between", "by", "case", "create", "default", "delete",
        "distinct", "drop", "else", "end", "exists", "foreign", "from", "group", "having", "in",
        "index", "inner", "insert", "into", "is", "join", "key", "left", "like", "limit", "not",
        "offset", "on", "or", "order", "outer", "primary", "references", "right", "select",
        "set", "table", "then", "union", "update", "values", "view", "when", "where", "with",
    ],
    types: &[
        "bigint", "boolean", "char", "date", "decimal", "double", "float", "int", "integer",
        "serial", "text", "timestamp", "varchar",
    ],
    constants: &["true", "false", "null"],
    ..BASE
};

pub static LUA: Def = Def {
    id: "lua",
    line: &["--"],
    block: Some(("--[[", "]]")),
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in",
        "local", "not", "or", "repeat", "return", "then", "until", "while",
    ],
    constants: &["true", "false", "nil"],
    ..BASE
};

pub static KOTLIN: Def = Def {
    id: "kotlin",
    line: &["//"],
    block: SLASH_BLOCK,
    triple: true,
    keywords: &[
        "abstract", "as", "break", "class", "companion", "continue", "data", "do", "else",
        "enum", "final", "for", "fun", "if", "import", "in", "interface", "is", "object",
        "open", "override", "package", "private", "protected", "public", "return", "sealed",
        "super", "this", "throw", "try", "typealias", "val", "var", "when", "while",
    ],
    types: &[
        "Any", "Boolean", "Byte", "Char", "Double", "Float", "Int", "Long", "Short", "String",
        "Unit",
    ],
    constants: &["true", "false", "null"],
    decorator: Some(b'@'),
    ..BASE
};

pub static SWIFT: Def = Def {
    id: "swift",
    line: &["//"],
    block: SLASH_BLOCK,
    triple: true,
    keywords: &[
        "as", "associatedtype", "async", "await", "break", "case", "catch", "class", "continue",
        "default", "defer", "do", "else", "enum", "extension", "fallthrough", "for", "func",
        "guard", "if", "import", "in", "init", "inout", "is", "let", "private", "protocol",
        "public", "repeat", "return", "self", "static", "struct", "subscript", "super",
        "switch", "throw", "throws", "try", "var", "where", "while",
    ],
    types: &["Any", "Bool", "Double", "Float", "Int", "String", "Void"],
    constants: &["true", "false", "nil"],
    decorator: Some(b'@'),
    ..BASE
};

/// Dockerfile, Makefile, .env, .gitignore and other files that only have # comments.
pub static CONFIG: Def = Def {
    id: "properties",
    line: &["#"],
    keys_eq: true,
    constants: &["true", "false"],
    ..BASE
};

pub fn lex_def(b: &[u8], state: u32, d: &Def, out: &mut Vec<Tok>) -> u32 {
    let n = b.len();
    let mut i = 0;
    if state == BLOCK {
        let close = d.block.map_or("", |c| c.1);
        match find(b, 0, close.as_bytes()) {
            Some(e) if !close.is_empty() => {
                push(out, 0, e + close.len(), COMMENT);
                i = e + close.len();
            }
            _ => {
                push(out, 0, n, COMMENT);
                return BLOCK;
            }
        }
    } else if state == TRIPLE_D || state == TRIPLE_S {
        let q: &[u8] = if state == TRIPLE_D { b"\"\"\"" } else { b"'''" };
        match find(b, 0, q) {
            Some(e) => {
                push(out, 0, e + 3, STRING);
                i = e + 3;
            }
            None => {
                push(out, 0, n, STRING);
                return state;
            }
        }
    } else if state == TICK {
        let (e, closed) = scan_quoted(b, 0, b'`');
        push_quoted(out, b, 0, e);
        if !closed {
            return TICK;
        }
        i = e;
    }
    let only_indent = |upto: usize| b[..upto].iter().all(|c| *c == b' ' || *c == b'\t' || *c == b'-');
    while i < n {
        let c = b[i];
        if c == b' ' || c == b'\t' {
            i += 1;
            continue;
        }
        if d.line.iter().any(|m| b[i..].starts_with(m.as_bytes())) && !(d.id == "lua" && b[i..].starts_with(b"--[[")) {
            push(out, i, n, COMMENT);
            return 0;
        }
        if let Some((open, close)) = d.block {
            if b[i..].starts_with(open.as_bytes()) {
                match find(b, i + open.len(), close.as_bytes()) {
                    Some(e) => {
                        push(out, i, e + close.len(), COMMENT);
                        i = e + close.len();
                        continue;
                    }
                    None => {
                        push(out, i, n, COMMENT);
                        return BLOCK;
                    }
                }
            }
        }
        if d.triple && (b[i..].starts_with(b"\"\"\"") || b[i..].starts_with(b"'''")) {
            let dq = b[i] == b'"';
            let q: &[u8] = if dq { b"\"\"\"" } else { b"'''" };
            match find(b, i + 3, q) {
                Some(e) => {
                    push(out, i, e + 3, STRING);
                    i = e + 3;
                    continue;
                }
                None => {
                    push(out, i, n, STRING);
                    return if dq { TRIPLE_D } else { TRIPLE_S };
                }
            }
        }
        if d.quotes.contains(&c) {
            let (e, closed) = scan_quoted(b, i + 1, c);
            let mark = out.len();
            push_quoted(out, b, i, e);
            if c == b'`' && d.tick_multiline && !closed {
                return TICK;
            }
            let mut j = e;
            while j < n && b[j] == b' ' {
                j += 1;
            }
            if d.id == "json" && j < n && b[j] == b':' {
                out.truncate(mark);
                push(out, i, e, MEMBER);
            }
            i = e;
            continue;
        }
        if d.sections && c == b'[' && only_indent(i) {
            let e = find(b, i, b"]").map_or(n, |e| e + 1);
            push(out, i, e, DIRECTIVE);
            i = e;
            continue;
        }
        if let Some(dc) = d.decorator {
            let attr = dc == b'#' && i + 1 < n && (b[i + 1] == b'[' || (b[i + 1] == b'!' && b.get(i + 2) == Some(&b'[')));
            if c == dc && ((dc != b'#' && i + 1 < n && is_ident(b[i + 1])) || attr) {
                let mut j = i + 1;
                if attr {
                    j = find(b, i, b"]").map_or(n, |e| e + 1);
                } else {
                    while j < n && (is_ident(b[j]) || b[j] == b'.') {
                        j += 1;
                    }
                }
                push(out, i, j, DIRECTIVE);
                i = j;
                continue;
            }
        }
        if d.sigils.contains(&c) && i + 1 < n && (is_ident(b[i + 1]) || b[i + 1] == b'{' || d.sigils.contains(&b[i + 1])) {
            let mut j = i + 1;
            if b[j] == b'{' {
                j = find(b, j, b"}").map_or(n, |e| e + 1);
            } else {
                while j < n && (is_ident(b[j]) || d.sigils.contains(&b[j])) {
                    j += 1;
                }
            }
            push(out, i, j, VARIABLE);
            i = j;
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && i + 1 < n && b[i + 1].is_ascii_digit()) {
            let start = i;
            i += 1;
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || (b[i] == b'.' && i + 1 < n && b[i + 1].is_ascii_digit())) {
                i += 1;
            }
            push(out, start, i, NUMBER);
            continue;
        }
        if is_ident(c) {
            let start = i;
            while i < n && is_ident(b[i]) {
                i += 1;
            }
            let text = std::str::from_utf8(&b[start..i]).unwrap_or("");
            // A prefix such as f, r or b right before a quote belongs to the string.
            if (d.id == "python" || d.id == "rust")
                && text.len() <= 2
                && text.bytes().all(|x| b"rbufRBUF".contains(&x))
                && b.get(i).is_some_and(|q| d.quotes.contains(q))
            {
                push(out, start, i, STRING);
                continue;
            }
            let lower;
            let probe = if d.nocase {
                lower = text.to_ascii_lowercase();
                lower.as_str()
            } else {
                text
            };
            let mut j = i;
            while j < n && b[j] == b' ' {
                j += 1;
            }
            let next = b.get(j).copied().unwrap_or(0);
            let after = b.get(j + 1).copied().unwrap_or(0);
            let tag = if d.keywords.contains(&probe) {
                KEYWORD
            } else if d.types.contains(&probe) {
                TYPE
            } else if d.constants.contains(&probe) {
                CONSTANT
            } else if ((d.keys_colon && next == b':' && after != b':') || (d.keys_eq && next == b'=' && after != b'=')) && only_indent(start) {
                MEMBER
            } else if next == b'(' {
                FUNCTION
            } else if d.bang_macros && next == b'!' && j == i {
                MACRO
            } else if start > 0 && b[start - 1] == b'.' {
                MEMBER
            } else if text.len() >= 2 && text.bytes().all(|x| x.is_ascii_uppercase() || x.is_ascii_digit() || x == b'_') && text.bytes().any(|x| x.is_ascii_uppercase()) {
                MACRO
            } else if text.as_bytes()[0].is_ascii_uppercase() {
                USER_TYPE
            } else {
                VARIABLE
            };
            push(out, start, i, tag);
            continue;
        }
        i += 1;
    }
    0
}

fn inline_markdown(b: &[u8], from: usize, out: &mut Vec<Tok>) {
    let n = b.len();
    let mut i = from;
    while i < n {
        match b[i] {
            b'`' => {
                let e = find(b, i + 1, b"`").map_or(n, |e| e + 1);
                push(out, i, e, STRING);
                i = e;
            }
            b'*' if b.get(i + 1) == Some(&b'*') => {
                let e = find(b, i + 2, b"**").map_or(n, |e| e + 2);
                push(out, i, e, KEYWORD);
                i = e;
            }
            b'[' => match find(b, i, b"](") {
                Some(mid) => {
                    let end = find(b, mid, b")").map_or(n, |e| e + 1);
                    push(out, i, mid + 1, FUNCTION);
                    push(out, mid + 1, end, STRING);
                    i = end;
                }
                None => i += 1,
            },
            _ => i += 1,
        }
    }
}

pub fn lex_markdown(b: &[u8], state: u32, out: &mut Vec<Tok>) -> u32 {
    let n = b.len();
    let lead = b.iter().take_while(|c| **c == b' ').count();
    let rest = &b[lead..];
    if rest.starts_with(b"```") || rest.starts_with(b"~~~") {
        push(out, 0, n, STRING);
        return if state == FENCE { 0 } else { FENCE };
    }
    if state == FENCE {
        push(out, 0, n, STRING);
        return FENCE;
    }
    if rest.first() == Some(&b'#') {
        push(out, 0, n, KEYWORD);
    } else if rest.first() == Some(&b'>') {
        push(out, 0, n, COMMENT);
    } else {
        let mut start = lead;
        let marker = rest.len() > 1
            && rest[1] == b' '
            && matches!(rest[0], b'-' | b'*' | b'+')
            || rest
                .iter()
                .position(|c| !c.is_ascii_digit())
                .is_some_and(|p| p > 0 && rest.get(p) == Some(&b'.') && rest.get(p + 1) == Some(&b' '));
        if marker {
            let e = rest.iter().position(|c| *c == b' ').unwrap_or(0) + lead;
            push(out, lead, e, DIRECTIVE);
            start = e;
        }
        inline_markdown(b, start, out);
    }
    0
}

pub fn lex_html(b: &[u8], state: u32, out: &mut Vec<Tok>) -> u32 {
    let n = b.len();
    let mut i = 0;
    let mut in_tag = state == TAG;
    if state == BLOCK {
        match find(b, 0, b"-->") {
            Some(e) => {
                push(out, 0, e + 3, COMMENT);
                i = e + 3;
            }
            None => {
                push(out, 0, n, COMMENT);
                return BLOCK;
            }
        }
    }
    while i < n {
        if in_tag {
            match b[i] {
                b'>' => {
                    in_tag = false;
                    i += 1;
                }
                b'"' | b'\'' => {
                    let (e, _) = scan_quoted(b, i + 1, b[i]);
                    push(out, i, e, STRING);
                    i = e;
                }
                c if is_ident(c) => {
                    let s = i;
                    while i < n && (is_ident(b[i]) || b[i] == b'-' || b[i] == b':') {
                        i += 1;
                    }
                    push(out, s, i, MEMBER);
                }
                _ => i += 1,
            }
            continue;
        }
        if b[i..].starts_with(b"<!--") {
            match find(b, i + 4, b"-->") {
                Some(e) => {
                    push(out, i, e + 3, COMMENT);
                    i = e + 3;
                }
                None => {
                    push(out, i, n, COMMENT);
                    return BLOCK;
                }
            }
        } else if b[i] == b'<' && b.get(i + 1) == Some(&b'!') {
            let e = find(b, i, b">").map_or(n, |e| e + 1);
            push(out, i, e, DIRECTIVE);
            i = e;
        } else if b[i] == b'<' && b.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic() || *c == b'/') {
            let s = i + 1 + usize::from(b[i + 1] == b'/');
            let mut e = s;
            while e < n && (is_ident(b[e]) || b[e] == b'-') {
                e += 1;
            }
            push(out, s, e, KEYWORD);
            i = e;
            in_tag = true;
        } else if b[i] == b'&' {
            let e = find(b, i, b";").map_or(i + 1, |e| e + 1);
            push(out, i, e, ESCAPE);
            i = e;
        } else {
            i += 1;
        }
    }
    if in_tag {
        TAG
    } else {
        0
    }
}

pub fn lex_css(b: &[u8], state: u32, out: &mut Vec<Tok>) -> u32 {
    let n = b.len();
    let mut i = 0;
    let mut inside = state & INSIDE != 0;
    if state & BLOCK != 0 {
        match find(b, 0, b"*/") {
            Some(e) => {
                push(out, 0, e + 2, COMMENT);
                i = e + 2;
            }
            None => {
                push(out, 0, n, COMMENT);
                return state;
            }
        }
    }
    while i < n {
        let c = b[i];
        if b[i..].starts_with(b"/*") {
            match find(b, i + 2, b"*/") {
                Some(e) => {
                    push(out, i, e + 2, COMMENT);
                    i = e + 2;
                }
                None => {
                    push(out, i, n, COMMENT);
                    return BLOCK | if inside { INSIDE } else { 0 };
                }
            }
        } else if c == b'"' || c == b'\'' {
            let (e, _) = scan_quoted(b, i + 1, c);
            push(out, i, e, STRING);
            i = e;
        } else if c == b'{' {
            inside = true;
            i += 1;
        } else if c == b'}' {
            inside = false;
            i += 1;
        } else if c == b'@' {
            let mut e = i + 1;
            while e < n && (is_ident(b[e]) || b[e] == b'-') {
                e += 1;
            }
            push(out, i, e, DIRECTIVE);
            i = e;
        } else if c == b'#' && inside {
            let mut e = i + 1;
            while e < n && b[e].is_ascii_hexdigit() {
                e += 1;
            }
            push(out, i, e, NUMBER);
            i = e;
        } else if c.is_ascii_digit() || (c == b'.' && inside && b.get(i + 1).is_some_and(|x| x.is_ascii_digit())) {
            let s = i;
            i += 1;
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'%') {
                i += 1;
            }
            push(out, s, i, NUMBER);
        } else if (c == b'.' || c == b'#') && !inside {
            let s = i;
            i += 1;
            while i < n && (is_ident(b[i]) || b[i] == b'-') {
                i += 1;
            }
            push(out, s, i, USER_TYPE);
        } else if is_ident(c) || c == b'-' {
            let s = i;
            while i < n && (is_ident(b[i]) || b[i] == b'-') {
                i += 1;
            }
            let mut j = i;
            while j < n && b[j] == b' ' {
                j += 1;
            }
            if i == s {
                i += 1;
                continue;
            }
            if inside && b.get(j) == Some(&b':') {
                push(out, s, i, MEMBER);
            } else if inside && b.get(j) == Some(&b'(') {
                push(out, s, i, FUNCTION);
            } else if !inside {
                push(out, s, i, KEYWORD);
            }
        } else {
            i += 1;
        }
    }
    if inside {
        INSIDE
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(d: &Def, line: &str, state: u32) -> (Vec<(String, u8)>, u32) {
        let mut out = vec![];
        let next = lex_def(line.as_bytes(), state, d, &mut out);
        (
            out.iter()
                .map(|t| (line[t.start as usize..t.end as usize].to_string(), t.tag))
                .collect(),
            next,
        )
    }

    #[test]
    fn python_has_decorators_triple_quotes_and_calls() {
        let (t, st) = tags(&PYTHON, "@app.route('/') # x", 0);
        assert_eq!(t[0], ("@app.route".to_string(), DIRECTIVE));
        assert!(t.iter().any(|(s, tag)| s == "'/'" && *tag == STRING));
        assert!(t.iter().any(|(s, tag)| s == "# x" && *tag == COMMENT));
        assert_eq!(st, 0);
        let (_, st) = tags(&PYTHON, "x = \"\"\"doc", 0);
        assert_eq!(st, TRIPLE_D);
        let (t, st) = tags(&PYTHON, "end\"\"\" + 1", TRIPLE_D);
        assert_eq!(t[0], ("end\"\"\"".to_string(), STRING));
        assert_eq!(st, 0);
    }

    #[test]
    fn rust_has_macros_attributes_and_lifetimes_left_alone() {
        let (t, _) = tags(&RUST, "#[derive(Debug)] fn main<'a>() { println!(\"hi\"); }", 0);
        assert_eq!(t[0], ("#[derive(Debug)]".to_string(), DIRECTIVE));
        assert!(t.iter().any(|(s, tag)| s == "fn" && *tag == KEYWORD));
        assert!(t.iter().any(|(s, tag)| s == "println" && *tag == MACRO));
    }

    #[test]
    fn json_keys_are_not_plain_strings() {
        let (t, _) = tags(&JSON, "  \"name\": \"assioma\", \"n\": 3, \"ok\": true", 0);
        assert_eq!(t[0], ("\"name\"".to_string(), MEMBER));
        assert!(t.iter().any(|(s, tag)| s == "\"assioma\"" && *tag == STRING));
        assert!(t.iter().any(|(s, tag)| s == "3" && *tag == NUMBER));
        assert!(t.iter().any(|(s, tag)| s == "true" && *tag == CONSTANT));
    }

    #[test]
    fn yaml_keys_and_toml_sections() {
        let (t, _) = tags(&YAML, "- name: web # c", 0);
        assert!(t.iter().any(|(s, tag)| s == "name" && *tag == MEMBER));
        let (t, _) = tags(&TOML, "[package]", 0);
        assert_eq!(t[0], ("[package]".to_string(), DIRECTIVE));
        let (t, _) = tags(&TOML, "version = \"1\"", 0);
        assert_eq!(t[0], ("version".to_string(), MEMBER));
    }

    #[test]
    fn block_comments_and_template_strings_continue_on_the_next_line() {
        let (_, st) = tags(&JAVASCRIPT, "/* open", 0);
        assert_eq!(st, BLOCK);
        let (t, st) = tags(&JAVASCRIPT, "close */ let a = `x", BLOCK);
        assert_eq!(t[0].0, "close */");
        assert_eq!(st, TICK);
    }

    #[test]
    fn sql_keywords_ignore_case() {
        let (t, _) = tags(&SQL, "SELECT name FROM users -- all", 0);
        assert_eq!(t[0], ("SELECT".to_string(), KEYWORD));
        assert!(t.iter().any(|(s, tag)| s == "FROM" && *tag == KEYWORD));
    }

    #[test]
    fn markdown_headings_fences_and_inline_code() {
        let mut out = vec![];
        assert_eq!(lex_markdown(b"## Title", 0, &mut out), 0);
        assert_eq!(out[0].tag, KEYWORD);
        let mut out = vec![];
        assert_eq!(lex_markdown(b"```rust", 0, &mut out), FENCE);
        let mut out = vec![];
        assert_eq!(lex_markdown(b"let x = 1;", FENCE, &mut out), FENCE);
        assert_eq!(out[0].tag, STRING);
        let mut out = vec![];
        lex_markdown(b"- use `code` here", 0, &mut out);
        assert!(out.iter().any(|t| t.tag == STRING));
    }

    #[test]
    fn html_tags_attributes_and_comments() {
        let mut out = vec![];
        let line = "<div class=\"a\">hi &amp;</div>";
        lex_html(line.as_bytes(), 0, &mut out);
        let text = |t: &Tok| &line[t.start as usize..t.end as usize];
        assert!(out.iter().any(|t| text(t) == "div" && t.tag == KEYWORD));
        assert!(out.iter().any(|t| text(t) == "class" && t.tag == MEMBER));
        assert!(out.iter().any(|t| text(t) == "\"a\"" && t.tag == STRING));
        assert!(out.iter().any(|t| text(t) == "&amp;" && t.tag == ESCAPE));
        let mut out = vec![];
        assert_eq!(lex_html(b"<!-- open", 0, &mut out), BLOCK);
    }

    #[test]
    fn css_properties_selectors_and_colors() {
        let mut out = vec![];
        let st = lex_css(b".box {", 0, &mut out);
        assert_eq!(st, INSIDE);
        assert_eq!(out[0].tag, USER_TYPE);
        let line = "  color: #fff; width: 10px;";
        let mut out = vec![];
        let st = lex_css(line.as_bytes(), INSIDE, &mut out);
        assert_eq!(st, INSIDE);
        let text = |t: &Tok| &line[t.start as usize..t.end as usize];
        assert!(out.iter().any(|t| text(t) == "color" && t.tag == MEMBER));
        assert!(out.iter().any(|t| text(t) == "#fff" && t.tag == NUMBER));
        assert!(out.iter().any(|t| text(t) == "10px" && t.tag == NUMBER));
    }
}
