#!/usr/bin/env python3
"""Packs the TextMate grammars of VS Code's built-in language extensions for Assioma.

Usage: pack_grammars.py /path/to/vscode/extensions
Writes src-tauri/grammars/ with one gzipped JSON file per grammar, an index of languages and
src-tauri/src/bundle.rs, which embeds them. The grammars come from the vscode repository (MIT)
and from the projects named in each grammar's own header.
"""
import glob, gzip, json, os, plistlib, re, sys

SRC = sys.argv[1]
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'src-tauri')
GRAM = os.path.join(OUT, 'grammars')

# extension folder -> grammar file names to take (None means all of them)
TAKE = {
    'bat': None, 'cpp': ['c.tmLanguage.json', 'cpp.tmLanguage.json', 'cpp.embedded.macro.tmLanguage.json', 'platform.tmLanguage.json'],
    'csharp': None, 'css': None, 'dart': None, 'diff': None, 'docker': None, 'dotenv': None, 'go': None, 'groovy': None,
    'html': None, 'ini': None, 'java': None, 'javascript': None,
    'json': ['JSON.tmLanguage.json', 'JSONC.tmLanguage.json'], 'less': None, 'lua': None, 'make': None,
    'markdown-basics': None, 'perl': ['perl.tmLanguage.json'], 'php': ['php.tmLanguage.json'], 'powershell': None,
    'python': None, 'r': None, 'ruby': None, 'rust': None, 'scss': None, 'shellscript': None, 'sql': None, 'swift': None,
    'typescript-basics': None, 'xml': ['xml.tmLanguage.json'], 'yaml': ['yaml.tmLanguage.json'],
}

def strip_jsonc(text):
    out, i, n, in_str = [], 0, len(text), False
    while i < n:
        c = text[i]
        if in_str:
            out.append(c)
            if c == '\\' and i + 1 < n:
                out.append(text[i + 1]); i += 1
            elif c == '"':
                in_str = False
        elif c == '"':
            in_str = True; out.append(c)
        elif text.startswith('//', i):
            while i < n and text[i] != '\n': i += 1
            continue
        elif text.startswith('/*', i):
            i = text.find('*/', i + 2); i = n if i < 0 else i + 2
            continue
        else:
            out.append(c)
        i += 1
    return re.sub(r',(\s*[}\]])', r'\1', ''.join(out))

def load_grammar(path):
    raw = open(path, 'rb').read()
    if path.endswith('.json'):
        return json.loads(raw.decode('utf-8'))
    return plistlib.loads(raw)

def comments_of(cfg_path):
    try:
        cfg = json.loads(strip_jsonc(open(cfg_path, encoding='utf-8').read()))
    except Exception:
        return {}
    c = cfg.get('comments', {})
    out = {}
    if c.get('lineComment'): out['line'] = c['lineComment']
    if c.get('blockComment'): out['block'] = c['blockComment']
    return out

def slim(node):
    """Drops what the engine never reads, which is a lot of the size."""
    if isinstance(node, dict):
        return {k: slim(v) for k, v in node.items() if k not in ('information_for_contributors', 'fileTypes', 'firstLineMatch', 'uuid', 'foldingStartMarker', 'foldingStopMarker') and not (k == 'comment' and isinstance(v, str))}
    if isinstance(node, list):
        return [slim(x) for x in node]
    return node

os.makedirs(GRAM, exist_ok=True)
for f in glob.glob(os.path.join(GRAM, '*')): os.remove(f)

languages, grammars, seen_scopes, third = [], [], set(), []
for ext in sorted(TAKE):
    base = os.path.join(SRC, ext)
    pkg = json.load(open(os.path.join(base, 'package.json'), encoding='utf-8'))
    contrib = pkg.get('contributes', {})
    wanted = TAKE[ext]
    lang_scope = {}
    for g in contrib.get('grammars', []):
        fname = os.path.basename(g['path'])
        if wanted is not None and fname not in wanted: continue
        scope = g['scopeName']
        if scope in seen_scopes: continue
        gpath = os.path.join(base, g['path'])
        if not os.path.exists(gpath): continue
        data = slim(load_grammar(gpath))
        data['scopeName'] = scope
        blob = gzip.compress(json.dumps(data, separators=(',', ':'), ensure_ascii=False).encode('utf-8'), 9)
        fn = scope + '.json.gz'
        open(os.path.join(GRAM, fn), 'wb').write(blob)
        seen_scopes.add(scope)
        entry = {'scope': scope, 'file': fn}
        if g.get('injectTo'): entry['injectTo'] = g['injectTo']
        if g.get('embeddedLanguages'): entry['embedded'] = g['embeddedLanguages']
        grammars.append(entry)
        if g.get('language') and 'embedded' not in scope and 'injection' not in scope:
            lang_scope.setdefault(g['language'], scope)
        origin = load_grammar(gpath).get('version') if isinstance(load_grammar(gpath), dict) else None
        third.append((scope, ext, origin))
    for l in contrib.get('languages', []):
        scope = lang_scope.get(l['id'])
        if not scope: continue
        item = {'id': l['id'], 'scope': scope, 'aliases': l.get('aliases', []), 'extensions': [e.lower() for e in l.get('extensions', [])], 'filenames': l.get('filenames', [])}
        if l.get('firstLine'): item['firstLine'] = l['firstLine']
        cfg = l.get('configuration')
        if cfg: item['comments'] = comments_of(os.path.normpath(os.path.join(base, cfg)))
        languages.append(item)

json.dump({'languages': languages, 'grammars': grammars}, open(os.path.join(GRAM, 'index.json'), 'w', encoding='utf-8'), separators=(',', ':'), ensure_ascii=False)

with open(os.path.join(OUT, 'src', 'bundle.rs'), 'w', encoding='utf-8') as f:
    f.write('// Generated by tools/pack_grammars.py. Do not edit.\n\n')
    f.write('pub static INDEX: &str = include_str!("../grammars/index.json");\n\n')
    f.write('pub static GRAMMARS: &[(&str, &[u8])] = &[\n')
    for g in grammars:
        f.write(f'    ("{g["scope"]}", include_bytes!("../grammars/{g["file"]}")),\n')
    f.write('];\n')

with open(os.path.join(OUT, '..', 'THIRD_PARTY.md'), 'w', encoding='utf-8') as f:
    f.write('# Third party grammars\n\nThe syntax grammars in `src-tauri/grammars` come from the built-in language extensions of\n[VS Code](https://github.com/microsoft/vscode) (MIT license, Copyright (c) 2015 - present Microsoft Corporation).\nMost of them were written in other projects, named in the `version` line below. See\n`extensions/cgmanifest.json` in the vscode repository for their licenses.\n\n')
    for scope, ext, origin in third:
        f.write(f'- `{scope}` (extension `{ext}`)' + (f': {origin}' if origin else '') + '\n')
size = sum(os.path.getsize(p) for p in glob.glob(os.path.join(GRAM, '*')))
print(f'{len(grammars)} grammars, {len(languages)} languages, {size // 1024} KB packed')
