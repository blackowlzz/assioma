import * as ed from './editor.js'
import * as ai from './ai.js'
import { $, h, invoke, listen, app, state, settings, reg, toast, guard, promptBox, palette, showMenu, basename, SEP, runAction } from './core.js'

const compEl = $('completion'), hoverEl = $('hover'), sigEl = $('sig')

/** There is no language server: completion is word based and the AI does the rest. */
export function syncDoc() {}

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
    const r = await invoke('word_complete', { id, force })
    if (token !== comp.token) return
    comp.items = r.items
    comp.incomplete = r.incomplete
    comp.prefix = r.prefix + comp.typed
    comp.sel = 0
    if (r.items.length) renderComp()
  } catch (e) {
    console.warn(e)
  }
}

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
