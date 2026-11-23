import { Fragment, useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'
import { apiRequest, type InventoryEntry, type Page } from './api'
import { EmptyState, LoadingState, ProblemNotice, TechnicalDetails } from './components'
import { fileSize } from './release-information'
import { useResource } from './use-resource'

const pageSize = 50
const enc = encodeURIComponent
const filename = (path: string) => path.split('/').at(-1) || 'Artifact root'
const parentOf = (path: string) => path.slice(0, Math.max(0, path.lastIndexOf('/')))
type Position = { directory: string; search: string }
type Order = 'folders' | 'size'
type Query = Position & { cursor: string; directoriesOnly?: boolean; order: Order }
type InventorySource = { key: string; load: (query: Query, signal: AbortSignal) => Promise<Page<InventoryEntry>> }
type Choice = 'file' | 'directory' | 'any'

function artifactSource(id: string): InventorySource {
  return { key: id, load: ({ directory, search, order, cursor, directoriesOnly }, signal) => {
    const params = new URLSearchParams({
      pageSize: String(pageSize), order,
      ...(search ? { q: search } : { parent: directory }),
      ...(directoriesOnly ? { kind: 'Directory' } : {}),
      ...(cursor ? { cursor } : {}),
    })
    return apiRequest<Page<InventoryEntry>>(`/artifacts/${enc(id)}/inventory?${params}`, { authenticated: true, signal }).then(result => result.value)
  } }
}

function useInventory(source: InventorySource, query: Omit<Query, 'cursor'>) {
  const scope = JSON.stringify([source.key, query])
  const [position, setPosition] = useState<{ scope: string; history: string[] }>({ scope, history: [''] })
  const history = position.scope === scope ? position.history : ['']
  const cursor = history.at(-1)!
  const result = useResource(`${scope}:${cursor}`, signal => source.load({ ...query, cursor }, signal))
  return { ...result, number: history.length, back: history.length > 1 ? () => setPosition({ scope, history: history.slice(0, -1) }) : undefined,
    next: result.value?.nextCursor ? () => setPosition({ scope, history: [...history, result.value!.nextCursor!] }) : undefined }
}

function Pages({ page, label }: { page: ReturnType<typeof useInventory>; label: string }) {
  return <nav className="browser-pages" aria-label={label}>
    <button type="button" className="text-button" disabled={!page.back} onClick={page.back}>Previous page</button>
    <span aria-live="polite">Page {page.number} · up to {pageSize} entries</span>
    <button type="button" className="text-button" disabled={!page.next} onClick={page.next}>Next page</button>
  </nav>
}

function DirectoryBranch({ path, source, current, open, onToggle, navigate }: {
  path: string; source: InventorySource; current: string; open: Set<string>; onToggle: (path: string) => void; navigate: (path: string) => void
}) {
  const expanded = open.has(path)
  const name = filename(path)
  return <li>
    <div className="browser-branch">
      <button type="button" className="browser-disclosure" aria-label={`${expanded ? 'Collapse' : 'Expand'} ${name}`} aria-expanded={expanded}
        onClick={() => onToggle(path)}>{expanded ? '▾' : '▸'}</button>
      <button type="button" className="browser-folder" aria-current={current === path ? 'location' : undefined} onClick={() => navigate(path)}
        onKeyDown={event => {
          if ((event.key === 'ArrowRight' && !expanded) || (event.key === 'ArrowLeft' && expanded)) {
            event.preventDefault()
            onToggle(path)
          }
        }}>{name}</button>
    </div>
    {expanded && <DirectoryChildren {...{ path, source, current, open, onToggle, navigate }} />}
  </li>
}

function DirectoryChildren({ path, source, current, open, onToggle, navigate }: Parameters<typeof DirectoryBranch>[0]) {
  const page = useInventory(source, { directory: path, search: '', order: 'folders', directoriesOnly: true })
  if (page.error) return <div className="browser-branch-detail"><ProblemNotice error={page.error} retry={page.retry} /></div>
  if (page.loading) return <div className="browser-branch-detail"><LoadingState label="Loading folders…" /></div>
  return <div className="browser-branch-detail">
    {page.value!.items.length ? <ul>{page.value!.items.map(entry =>
      <DirectoryBranch key={entry.path} path={entry.path} {...{ source, current, open, onToggle, navigate }} />)}</ul> : <small>No subfolders</small>}
    {(page.back || page.next) && <Pages page={page} label={`Subfolders of ${filename(path)}`} />}
  </div>
}

function DirectoryTree({ source, current, navigate }: { source: InventorySource; current: string; navigate: (path: string) => void }) {
  const [open, setOpen] = useState(() => new Set(['']))
  useEffect(() => {
    if (current) setOpen(old => {
      const parts = current.split('/')
      const ancestry = parts.map((_, index) => parts.slice(0, index).join('/'))
      const siblings = [...old].filter(path => !ancestry.includes(path)).slice(-8)
      return new Set([...ancestry, ...siblings])
    })
  }, [current])
  return <nav className="browser-tree" aria-label="Directory tree"><ul><DirectoryBranch path="" {...{ source, current, open, navigate }}
    onToggle={path => setOpen(old => {
      const next = new Set(old)
      if (next.has(path)) {
        for (const entry of next) if (!path || entry === path || entry.startsWith(path + '/')) next.delete(entry)
      } else {
        // Keep expanded siblings within a fixed budget, leaving room for deep ancestor chains.
        const unrelated = [...next].filter(entry => entry && !path.startsWith(entry + '/') && !current.startsWith(entry + '/'))
        if (unrelated.length >= 8) {
          const oldest = unrelated[0]
          for (const entry of next) if (entry === oldest || entry.startsWith(oldest + '/')) next.delete(entry)
        }
        next.add(path)
      }
      return next
    })} /></ul></nav>
}

function Breadcrumbs({ directory, navigate }: { directory: string; navigate: (path: string) => void }) {
  const parts = directory ? directory.split('/') : []
  return <nav className="browser-crumbs" aria-label="Artifact path">
    {directory && <button type="button" className="text-button" onClick={() => navigate(parentOf(directory))}>Parent directory</button>}
    <button type="button" className="text-button" aria-current={!directory ? 'location' : undefined} onClick={() => navigate('')}>Root</button>
    {parts.map((name, index) => <Fragment key={index}><span aria-hidden="true">/</span>
      <button type="button" className="text-button" aria-current={index === parts.length - 1 ? 'location' : undefined}
        onClick={() => navigate(parts.slice(0, index + 1).join('/'))}>{name}</button>
    </Fragment>)}
  </nav>
}

function BrowserList({ items, selected, search, select, open, focusAfterNavigation }: {
  items: InventoryEntry[]; selected?: InventoryEntry; search: boolean; select: (entry: InventoryEntry) => void; open: (path: string) => void;
  focusAfterNavigation?: () => void
}) {
  const list = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (focusAfterNavigation) {
      list.current?.querySelector<HTMLElement>('[tabindex="0"]')?.focus()
      focusAfterNavigation()
    }
  }, [focusAfterNavigation])
  const current = items.findIndex(item => item.path === selected?.path)
  function keyDown(event: KeyboardEvent<HTMLDivElement>, index: number, entry: InventoryEntry) {
    const target = ({ ArrowDown: index + 1, ArrowUp: index - 1, Home: 0, End: items.length - 1 })[event.key]
    if (target !== undefined) {
      event.preventDefault()
      const next = Math.min(items.length - 1, Math.max(0, target))
      select(items[next])
      event.currentTarget.parentElement?.querySelector<HTMLElement>(`[data-entry-index="${next}"]`)?.focus()
    }
    if (event.key === ' ' || event.key === 'Enter') {
      event.preventDefault()
      if (event.key === 'Enter' && entry.type === 'Directory') open(entry.path)
      else select(entry)
    }
  }
  return <div ref={list} className="browser-list" role="listbox" aria-label={search ? 'Search results' : 'Directory contents'}>
    {items.map((entry, index) => {
      const name = entry.path ? filename(entry.path) : 'Original file'
      const type = entry.type === 'Directory' ? 'Folder' : 'File'
      const size = entry.type === 'File' && entry.size != null ? fileSize(entry.size) : ''
      return <div className="browser-row" key={entry.path} role="option" aria-selected={entry.path === selected?.path}
        aria-label={[name, type, size, search ? entry.path : ''].filter(Boolean).join(' ')}
        data-entry-index={index} tabIndex={index === Math.max(current, 0) ? 0 : -1}
        onClick={event => { select(entry); event.currentTarget.focus() }} onDoubleClick={() => { if (entry.type === 'Directory') open(entry.path) }}
        onKeyDown={event => keyDown(event, index, entry)}>
        <span className="browser-name"><span aria-hidden="true">{entry.type === 'Directory' ? '📁' : '📄'}</span><span>{name}</span></span>
        <span className="browser-type">{type}</span>
        <span className="browser-size">{size}</span>
        {search && <small className="browser-result-path">{entry.path}</small>}
      </div>
    })}
  </div>
}

async function copyPath(path: string) {
  if (navigator.clipboard) {
    try { await navigator.clipboard.writeText(path); return true } catch { /* LAN HTTP or clipboard permission. */ }
  }
  const input = document.createElement('textarea')
  input.value = path
  input.className = 'browser-copy-buffer'
  input.setAttribute('readonly', '')
  const previous = document.activeElement
  const container = previous?.closest('dialog') || document.body
  container.append(input)
  input.select()
  try { return document.execCommand('copy') }
  catch { return false }
  finally { input.remove(); if (previous instanceof HTMLElement) previous.focus() }
}

function Inspector({ entry, artifactId, search, navigate, choose, mode, actions }: {
  entry: InventoryEntry; artifactId?: string; search: boolean; navigate: (path: string, reveal?: InventoryEntry) => void;
  choose?: (entry: InventoryEntry) => void; mode?: Choice; actions?: (entry: InventoryEntry) => ReactNode
}) {
  const [copied, setCopied] = useState<boolean>()
  const directory = entry.type === 'Directory'
  const canChoose = choose && (mode === 'any' || (directory ? mode === 'directory' : mode === 'file'))
  const download = artifactId && !directory && (!entry.path || entry.fileId)
    ? `/api/v1/artifacts/${enc(artifactId)}/${entry.fileId ? `files/${enc(entry.fileId)}` : 'content'}` : undefined
  return <aside className="browser-inspector" aria-label="Selected item">
    <h3>{entry.path || directory ? filename(entry.path) : 'Original file'}</h3>
    <p>{directory ? 'Folder' : entry.size == null ? 'File' : `File · ${fileSize(entry.size)}`}</p>
    {(entry.path || directory) && <p className="browser-relative-path"><code>{entry.path || '/'}</code></p>}
    <div className="form-actions">
      {directory && entry.path && <button type="button" className="button button-secondary" onClick={() => navigate(entry.path)}>Open folder</button>}
      {search && <button type="button" className="button button-secondary" onClick={() => navigate(parentOf(entry.path), entry)}>Show in folder</button>}
      {download && !choose && <a className="button button-secondary" href={download} download={entry.path ? filename(entry.path) : true}>{entry.path ? 'Download' : 'Download original file'}</a>}
      {entry.path && <button type="button" className="button button-secondary" onClick={() => void copyPath(entry.path).then(setCopied)}>Copy relative path</button>}
      {canChoose && <button type="button" className="button button-primary" onClick={() => choose(entry)}>{directory ? (entry.path ? 'Use this directory' : 'Use entire tree') : 'Use this file'}</button>}
      {actions?.(entry)}
    </div>
    {copied === true && <p role="status">Relative path copied.</p>}
    {copied === false && <label>Copy this relative path<input readOnly value={entry.path} onFocus={event => event.currentTarget.select()} /></label>}
    {(artifactId || entry.sha256 || entry.executable != null) && <TechnicalDetails>
      {artifactId && <p>Artifact ID <code>{artifactId}</code></p>}
      {!directory && entry.sha256 && <p>SHA-256 <code>{entry.sha256}</code></p>}
      {!directory && entry.executable != null && <p>Executable attribute {entry.executable ? 'Yes' : 'No'}</p>}
    </TechnicalDetails>}
  </aside>
}

type BrowserProps = {
  artifactId?: string; size?: number | null; position?: Position; onNavigate?: (position: Position) => void;
  choose?: (entry: InventoryEntry) => void; mode?: Choice; actions?: (entry: InventoryEntry) => ReactNode
}

export function ArtifactBrowser(props: BrowserProps & { artifactId: string }) {
  const { artifactId } = props
  if (artifactId.startsWith('adtree:')) return <TreeBrowser key={artifactId} {...props} source={artifactSource(artifactId)} />
  const entry: InventoryEntry = { type: 'File', path: '', size: props.size, sha256: artifactId.split(':').at(-1) }
  return <div className="artifact-browser">
    <BrowserList items={[entry]} selected={entry} search={false} select={() => {}} open={() => {}} />
    <Inspector entry={entry} search={false} navigate={() => {}} {...props} />
  </div>
}

function TreeBrowser({ source, artifactId, position, onNavigate, choose, mode, actions }: BrowserProps & { source: InventorySource }) {
  const [local, setLocal] = useState<Position>({ directory: '', search: '' })
  const { directory, search } = position || local
  const [draft, setDraft] = useState(search)
  const [searchError, setSearchError] = useState(false)
  const [focusContents, setFocusContents] = useState(false)
  const [order, setOrder] = useState<Order>('folders')
  const [selection, setSelection] = useState<{ position: string; entry: InventoryEntry; revealed?: InventoryEntry }>()
  const scope = JSON.stringify([directory, search])
  const currentSelection = selection?.position === scope ? selection : undefined
  const selected = currentSelection?.entry
  const page = useInventory(source, { directory: search ? '' : directory, search, order })
  useEffect(() => setDraft(search), [search])

  const changePosition = onNavigate || setLocal
  function navigate(path: string, reveal?: InventoryEntry) {
    changePosition({ directory: path, search: '' })
    setSelection(reveal ? { position: JSON.stringify([path, '']), entry: reveal, revealed: reveal } : undefined)
    setFocusContents(false)
  }
  function submitSearch(value: string) {
    if (new TextEncoder().encode(value).length > 256) {
      setSearchError(true)
      return
    }
    setSearchError(false)
    changePosition({ directory, search: value })
    setSelection(undefined)
  }
  const matches = page.value?.items || []
  const revealed = currentSelection?.revealed
  const pinned = revealed && !matches.some(entry => entry.path === revealed.path)
  const rows = pinned ? [revealed, ...matches] : matches
  const inspector = selected || (!search && choose && mode !== 'file' ? { type: 'Directory' as const, path: directory } : undefined)
  const [treeVisible, setTreeVisible] = useState(false)
  return <div className="artifact-browser">
    <div className="browser-toolbar" role="search" aria-label="Search Artifact">
      <label>Search original paths<input value={draft} maxLength={256} onChange={event => setDraft(event.target.value)}
        onKeyDown={event => { if (event.key === 'Enter') { event.preventDefault(); submitSearch(draft.trim()) } }} /></label>
      <button type="button" className="button button-secondary" onClick={() => submitSearch(draft.trim())}>Search</button>
      {search && <button type="button" className="button button-secondary" onClick={() => { setDraft(''); submitSearch('') }}>Clear search</button>}
    </div>
    {searchError && <p role="alert">Shorten the search to 256 UTF-8 bytes or fewer.</p>}
    <Breadcrumbs directory={directory} navigate={navigate} />
    <button type="button" className="button button-secondary browser-mobile-tree" aria-expanded={treeVisible} onClick={() => setTreeVisible(!treeVisible)}>
      {treeVisible ? 'Hide folders' : 'Show folders'}
    </button>
    <div className="browser-panes">
      <div className={`browser-tree-pane${treeVisible ? ' browser-tree-visible' : ''}`}><DirectoryTree source={source} current={directory} navigate={path => { navigate(path); setTreeVisible(false) }} /></div>
      <div className="browser-contents">
        <div className="browser-heading"><strong>{search ? `Results for “${search}”` : filename(directory)}</strong>
          <label>Sort by<select value={order} onChange={event => setOrder(event.target.value as Order)}><option value="folders">{search ? 'Path' : 'Name'}</option><option value="size">Largest first</option></select></label>
        </div>
        {page.loading ? <LoadingState label={search ? 'Searching Artifact…' : 'Loading directory…'} /> : page.error ? <ProblemNotice error={page.error} retry={page.retry} /> :
          !rows.length ? <EmptyState title={search ? 'No matching paths' : 'Empty directory'} detail={search ? 'Try another search.' : 'This folder has no entries.'} /> : <>
            {pinned && <p className="subtle">Search match shown above this page.</p>}
            <BrowserList items={rows} search={!!search} selected={selected} select={entry => setSelection({ position: scope, entry, revealed })}
              open={path => { navigate(path); setFocusContents(true) }}
              focusAfterNavigation={focusContents ? () => setFocusContents(false) : undefined} />
          </>}
        <Pages page={page} label={search ? 'Search result pages' : 'Directory pages'} />
        {!page.loading && !page.error && inspector && <Inspector key={inspector.path} entry={inspector} artifactId={artifactId} search={!!search} {...{ navigate, choose, mode, actions }} />}
      </div>
    </div>
  </div>
}

export function ArtifactPicker({ artifactId, choose, mode = 'any', label = 'Browse source tree' }: {
  artifactId: string; choose: (path: string, file: boolean) => void; mode?: Choice; label?: string
}) {
  const [open, setOpen] = useState(false)
  return <details onToggle={event => setOpen(event.currentTarget.open)}><summary>{label}</summary>
    {open && <ArtifactBrowser artifactId={artifactId} mode={mode} choose={entry => choose(entry.path, entry.type === 'File')} />}
  </details>
}

export function ResolvedTreePicker({ entries, identity, choose, mode = 'any' }: {
  entries: InventoryEntry[]; identity: string; choose?: (path: string, file: boolean) => void; mode?: Choice
}) {
  return <TreeBrowser mode={mode} choose={choose ? entry => choose(entry.path, entry.type === 'File') : undefined} source={{ key: identity, load: async query => {
    const matches = entries.filter(entry => (query.search ? entry.path.includes(query.search) : parentOf(entry.path) === query.directory)
      && (!query.directoriesOnly || entry.type === 'Directory'))
    matches.sort((a, b) => (a.type === b.type ? 0 : a.type === 'Directory' ? -1 : 1)
      || (query.order === 'size' ? (b.size || 0) - (a.size || 0) : 0) || (a.path < b.path ? -1 : a.path > b.path ? 1 : 0))
    const offset = Number(query.cursor || 0)
    return { items: matches.slice(offset, offset + pageSize), nextCursor: offset + pageSize < matches.length ? String(offset + pageSize) : null }
  } }} />
}
