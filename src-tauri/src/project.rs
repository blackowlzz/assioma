use serde::Serialize;
use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".cache",
    ".idea",
    ".vscode",
    ".assioma",
    "node_modules",
    "target",
];
const MAX_WALK: usize = 30_000;
const MAX_GREP_FILE: u64 = 2 << 20;
const MAX_GREP_HITS: usize = 400;

fn skip(name: &str) -> bool {
    SKIP_DIRS.contains(&name) || name.starts_with("cmake-build-")
}

#[derive(Serialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub dir: bool,
}

pub fn list_dir(path: &str, hidden: bool) -> Result<Vec<Entry>, String> {
    let mut out: Vec<Entry> = fs::read_dir(path)
        .map_err(|e| format!("{path}: {e}"))?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !hidden && name.starts_with('.') {
                return None;
            }
            let dir = e.path().is_dir();
            Some(Entry {
                path: e.path().to_string_lossy().into_owned(),
                name,
                dir,
            })
        })
        .collect();
    out.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase()));
    Ok(out)
}

/// Visits the files of a project, folders like `build` and `.git` left out. The visitor gets
/// the path and the entry, and asks for the metadata only when it needs the size: listing
/// files doesn't, and one less call per file shows on big trees.
fn size(e: &fs::DirEntry) -> Option<u64> {
    e.metadata().ok().map(|m| m.len())
}

fn walk(root: &Path, visit: &mut dyn FnMut(&Path, &fs::DirEntry) -> bool) {
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            let Ok(ty) = e.file_type() else { continue };
            if ty.is_dir() {
                if !skip(&name) {
                    stack.push(e.path());
                }
            } else if ty.is_file() {
                seen += 1;
                if seen > MAX_WALK || !visit(&e.path(), &e) {
                    return;
                }
            }
        }
    }
}

/// Texts of the project's source files, capped so the memory stays small.
pub fn source_texts(root: &str) -> Vec<(PathBuf, String)> {
    const MAX_FILES: usize = 400;
    const MAX_BYTES: usize = 3 << 20;
    let mut out = vec![];
    let mut total = 0usize;
    walk(Path::new(root), &mut |p, e| {
        if !crate::lexer::Lang::from_path(p).is_code() {
            return true;
        }
        let Some(len) = size(e) else { return true };
        if len > 256 << 10 {
            return true;
        }
        if out.len() >= MAX_FILES || total + len as usize > MAX_BYTES {
            return false;
        }
        if let Ok(text) = fs::read_to_string(p) {
            total += text.len();
            out.push((p.to_path_buf(), text));
        }
        true
    });
    out
}

pub fn list_files(root: &str) -> Vec<String> {
    let mut out = vec![];
    let base = Path::new(root);
    walk(base, &mut |p, _| {
        if let Ok(rel) = p.strip_prefix(base) {
            out.push(rel.to_string_lossy().into_owned());
        }
        true
    });
    out.sort();
    out
}

#[derive(Serialize)]
pub struct Hit {
    pub path: String,
    pub rel: String,
    pub line: usize,
    pub col: usize,
    pub text: String,
}

pub fn grep(root: &str, query: &str) -> Vec<Hit> {
    let needle = query.to_lowercase();
    if needle.is_empty() {
        return vec![];
    }
    let base = Path::new(root);
    let mut files = vec![];
    walk(base, &mut |p, e| {
        if size(e).is_some_and(|n| n <= MAX_GREP_FILE) {
            files.push(p.to_path_buf());
        }
        true
    });
    // The files are shared out between a few threads. Each takes the next file in order, and
    // they stop taking more once enough was found; every file before the last one taken has
    // then been read in full, so the first hits in the order of the walk are the same as when
    // going through the files one by one.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let found = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8);
    let mut all: Vec<(usize, Hit)> = std::thread::scope(|sc| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                sc.spawn(|| {
                    let mut mine = vec![];
                    while found.load(std::sync::atomic::Ordering::Relaxed) < MAX_GREP_HITS {
                        let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(p) = files.get(k) else { break };
                        let hits = grep_file(base, p, &needle);
                        found.fetch_add(hits.len(), std::sync::atomic::Ordering::Relaxed);
                        mine.extend(hits.into_iter().map(|h| (k, h)));
                    }
                    mine
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
    });
    all.sort_by_key(|(k, h)| (*k, h.line));
    all.truncate(MAX_GREP_HITS);
    let mut hits: Vec<Hit> = all.into_iter().map(|(_, h)| h).collect();
    hits.sort_by(|a, b| (&a.rel, a.line).cmp(&(&b.rel, b.line)));
    hits
}

fn grep_file(base: &Path, p: &Path, needle: &str) -> Vec<Hit> {
    let mut hits = vec![];
    let Ok(bytes) = fs::read(p) else { return hits };
    if bytes.iter().take(4096).any(|b| *b == 0) {
        return hits;
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return hits;
    };
    // Code is nearly always ASCII. Then lowering the whole file once gives, line by line,
    // exactly what lowering each line would, and a file without the query is passed over
    // without looking at its lines at all.
    let ascii = needle.is_ascii() && text.is_ascii();
    let lowered = if ascii { text.to_ascii_lowercase() } else { String::new() };
    if ascii && !lowered.contains(needle) {
        return hits;
    }
    let mut lower_lines = lowered.lines();
    for (i, l) in text.lines().enumerate() {
        let lower: Cow<str> = if ascii { Cow::Borrowed(lower_lines.next().unwrap_or("")) } else { Cow::Owned(l.to_lowercase()) };
        if let Some(b) = lower.find(needle) {
            hits.push(Hit {
                path: p.to_string_lossy().into_owned(),
                rel: p.strip_prefix(base).unwrap_or(p).to_string_lossy().into_owned(),
                line: i,
                col: lower[..b].encode_utf16().count(),
                text: l.trim().chars().take(160).collect(),
            });
            if hits.len() >= MAX_GREP_HITS {
                break;
            }
        }
    }
    hits
}

#[derive(Serialize)]
pub struct Sys {
    pub cores: usize,
    pub total_mb: u64,
    pub avail_mb: u64,
    pub home: String,
}

#[cfg(target_os = "linux")]
fn memory_mb() -> (u64, u64) {
    let info = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let field = |name: &str| {
        info.lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
            .unwrap_or(0)
            / 1024
    };
    (field("MemTotal:"), field("MemAvailable:"))
}

#[cfg(windows)]
fn memory_mb() -> (u64, u64) {
    #[repr(C)]
    struct MemoryStatus {
        length: u32,
        load: u32,
        total: u64,
        avail: u64,
        total_page: u64,
        avail_page: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended: u64,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32;
    }
    let mut s = MemoryStatus {
        length: std::mem::size_of::<MemoryStatus>() as u32,
        load: 0,
        total: 0,
        avail: 0,
        total_page: 0,
        avail_page: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended: 0,
    };
    if unsafe { GlobalMemoryStatusEx(&mut s) } == 0 {
        return (0, 0);
    }
    (s.total >> 20, s.avail >> 20)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn memory_mb() -> (u64, u64) {
    (0, 0)
}

pub fn sys_info() -> Sys {
    let (total, avail) = memory_mb();
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/".into());
    Sys {
        cores: std::thread::available_parallelism().map_or(1, |n| n.get()),
        total_mb: total,
        avail_mb: avail,
        home,
    }
}

/// Where `needle` (already lowercase) is in `hay`, ignoring case. `lower` is `hay` in
/// lowercase, worked out once by the caller: doing it here, for each match, was quadratic.
fn find_ci(hay: &str, lower: &str, needle: &str, from: usize) -> Option<(usize, usize)> {
    if lower.len() == hay.len() {
        return lower[from..].find(needle).map(|i| (from + i, from + i + needle.len()));
    }
    // Some letters change length in lowercase: fall back to comparing ASCII only.
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    (from..=h.len().checked_sub(n.len())?)
        .find(|&i| hay.is_char_boundary(i) && hay.is_char_boundary(i + n.len()) && h[i..i + n.len()].eq_ignore_ascii_case(n))
        .map(|i| (i, i + n.len()))
}

/// Replaces `query` with `with` in every text file of the project, ignoring case.
/// Returns how many files and how many places changed.
pub fn replace_all(root: &str, query: &str, with: &str, skip: &[PathBuf]) -> Result<(usize, usize), String> {
    let needle = query.to_lowercase();
    if needle.is_empty() {
        return Ok((0, 0));
    }
    let base = Path::new(root);
    let (mut files, mut places) = (0, 0);
    let mut failure: Option<String> = None;
    walk(base, &mut |p, e| {
        if size(e).is_none_or(|n| n > MAX_GREP_FILE) || skip.iter().any(|s| s == p) {
            return true;
        }
        let Ok(bytes) = fs::read(p) else { return true };
        if bytes.iter().take(4096).any(|b| *b == 0) {
            return true;
        }
        let Ok(text) = String::from_utf8(bytes) else { return true };
        let lower = text.to_lowercase();
        if lower.len() == text.len() && !lower.contains(&needle) {
            return true;
        }
        // Replacing would get around the permissions of a read-only file, as saving would.
        if fs::metadata(p).is_ok_and(|m| m.permissions().readonly()) {
            return true;
        }
        let mut out = String::with_capacity(text.len());
        let (mut at, mut n) = (0, 0);
        while let Some((s, e)) = find_ci(&text, &lower, &needle, at) {
            out.push_str(&text[at..s]);
            out.push_str(with);
            at = e;
            n += 1;
        }
        if n == 0 {
            return true;
        }
        out.push_str(&text[at..]);
        let tmp = p.with_extension(format!("replace-{}.tmp", std::process::id()));
        let written = fs::write(&tmp, out).and_then(|()| {
            if let Ok(m) = fs::metadata(p) {
                let _ = fs::set_permissions(&tmp, m.permissions());
            }
            fs::rename(&tmp, p)
        });
        match written {
            Ok(()) => {
                files += 1;
                places += n;
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                failure = Some(format!("{}: {e}", p.display()));
                return false;
            }
        }
        true
    });
    match failure {
        Some(e) => Err(e),
        None => Ok((files, places)),
    }
}

#[cfg(test)]
mod replace_tests {
    use super::*;

    #[test]
    fn grep_finds_the_same_in_ascii_and_other_files() {
        let dir = std::env::temp_dir().join(format!("proj-grep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.c"), "int Foo;\r\nfoo(); // FOO\nnone\n").unwrap();
        fs::write(dir.join("b.c"), "é Foo\nnone\nÉFOO\n").unwrap();
        fs::write(dir.join("c.c"), "nothing here\n").unwrap();
        let hits = grep(dir.to_str().unwrap(), "foo");
        let got: Vec<(String, usize, usize)> = hits.iter().map(|h| (h.rel.clone(), h.line, h.col)).collect();
        assert_eq!(got, vec![("a.c".into(), 0, 4), ("a.c".into(), 1, 0), ("b.c".into(), 0, 2), ("b.c".into(), 2, 1)]);
        assert!(grep(dir.to_str().unwrap(), "missing").is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Like saving, replacing must not get around a file's permissions.
    #[test]
    fn replacing_leaves_read_only_files_alone() {
        let dir = std::env::temp_dir().join(format!("proj-ro-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        fs::write(&f, "foo").unwrap();
        let mut perm = fs::metadata(&f).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&f, perm.clone()).unwrap();
        assert_eq!(replace_all(dir.to_str().unwrap(), "foo", "bar", &[]).unwrap(), (0, 0));
        assert_eq!(fs::read_to_string(&f).unwrap(), "foo");
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        let _ = fs::set_permissions(&f, perm);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replacing_in_the_project_ignores_case_and_keeps_line_endings() {
        let dir = std::env::temp_dir().join(format!("proj-replace-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.txt"), "Foo foo FOO\r\nbar\r\n").unwrap();
        fs::write(dir.join("src/b.txt"), "nothing\n").unwrap();
        fs::write(dir.join("src/bin.dat"), b"foo\0foo").unwrap();
        let (files, places) = replace_all(dir.to_str().unwrap(), "Foo", "baz", &[]).unwrap();
        assert_eq!((files, places), (1, 3));
        assert_eq!(fs::read_to_string(dir.join("src/a.txt")).unwrap(), "baz baz baz\r\nbar\r\n");
        assert_eq!(fs::read(dir.join("src/bin.dat")).unwrap(), b"foo\0foo");
        let skipped = dir.join("src/a.txt");
        fs::write(&skipped, "foo").unwrap();
        assert_eq!(replace_all(dir.to_str().unwrap(), "foo", "x", std::slice::from_ref(&skipped)).unwrap(), (0, 0));
        assert_eq!(find_ci("İfoo", &"İfoo".to_lowercase(), "foo", 0), Some((2, 5)));
        let _ = fs::remove_dir_all(&dir);
    }
}
