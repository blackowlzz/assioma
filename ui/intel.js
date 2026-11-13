import * as ed from './editor.js'
import * as ai from './ai.js'
import { $, h, invoke, listen, app, state, settings, reg, toast, guard, promptBox, palette, showMenu, basename, SEP, runAction } from './core.js'

import { notify, closeNote } from './notify.js'

const compEl = $('completion'), hoverEl = $('hover'), sigEl = $('sig')

// Language servers (see lsp.rs). Each file goes to the server of its language, started the first
// time it is needed, as in VS Code. The status bar names the server of the file being edited.
const progress = new Map()
const warned = new Set()
let hoverSticky = false

export function syncDoc(id) {
  invoke('lsp_sync', { id })
    .then(() => requestSemantic(id))
    .catch(() => missing(id))
}

/** The first time a language has no server installed, say how to get one. */
async function missing(id) {
  const st = await invoke('lsp_status', { id }).catch(() => null)
  if (st?.state !== 'missing' || warned.has(st.id)) return
  warned.add(st.id)
  // Copy only when the way to get it is a command line ("pip install …", "npm install -g …").
  const cmd = /^(pip|pipx|npm|go|gem|cargo|rustup|dotnet) /.test(st.detail) ? st.detail.split(' (or ')[0] : null
  notify({
    id: 'lsp-' + st.id,
    title: `${st.server} isn't installed`,
    body: `Install it for errors as you type, go to definition, rename and smarter completion. ${st.detail}`,
    actions: [
      ...(cmd ? [{ label: 'Copy command', primary: true, run: () => app.copyText?.(cmd) }] : []),
      { label: 'Language servers…', run: () => app.openSettings?.('lsp') },
    ],
    onDismiss: () => closeNote('lsp-' + st.id),
  })
}

export function requestSemantic(id) {
  if (id === null) return
  invoke('lsp_semantic', { id })
    .then(() => id === state.active && ed.repaint())
    .catch(() => {})
}
app.requestSemanticAll = () => state.tabs.filter((t) => !t.virtual).forEach((t) => requestSemantic(t.id))

let statusToken = 0
export async function refreshStatus() {
  const el = $('st-lsp')
  const id = state.active
  const token = ++statusToken
  const st = id === null ? null : await invoke('lsp_status', { id }).catch(() => null)
  if (token !== statusToken) return
  el.dataset.server = st?.id ?? ''
  if (!st || st.state === 'none') {
    el.textContent = ''
    return
  }
  const busy = progress.get(st.id)
  el.textContent = {
    running: busy ? `${st.server}: ${busy}` : st.server,
    idle: st.server,
    starting: `${st.server} starting…`,
    missing: `${st.server} not installed`,
    off: `${st.server} off`,
    stopped: `${st.server} stopped`,
  }[st.state] ?? st.server
  el.title = st.detail || `Language server: ${st.server}`
  el.classList.toggle('warn', st.state === 'missing' || st.state === 'stopped')
}

listen('lsp-status', (e) => {
  const { server, state: s } = e.payload
  if (s !== 'running') progress.delete(server)
  // A crashed server starts again for the files that need it, as VS Code does (up to 4 times).
  if (s === 'crashed') state.tabs.filter((t) => !t.virtual).forEach((t) => syncDoc(t.id))
  if (s === 'running') state.tabs.filter((t) => !t.virtual).forEach((t) => requestSemantic(t.id))
  refreshStatus()
})
listen('lsp-progress', (e) => {
  const { server, text, percent } = e.payload
  if (text) progress.set(server, percent != null ? `${text} ${percent}%` : text)
  else progress.delete(server)
  refreshStatus()
})
listen('lsp-message', (e) => toast(e.payload))

$('st-lsp').addEventListener('click', (e) => {
  const server = e.currentTarget.dataset.server
  if (!server) return
  const r = e.currentTarget.getBoundingClientRect()
  showMenu(r.left, r.top - 4, [
    ['Restart Language Server', () => guard(invoke('lsp_restart', { server }).then(() => state.tabs.filter((t) => !t.virtual).forEach((t) => syncDoc(t.id))))],
    ['Language Servers…', () => app.openSettings?.('lsp')],
  ])
})

const comp = { open: false, items: [], shown: [], sel: 0, prefix: '', token: 0, typed: '', incomplete: false }
let compTimer = 0

let ghost = null
let viewKey = ''
let ghostTimer = 0
let ghostToken = 0

let aiTimer = 0

// Hides what is shown. Timers and requests in flight are left alone: whatever they bring is checked against the caret.
function dropGhost() {
  if (!ghost) return
  ghost = null
  ed.setGhost('')
}

function clearGhost() {
  ghostToken++
  clearTimeout(ghostTimer)
  clearTimeout(aiTimer)
  dropGhost()
}
app.clearGhost = clearGhost

/** The built-in AI finishes the line when it is installed and the setting is on. */
const smart = () => settings.ghost && settings.ghostAi && ai.isBuiltin() && ai.ready()

export function viewChanged(v) {
  viewKey = `${v.rev}:${v.line}:${v.col}`
  if (!ghost || ghost.key === viewKey) return
  // The caret moved along the suggestion because the typed letters matched it: keep it.
  if (ghost.base !== undefined && v.line === ghost.line && v.col >= ghost.base && v.col <= ghost.col && v.rev >= ghost.rev) {
    ghost.rev = v.rev
    ghost.base = v.col
    if (v.col === ghost.col) ghost.key = viewKey
    return
  }
  dropGhost()
}

async function requestGhost() {
  if (!settings.ghost || state.active === null || comp.open) return
  const token = ++ghostToken
  await ed.idle()
  const r = await invoke('line_suggest', { id: state.active, root: state.root ?? '' }).catch(() => null)
  if (token !== ghostToken || !r || comp.open || r.key !== viewKey) return
  const [rev, line, col] = r.key.split(':').map(Number)
  ghost = { ...r, rev, line, col, base: undefined }
  ed.setGhost(r.text)
}

// Typing these can add a closing character of its own, which would change the rest of the line.
const PAIRS = /[()[\]{}"'<>]/

async function requestAi() {
  if (!smart() || state.active === null || comp.open) return
  await ed.idle()
  invoke('ai_suggest', { id: state.active, size: ai.size() }).catch(() => {})
}

listen('ai-suggestion', (e) => {
  const r = e.payload
  if (!smart() || comp.open || r.key !== viewKey || ghost?.text === r.text) return
  const [rev, line, col] = r.key.split(':').map(Number)
  ghost = { text: r.text, key: r.key, rev, line, col, base: undefined }
  ed.setGhost(r.text)
})

/** Typed text that matches the start of the suggestion eats into it, so it stays on screen. */
export function typed(s = '') {
  clearTimeout(ghostTimer)
  clearTimeout(aiTimer)
  if (smart()) invoke('ai_suggest_cancel').catch(() => {})
  if (ghost && s && !PAIRS.test(s) && ghost.text.length > s.length && ghost.text.startsWith(s)) {
    const n = [...s].length
    ghost.text = ghost.text.slice(s.length)
    ghost.base ??= ghost.col
    ghost.col += n
    ed.setGhost(ghost.text, ghost.col - ghost.base)
    return
  }
  ghostTimer = setTimeout(requestGhost, 60)
  if (smart()) aiTimer = setTimeout(requestAi, 280)
}

function acceptGhost(all) {
  const text = all ? ghost.text : (ghost.text.match(/^\s*\S+/)?.[0] ?? ghost.text)
  clearGhost()
  ed.send({ t: 'insert', s: text })
  typed()
}

export function hidePopups() {
  clearGhost()
  comp.open = false
  comp.token++
  clearTimeout(compTimer)
  compEl.hidden = true
  hoverEl.hidden = true
  hoverSticky = false
  sigEl.hidden = true
}

function place(el) {
  const c = ed.caretBox()
  const box = ed.host().getBoundingClientRect()
  const below = c.y + c.lh + el.offsetHeight + 4 < box.height
  el.style.left = Math.max(6, Math.min(c.x, box.width - el.offsetWidth - 10)) + 'px'
  el.style.top = (below ? c.y + c.lh + 2 : Math.max(0, c.y - el.offsetHeight - 2)) + 'px'
}

function renderComp() {
  const p = comp.prefix.toLowerCase()
  comp.shown = comp.items.filter((i) => (i.key ?? i.insert).toLowerCase().startsWith(p)).slice(0, 40)
  if (!comp.shown.length) {
    comp.open = false
    compEl.hidden = true
    return
  }
  comp.open = true
  clearGhost()
  comp.sel = Math.min(comp.sel, comp.shown.length - 1)
  compEl.replaceChildren(...comp.shown.map((it, i) => h('div', { class: 'ci' + (i === comp.sel ? ' on' : ''), onpointerdown: (e) => (e.preventDefault(), acceptComp(i)) }, h('span', { class: 'lab' }, it.label), h('span', { class: 'det' }, it.detail))))
  compEl.hidden = false
  place(compEl)
}

function moveComp(d) {
  comp.sel = (comp.sel + d + comp.shown.length) % comp.shown.length
  ;[...compEl.children].forEach((c, i) => c.classList.toggle('on', i === comp.sel))
  compEl.children[comp.sel].scrollIntoView({ block: 'nearest' })
}

function acceptComp(i) {
  const it = comp.shown[i]
  hidePopups()
  if (it) {
    ed.send(it.stops ? { t: 'snippet', s: it.insert, stops: it.stops } : { t: 'complete', s: it.insert })
    if (it.insert.endsWith('(')) requestSig()
  }
}

async function requestComp(force) {
  const id = state.active
  if (id === null) return
  const token = ++comp.token
  comp.typed = ''
  await ed.idle()
  try {
    // The server's suggestions first, then the words and snippets it doesn't have, as VS Code
    // keeps the snippets of extensions next to the language's own suggestions.
    const [server, words] = await Promise.all([invoke('lsp_complete', { id, force }).catch(() => null), invoke('word_complete', { id, force }).catch(() => null)])
    if (token !== comp.token) return
    const seen = new Set((server?.items ?? []).map((i) => i.label))
    const r = { prefix: (server ?? words)?.prefix ?? '', incomplete: !!server?.incomplete, items: [...(server?.items ?? []), ...(words?.items ?? []).filter((i) => !seen.has(i.label))] }
    comp.items = r.items
    comp.incomplete = r.incomplete
    comp.prefix = r.prefix + comp.typed
    comp.sel = 0
    if (r.items.length) renderComp()
  } catch (e) {
    console.warn(e)
  }
}

async function requestSig() {
  const id = state.active
  if (id === null) return
  await ed.idle()
  try {
    const s = await invoke('lsp_signature', { id })
    if (!s) {
      sigEl.hidden = true
      return
    }
    const [label, range] = s
    if (range) sigEl.replaceChildren(label.slice(0, range[0]), h('b', {}, label.slice(range[0], range[1])), label.slice(range[1]))
    else sigEl.textContent = label
    sigEl.hidden = false
    const c = ed.caretBox()
    sigEl.style.left = Math.max(6, c.x - 20) + 'px'
    sigEl.style.top = Math.max(0, c.y - sigEl.offsetHeight - 3) + 'px'
  } catch {
    sigEl.hidden = true
  }
}
let sigTimer = 0

const isWordChar = (s) => s.length === 1 && /[\p{L}\p{N}_]/u.test(s)

export function beforeKey(e, name) {
  if (name === 'Escape' && (!hoverEl.hidden || !sigEl.hidden || comp.open)) {
    hidePopups()
    return true
  }
  if (comp.open) {
    if (name === 'ArrowDown') return moveComp(1), true
    if (name === 'ArrowUp') return moveComp(-1), true
    if (name === 'Enter' || name === 'Tab') return acceptComp(comp.sel), true
  }
  if (ghost) {
    if (name === 'Tab') return acceptGhost(true), true
    if (name === 'C-ArrowRight') return acceptGhost(false), true
    if (name === 'Escape') return clearGhost(), true
  }
  if (name === 'C- ') {
    requestComp(true)
    return true
  }
  return false
}

export function afterType(s) {
  hoverEl.hidden = true
  if (s === '(' || s === ',') {
    clearTimeout(sigTimer)
    sigTimer = setTimeout(requestSig, 120)
  } else if (s === ')' || s === ';') sigEl.hidden = true
  if (isWordChar(s)) {
    comp.typed += s
    if (comp.open) {
      comp.prefix += s
      renderComp()
      if (comp.incomplete || !comp.open) {
        clearTimeout(compTimer)
        compTimer = setTimeout(() => requestComp(false), 180)
      }
    } else {
      clearTimeout(compTimer)
      compTimer = setTimeout(() => requestComp(false), 180)
    }
  } else if (s === 'Backspace' && comp.open && comp.prefix.length > 1) {
    comp.prefix = comp.prefix.slice(0, -1)
    renderComp()
  } else {
    const wasOpen = comp.open
    comp.open = false
    comp.token++
    clearTimeout(compTimer)
    compEl.hidden = true
    if (!wasOpen && (s === '.' || s === '>' || s === ':')) compTimer = setTimeout(() => requestComp(false), 180)
  }
}

export function showHover(text, atCaret = true, point = null) {
  hoverEl.textContent = text
  hoverEl.hidden = false
  hoverSticky = atCaret
  if (atCaret) place(hoverEl)
  else {
    const box = ed.host().getBoundingClientRect()
    hoverEl.style.left = Math.max(6, Math.min(point.x - box.left, box.width - hoverEl.offsetWidth - 10)) + 'px'
    const y = point.y - box.top
    hoverEl.style.top = (y + hoverEl.offsetHeight + 24 < box.height ? y + 16 : Math.max(0, y - hoverEl.offsetHeight - 8)) + 'px'
  }
}

let hoverToken = 0
/** The pointer rests on a cell: its problem if it has one, otherwise what the server says about it. */
export function mouseHover(cell, point) {
  const id = state.active
  if (id === null || comp.open) return
  const token = ++hoverToken
  invoke('doc_probe', { id, row: cell.line, vcol: cell.vcol }).then(async ([diag, word]) => {
    if (token !== hoverToken) return
    if (diag) return showHover(diag, false, point)
    if (!word) return
    const text = await invoke('lsp_hover_at', { id, row: cell.line, vcol: cell.vcol }).catch(() => '')
    if (token === hoverToken && text && state.active === id) showHover(text, false, point)
  })
}

export function hoverEnd() {
  hoverToken++
  if (!hoverSticky) hoverEl.hidden = true
}

export async function applyEdits(changes) {
  for (const c of changes) {
    const d = await invoke('doc_open', { path: c.path })
    await app.ensureTab(d)
    await invoke('doc_apply_edits', { id: d.id, edits: c.edits })
  }
  ed.repaint()
  app.markAllDirty?.()
}
listen('apply-edit', (e) => guard(applyEdits(e.payload)))

/** Runs a request to the language server of the open file; its errors become a toast. */
async function withServer(fn) {
  if (state.active === null) return
  await ed.idle()
  try {
    await fn(state.active)
  } catch (e) {
    toast(e)
  }
}

const gotoDefinition = () =>
  withServer(async (id) => {
    const loc = await invoke('lsp_definition', { id })
    if (loc) await app.jumpTo(loc.path, loc.line, loc.col)
    else toast('No definition found')
  })

const showHoverAtCaret = () =>
  withServer(async (id) => {
    const s = await invoke('lsp_hover', { id })
    if (s) showHover(s)
    else toast('Nothing to show here')
  })

const findReferences = () =>
  withServer(async (id) => {
    const hits = await invoke('lsp_references', { id })
    app.showHits('References', `${hits.length} found`, hits)
  })

const rename = () =>
  withServer(async (id) => {
    const v = ed.lastView()
    const [, word] = await invoke('doc_probe', { id, row: v.cur[0], vcol: v.cur[1] })
    if (!word) return toast('Place the caret on a symbol')
    const name = await promptBox(`Rename ${word}`, word, 'Rename')
    if (!name || name === word) return
    const changes = await invoke('lsp_rename', { id, name })
    await applyEdits(changes)
    toast(`Renamed in ${changes.length} file${changes.length === 1 ? '' : 's'}`)
  })

const codeActions = () =>
  withServer(async (id) => {
    const list = await invoke('lsp_code_actions', { id })
    if (!list.length) return toast('No quick fixes here')
    const c = ed.caretBox()
    const box = ed.host().getBoundingClientRect()
    showMenu(
      box.left + c.x,
      box.top + c.y + c.lh + 2,
      list.map((a) => [
        a.title,
        () => {
          if (a.edit.length) guard(applyEdits(a.edit))
          else if (a.command) guard(invoke('lsp_exec_command', { command: a.command, id }).then(() => ed.repaint()))
        },
      ]),
    )
  })

const formatDocument = () =>
  withServer(async (id) => {
    await invoke('lsp_format', { id })
    ed.repaint()
  })

const KIND = { 2: 'mod', 3: 'ns', 5: 'cl', 6: 'm', 7: 'p', 8: 'f', 9: 'c', 10: 'en', 11: 'i', 12: 'fn', 13: 'v', 14: 'k', 22: 'e', 23: 'st' }

const fileSymbols = () =>
  withServer(async (id) => {
    const syms = await invoke('lsp_symbols', { id })
    palette({
      placeholder: 'Go to symbol in file',
      query: async (q) => syms.filter((s) => s.name.toLowerCase().includes(q.toLowerCase())).slice(0, 80).map((s) => ({ ...s, open: () => ed.send({ t: 'goto', line: s.line, col: s.col }) })),
      render: (s) => h('div', {}, h('b', {}, '  '.repeat(s.depth) + s.name), h('span', {}, s.detail), h('span', { class: 'kind' }, KIND[s.kind] ?? '')),
    })
  })

function workspaceSymbols() {
  const id = state.active
  if (id === null) return toast('Open a file of the language to search')
  palette({
    placeholder: 'Go to symbol in workspace',
    debounce: 200,
    query: async (q) => {
      if (q.length < 2) return []
      const hits = await invoke('lsp_workspace_symbols', { query: q, id }).catch((e) => (toast(e), []))
      return hits.map((x) => ({ ...x, open: () => guard(app.jumpTo(x.path, x.line, x.col)) }))
    },
    render: (x) => h('div', {}, h('b', {}, x.text), h('span', {}, basename(x.path))),
  })
}

reg('goto-def', 'Go to Definition', 'F12', gotoDefinition)
reg('references', 'Find All References', 'S-F12', findReferences)
reg('rename-symbol', 'Rename Symbol…', 'F2', rename)
reg('quick-fix', 'Quick Fix…', 'C-.', codeActions)
reg('format-doc', 'Format Document', 'A-S-f', formatDocument)
reg('file-symbols', 'Go to Symbol in File…', 'C-S-o', fileSymbols)
reg('workspace-symbols', 'Go to Symbol in Workspace…', 'C-t', workspaceSymbols)
reg('show-hover', 'Show Hover', null, showHoverAtCaret)
reg('parameter-hints', 'Trigger Parameter Hints', 'C-S- ', requestSig)

app.gotoDefinition = () => guard(gotoDefinition())
