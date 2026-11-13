import * as ed from './editor.js'
import { $, h, invoke, listen, app, state, store, load, reg, toast, guard, join, activeTab, promptBox, SEP } from './core.js'
import * as runTerm from './runterm.js'

const MAX_OUT = 3000
const CRASHES = {
  [-1073741819]: 'invalid memory access',
  [-1073741571]: 'stack overflow',
  [-1073741676]: 'division by zero',
  [-1073741515]: 'a required DLL was not found',
  [-1073740940]: 'heap corruption',
}
const SIGNALS = { 4: 'illegal instruction', 6: 'the program aborted', 7: 'invalid memory access', 8: 'arithmetic error', 11: 'invalid memory access' }
const WIN = SEP === '\\'

/** Plain scrolling text, used for the output of Git commands. */
class Output {
  constructor(el) {
    this.el = el
    this.pending = ''
    this.tailText = document.createElement('span')
    this.tail = h('div', { class: 'tail' }, this.tailText)
    el.append(this.tail)
  }

  clear() {
    this.el.replaceChildren(this.tail)
    this.tailText.textContent = ''
    this.pending = ''
  }

  line(text, cls) {
    const el = document.createElement('div')
    if (cls) el.className = cls
    else if (/(?:^|\s)(?:fatal|error)\b:?/i.test(text)) el.className = 'er'
    el.textContent = text || ' '
    return el
  }

  append(text, cls) {
    const follow = this.el.scrollTop + this.el.clientHeight >= this.el.scrollHeight - 30
    this.pending += text.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, '').replace(/\r\n/g, '\n')
    const parts = this.pending.split('\n')
    this.pending = parts.pop()
    const frag = document.createDocumentFragment()
    for (const p of parts) frag.append(this.line(p, cls))
    this.el.insertBefore(frag, this.tail)
    this.tailText.textContent = this.pending
    this.tailText.className = cls ?? ''
    let extra = this.el.childElementCount - MAX_OUT
    while (extra-- > 0) this.el.firstChild.remove()
    if (follow) this.el.scrollTop = this.el.scrollHeight
  }

  flush() {
    if (this.pending) this.append('\n')
  }
}

export const buildOut = new Output($('v-build'))
let taskDone = null

listen('task-out', (e) => buildOut.append(e.payload.s))
listen('task-done', (e) => {
  const resolve = taskDone
  taskDone = null
  resolve?.(e.payload)
})

function setRunning(on) {
  state.running = on
  syncButtons()
}

export function syncButtons() {
  $('run').disabled = state.running
  $('stop').disabled = !state.running
  $('stop').hidden = !state.running
}

/** Runs a program with its output in the Output panel. Git uses it. */
export function runTask(view, title, program, args, cwd, { env = [] } = {}) {
  return new Promise((resolve, reject) => {
    app.showBottom('build')
    view.flush()
    view.append(`${title}\n`, 'hd')
    setRunning(true)
    $('st-msg').textContent = title
    taskDone = (code) => {
      view.flush()
      setRunning(false)
      $('st-msg').textContent = ''
      resolve(code)
    }
    invoke('task_start', { program, args, cwd, env, background: true }).catch((e) => {
      taskDone = null
      setRunning(false)
      view.append(`${e}\n`, 'er')
      reject(e)
    })
  })
}

function splitArgs(s) {
  const out = []
  for (const m of s.matchAll(/"([^"]*)"|'([^']*)'|(\S+)/g)) out.push(m[1] ?? m[2] ?? m[3])
  return out
}

const extOf = (path) => (/\.[^./\\]+$/.exec(path)?.[0] ?? '').toLowerCase()

// {file} is the open file, {dir} its folder, {out} a program built from it.
const COMMANDS = {
  '.py': [[WIN ? 'python' : 'python3', '{file}']],
  '.js': [['node', '{file}']],
  '.mjs': [['node', '{file}']],
  '.ts': [['npx', '--yes', 'tsx', '{file}']],
  '.go': [['go', 'run', '{file}']],
  '.java': [['java', '{file}']],
  '.php': [['php', '{file}']],
  '.rb': [['ruby', '{file}']],
  '.sh': [['bash', '{file}']],
  '.lua': [['lua', '{file}']],
  '.swift': [['swift', '{file}']],
  '.c': [['gcc', '{file}', '-o', '{out}'], ['{out}']],
  '.cpp': [['g++', '{file}', '-o', '{out}'], ['{out}']],
  '.cc': [['g++', '{file}', '-o', '{out}'], ['{out}']],
  '.rs': [['rustc', '{file}', '-o', '{out}'], ['{out}']],
  '.cs': [['dotnet', 'run']],
}

const customKey = (ext) => 'runcmd:' + ext
export const commandText = (ext) => load(customKey(ext), '') || (COMMANDS[ext] ?? []).map((s) => s.map((a) => (/\s/.test(a) ? `"${a}"` : a)).join(' ')).join(' && ')

async function stepsFor(tab) {
  const ext = extOf(tab.path)
  const dir = tab.path.slice(0, Math.max(tab.path.lastIndexOf(SEP), 0)) || state.root
  const name = tab.name.replace(/\.[^.]+$/, '')
  const outDir = join(state.root, '.assioma')
  const out = join(outDir, name + (WIN ? '.exe' : ''))
  let steps
  const custom = load(customKey(ext), '')
  if (custom) steps = custom.split('&&').map((c) => splitArgs(c.trim())).filter((a) => a.length)
  else if (ext === '.rs' && (await invoke('path_exists', { path: join(state.root, 'Cargo.toml') }))) steps = [['cargo', 'run']]
  else if (ext === '.go' && (await invoke('path_exists', { path: join(state.root, 'go.mod') }))) steps = [['go', 'run', '.']]
  else steps = COMMANDS[ext]
  if (!steps) return null
  if (steps.some((s) => s.some((a) => a.includes('{out}')))) await invoke('make_dirs', { path: outDir })
  const fill = (a) => a.replaceAll('{file}', tab.path).replaceAll('{dir}', dir).replaceAll('{out}', out).replaceAll('{name}', name)
  return { steps: steps.map((s) => s.map(fill)), cwd: state.root }
}

let stopRequested = false

function exitNote({ code, signal }) {
  const reset = '\x1b[0m\r\n'
  const crash = stopRequested ? null : (CRASHES[code] ?? SIGNALS[signal])
  if (crash) {
    const id = code === null ? `signal ${signal}` : `0x${(code >>> 0).toString(16).toUpperCase()}`
    return `${reset}\x1b[31mProgram crashed: ${crash} (${id})${reset}`
  }
  const text = stopRequested || code === null ? 'Process finished (stopped)' : `Process finished with exit code ${code}`
  return `${reset}\x1b[2m${text}${reset}`
}

async function doRun() {
  if (state.running || !state.root) return
  const tab = activeTab()
  if (!tab || tab.virtual) return toast('Open a file to run')
  if (!(await app.saveAll())) return
  const plan = await stepsFor(tab)
  if (!plan) return toast(`Assioma doesn't know how to run ${extOf(tab.path) || 'this file'} yet. Use Run > Edit Run Command`)
  stopRequested = false
  app.showBottom('run')
  setRunning(true)
  $('st-msg').textContent = 'Running'
  let result = null
  try {
    for (const [program, ...args] of plan.steps) {
      const shown = [program, ...args].map((a) => (/\s/.test(a) ? `"${a}"` : a)).join(' ')
      result = await runTerm.start({ program, args, cwd: plan.cwd, env: [], banner: `\x1b[2m$ ${shown}\x1b[0m\r\n` })
      if (stopRequested || (result.code ?? 0) !== 0 || result.signal) break
    }
  } catch (e) {
    toast(String(e))
  }
  setRunning(false)
  $('st-msg').textContent = ''
  if (result) await runTerm.note(exitNote(result))
  if (!$('work').hidden) ed.focus()
}

async function stop() {
  stopRequested = true
  await invoke('task_kill')
}

async function editCommand() {
  const tab = activeTab()
  if (!tab || tab.virtual) return toast('Open a file first')
  const ext = extOf(tab.path)
  const text = await promptBox(`Run command for ${ext || 'this file'}`, commandText(ext), 'Save')
  if (text === null) return
  store(customKey(ext), text.trim())
  toast('Saved. Use {file} for the open file and && to run steps one after another')
}

$('run').addEventListener('click', doRun)
$('stop').addEventListener('click', stop)

reg('run', 'Run File', 'S-F10', doRun)
reg('stop', 'Stop', 'C-F2', stop)
reg('run-config', 'Edit Run Command…', null, editCommand)
app.rerun = doRun
app.stopTask = stop
export const runTermFocused = () => runTerm.hasFocus()
export const runTermShown = () => runTerm.shown()
export const clearRun = () => runTerm.clear()
