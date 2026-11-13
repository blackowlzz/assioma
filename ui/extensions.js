import { $, h, invoke, app, state, reg, toast, guard, confirmBox, seg, iconBtn } from './core.js'
import * as theme from './theme.js'

const view = $('v-ext')

const CATEGORIES = [['', 'All'], ['Themes', 'Themes'], ['Programming Languages', 'Languages'], ['Snippets', 'Snippets']]

let tab = 'browse'
let category = ''
let query = ''
let installed = []
let page = { items: [], total: 0 }
let loading = false
let failed = ''
let timer = 0
let token = 0
const busy = new Set()

const search = h('input', { class: 'ext-q', placeholder: 'Search Open VSX', spellcheck: 'false' })
const list = h('div', { class: 'ext-list' })
const bar = h('div', { class: 'ext-bar' })
view.replaceChildren(bar, list)

const compact = (n) => (n >= 1e6 ? (n / 1e6).toFixed(1) + 'M' : n >= 1e3 ? Math.round(n / 1e3) + 'k' : String(n))
const isInstalled = (id) => installed.find((x) => x.id === id)

async function loadInstalled() {
  installed = await invoke('ext_list').catch(() => [])
}

async function loadPage(more = false) {
  const mine = ++token
  loading = true
  failed = ''
  render()
  try {
    const r = await invoke('ext_search', { query, category, offset: more ? page.items.length : 0 })
    if (mine !== token) return
    page = more ? { items: [...page.items, ...r.items], total: r.total } : r
  } catch (e) {
    if (mine !== token) return
    failed = String(e)
    if (!more) page = { items: [], total: 0 }
  } finally {
    if (mine === token) loading = false
  }
  render()
}

async function applyChanges(info) {
  if (info) {
    for (const t of info.themes.slice(0, 20)) {
      try {
        const c = await invoke('ext_theme', { id: info.id, path: t.path, label: t.label, light: t.light })
        theme.addExtTheme(info.id, c)
      } catch (e) {
        console.warn(e)
      }
    }
  }
  const changed = await invoke('docs_relang')
  if (changed.includes(state.active)) app.repaintEditor?.()
}

async function install(item) {
  busy.add(item.id)
  render()
  try {
    const info = await invoke('ext_install', { item })
    await loadInstalled()
    await applyChanges(info)
    const what = [info.themes.length && `${info.themes.length} theme${info.themes.length > 1 ? 's' : ''}`, info.languages.length && 'languages', info.snippets && 'snippets'].filter(Boolean).join(', ')
    toast(`${info.display} installed` + (what ? `: ${what}` : info.partial ? '. It only adds features Assioma cannot run.' : ''))
  } catch (e) {
    toast(e)
  } finally {
    busy.delete(item.id)
    render()
  }
}

async function uninstall(ext) {
  if (!(await confirmBox(`Remove ${ext.display}?`, 'Its themes, languages and snippets go away.', 'Remove'))) return
  busy.add(ext.id)
  render()
  try {
    await invoke('ext_remove', { id: ext.id })
    theme.removeExtThemes(ext.id)
    await loadInstalled()
    await applyChanges(null)
  } catch (e) {
    toast(e)
  } finally {
    busy.delete(ext.id)
    render()
  }
}

async function useTheme(ext, t) {
  const id = `ext:${ext.id}:${t.label}`
  if (!theme.themes().some((x) => x.id === id)) {
    const c = await invoke('ext_theme', { id: ext.id, path: t.path, label: t.label, light: t.light })
    theme.addExtTheme(ext.id, c)
  }
  theme.setTheme(id)
  app.themeChanged?.()
}

function icon(src, name) {
  const letter = h('span', { class: 'ext-ico' }, (name || '?').trim().charAt(0).toUpperCase())
  if (!src) return letter
  const img = h('img', { class: 'ext-ico', src, alt: '', loading: 'lazy' })
  img.addEventListener('error', () => img.replaceWith(letter))
  return img
}

function installedRow(ext) {
  const parts = [
    ext.themes.length ? h('div', { class: 'ext-themes' }, ...ext.themes.slice(0, 8).map((t) => h('button', { class: 'chip', title: `Use ${t.label}`, onclick: () => guard(useTheme(ext, t)) }, t.label))) : null,
    ext.languages.length ? h('div', { class: 'small muted' }, `Languages: ${[...new Set(ext.languages)].join(', ')}`) : null,
    ext.snippets ? h('div', { class: 'small muted' }, `${ext.snippets} snippet file${ext.snippets > 1 ? 's' : ''}`) : null,
    ext.partial ? h('div', { class: 'small muted', title: 'Extensions that run code, such as language servers, are not supported' }, 'Only its themes, languages and snippets work here') : null,
  ]
  return h(
    'div',
    { class: 'ext' },
    icon(null, ext.display),
    h('div', { class: 'ext-body' },
      h('div', { class: 'ext-head' }, h('span', { class: 'ext-name' }, ext.display), h('span', { class: 'small muted' }, ext.version), h('span', { class: 'grow' }),
        h('button', { class: 'plain', disabled: busy.has(ext.id), onclick: () => guard(uninstall(ext)) }, busy.has(ext.id) ? 'Removing…' : 'Remove')),
      h('div', { class: 'ext-desc small muted' }, ext.description),
      ...parts,
    ),
  )
}

function browseRow(item) {
  const have = isInstalled(item.id)
  const working = busy.has(item.id)
  return h(
    'div',
    { class: 'ext' },
    icon(item.icon, item.display),
    h('div', { class: 'ext-body' },
      h('div', { class: 'ext-head' }, h('span', { class: 'ext-name' }, item.display), h('span', { class: 'small muted' }, item.version), h('span', { class: 'grow' }),
        have ? h('span', { class: 'small muted' }, 'Installed') : h('button', { disabled: working, onclick: () => guard(install(item)) }, working ? 'Installing…' : 'Install')),
      h('div', { class: 'ext-desc small muted' }, item.description),
      h('div', { class: 'small faint' }, `${item.ns}${item.verified ? ' ✓' : ''} · ${compact(item.downloads)} downloads`),
    ),
  )
}

function render() {
  bar.replaceChildren(
    ...[
    search,
    h('div', { class: 'ext-tabs' }, seg([['browse', 'Browse'], ['installed', `Installed${installed.length ? ' ' + installed.length : ''}`]], tab, (t) => {
      tab = t
      render()
      if (t === 'browse' && !page.items.length && !loading) loadPage()
    })),
    tab === 'browse'
      ? h('div', { class: 'ext-cats' }, ...CATEGORIES.map(([v, label]) => h('button', { class: 'chip', 'aria-pressed': String(category === v), onclick: () => ((category = v), loadPage()) }, label)))
      : null,
  ].filter(Boolean),
  )
  if (tab === 'installed') {
    const q = query.trim().toLowerCase()
    const rows = installed.filter((x) => !q || `${x.display} ${x.id} ${x.description}`.toLowerCase().includes(q))
    list.replaceChildren(...(rows.length ? rows.map(installedRow) : [h('div', { class: 'notice' }, installed.length ? 'No match' : 'Nothing is installed yet. Themes, languages and snippets from Open VSX can be added in Browse.')]))
    return
  }
  const rows = page.items.map(browseRow)
  if (failed) rows.push(h('div', { class: 'notice' }, failed, h('div', {}, h('button', { onclick: () => loadPage() }, 'Try again'))))
  else if (loading) rows.push(h('div', { class: 'notice' }, 'Loading…'))
  else if (!rows.length) rows.push(h('div', { class: 'notice' }, 'No extensions found'))
  else if (page.items.length < page.total) rows.push(h('div', { style: 'padding:6px 12px' }, h('button', { onclick: () => loadPage(true) }, 'Show more')))
  list.replaceChildren(...rows)
}

search.addEventListener('input', () => {
  query = search.value
  clearTimeout(timer)
  if (tab === 'installed') return render()
  timer = setTimeout(() => loadPage(), 350)
})

export async function shown() {
  await loadInstalled()
  if (!installed.length || query || page.items.length) tab = tab === 'installed' && !installed.length ? 'browse' : tab
  render()
  if (tab === 'browse' && !page.items.length && !loading) loadPage()
  search.focus()
}

export const panelActions = () => h('span', { class: 'headacts' }, iconBtn('refresh', 'Refresh', () => guard(tab === 'browse' ? loadPage() : loadInstalled().then(render))))

reg('extensions', 'Extensions…', 'C-S-x', () => ($('work').hidden ? toast('Open a folder to manage extensions') : app.showLeft('ext', true)))
