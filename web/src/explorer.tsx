import { useEffect, useState } from 'react'
import { Link, useOutletContext, useParams, useSearchParams } from 'react-router'
import { apiRequest, idempotencyKey, jsonBody, resource, type Artifact, type Association, type Declaration, type DepotApp, type InventoryEntry, type Page, type Release } from './api'
import { EmptyState, LoadingState, PageHeader, ProblemNotice, TechnicalDetails } from './components'
import { useAuth } from './portal'
import { useResource } from './use-resource'

const enc = encodeURIComponent
const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === 'object' && !Array.isArray(value)
type Entry = Record<string, unknown>
type Assessment = { status: 'Absent' | 'Valid' | 'Issues' | 'Unsupported'; items: { index: number; id: string; status: string }[] }

function readEntries(data: unknown): Entry[] | null {
  if (!object(data)) return null
  if (data.appDepot === undefined) return []
  if (!object(data.appDepot)) return null
  if (data.appDepot.entryPoints === undefined) return []
  if (!Array.isArray(data.appDepot.entryPoints) || data.appDepot.entryPoints.some(item => !object(item))) return null
  return data.appDepot.entryPoints as Entry[]
}

function withEntries(data: unknown, entries: Entry[]): unknown {
  if (!object(data) || (data.appDepot !== undefined && !object(data.appDepot))) throw new Error('Use the raw declaration editor for this JSON value.')
  const block = { ...(object(data.appDepot) ? data.appDepot : {}) }
  if (entries.length) block.entryPoints = entries
  else delete block.entryPoints
  const result = { ...data }
  if (Object.keys(block).length) result.appDepot = block
  else delete result.appDepot
  return result
}

function targetLabel(entry: Entry) {
  const target = entry.target
  if (!object(target)) return 'Unrecognized target'
  if (target.type === 'ArtifactFile') return 'This single-file Artifact'
  if (target.type === 'TreeFile') return typeof target.path === 'string' ? target.path : 'Invalid path'
  return 'Unrecognized target'
}

export function ArtifactExplorer() {
  const app = useOutletContext<DepotApp>()
  const { releaseId, associationId } = useParams()
  const { state } = useAuth()
  const [params, setParams] = useSearchParams()
  const directory = params.get('dir')
  const search = params.get('q') || ''
  const [searchDraft, setSearchDraft] = useState(search)
  const [cursor, setCursor] = useState('')
  const [selected, setSelected] = useState<{ type: 'TreeFile'; path: string } | { type: 'ArtifactFile' } | null>(null)
  const [editingIndex, setEditingIndex] = useState<number | null>(null)
  const [revision, setRevision] = useState(0)
  const [entries, setEntries] = useState<Entry[]>([])
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const [notice, setNotice] = useState('')
  const release = useResource(`explorer-release:${releaseId}`, signal => apiRequest<Release>(`/releases/${enc(releaseId!)}`, { authenticated: true, signal }).then(result => result.value))
  const association = useResource(`explorer-association:${associationId}`, signal => apiRequest<Association>(`/associations/${enc(associationId!)}`, { authenticated: true, signal }).then(result => result.value))
  const artifactId = association.value?.artifactId
  const artifact = useResource<Artifact>(artifactId ? `explorer-artifact:${artifactId}` : 'waiting-artifact', signal => artifactId ? apiRequest<Artifact>(`/artifacts/${enc(artifactId)}`, { authenticated: true, signal }).then(result => result.value) : new Promise<Artifact>(() => {}))
  const declaration = useResource(`explorer-declaration:${associationId}:${revision}`, signal => resource<Declaration>(`/associations/${enc(associationId!)}/declarations`, signal))
  const inventoryPath = artifact.value?.kind === 'Tree' ? `/artifacts/${enc(artifactId!)}/inventory?pageSize=50${search ? `&q=${enc(search)}` : `&parent=${enc(directory || '')}`}${cursor ? `&cursor=${enc(cursor)}` : ''}` : null
  const inventory = useResource<Page<InventoryEntry>>(inventoryPath || 'waiting-inventory', signal => inventoryPath ? apiRequest<Page<InventoryEntry>>(inventoryPath, { authenticated: true, signal }).then(result => result.value) : new Promise<Page<InventoryEntry>>(() => {}))
  const validation = useResource<Assessment>(declaration.value ? `entry-assessment:${associationId}:${declaration.value.value.revisionId}` : 'waiting-assessment', signal => declaration.value ? apiRequest<Assessment>(`/associations/${enc(associationId!)}/entry-points/validate`, { method: 'POST', authenticated: true, signal, ...jsonBody({ data: declaration.value.value.data }) }).then(result => result.value) : new Promise<Assessment>(() => {}))
  useEffect(() => { setEntries(readEntries(declaration.value?.value.data) || []) }, [declaration.value?.value.revisionId])
  useEffect(() => { setCursor('') }, [directory, search])
  useEffect(() => { setSearchDraft(search) }, [search])
  const allowed = state.session!.user.serverAdmin || state.session!.user.id === app.ownerId
  const validContext = release.value?.appId === app.id && association.value?.releaseId === releaseId
  const structured = declaration.value ? readEntries(declaration.value.value.data) !== null : false

  function openDirectory(path: string | null) { setParams(path === null ? { dir: '' } : { dir: path }) }
  function changeEntry(index: number, patch: Entry) { setEntries(current => current.map((item, position) => position === index ? { ...item, ...patch } : item)) }
  function applySelection() {
    const target = selected || (artifact.value?.kind === 'File' ? { type: 'ArtifactFile' as const } : null)
    if (!target) return
    if (editingIndex === null) setEntries(current => [...current, { id: idempotencyKey(), target }])
    else changeEntry(editingIndex, { target })
    setSelected(null); setEditingIndex(null)
    document.getElementById('entry-points-heading')?.scrollIntoView({ block: 'start' })
  }
  async function save() {
    setBusy(true); setError(undefined); setNotice('')
    try {
      const data = withEntries(declaration.value!.value.data, entries)
      const check = await apiRequest<Assessment>(`/associations/${enc(associationId!)}/entry-points/validate`, { method: 'POST', authenticated: true, ...jsonBody({ data }) })
      if (check.value.status !== 'Valid' && check.value.status !== 'Absent') throw new Error(`Entry points need correction: ${check.value.items.filter(item => item.status !== 'Valid').map(item => `#${item.index + 1} ${item.status}`).join(', ') || check.value.status}.`)
      await apiRequest(`/associations/${enc(associationId!)}/declarations`, { method: 'PUT', authenticated: true, csrf: state.session!.csrfToken,
        ifMatch: declaration.value!.etag, ...jsonBody({ data }) })
      setNotice('Entry points saved as a new declaration revision.'); setRevision(value => value + 1)
    } catch (failure) { setError(failure) } finally { setBusy(false) }
  }

  if (release.loading || association.loading || artifact.loading || declaration.loading) return <LoadingState label="Loading Artifact…" />
  if (release.error || association.error || artifact.error || declaration.error) return <ProblemNotice error={release.error || association.error || artifact.error || declaration.error} retry={() => { release.retry(); association.retry(); artifact.retry(); declaration.retry() }} />
  if (!validContext) return <ProblemNotice error={new Error('This Artifact association does not belong to the selected Release and application.')} />
  const item = artifact.value!
  const crumbs = directory ? directory.split('/') : []
  return <><PageHeader title={item.kind === 'Tree' ? 'Artifact Explorer' : 'File Artifact'} description={`Original ${item.kind === 'Tree' ? 'directory tree' : 'file'} in ${release.value!.version || 'Unversioned release'}`} />
    <p><Link to={`/apps/${app.id}/releases/${releaseId}`}>← Back to Release</Link></p>
    <section className="panel"><h2>Original content</h2><p>Stored content is immutable. Selecting an entry point only updates the association declaration; the Server never executes files.</p>
      {item.kind === 'File' ? <><p>Single-file Artifact · {item.size?.toLocaleString() || 0} bytes</p>
        <a className="button button-secondary" href={`/api/v1/artifacts/${enc(item.id)}/content`} download>Download original file</a>
        {allowed && <button className="button button-secondary" onClick={() => setSelected({ type: 'ArtifactFile' })}>Select as entry point</button>}</> : <>
        <form className="form-actions" onSubmit={event => { event.preventDefault(); setParams(searchDraft ? { q: searchDraft } : { dir: directory || '' }) }}>
          <label>Search original paths<input value={searchDraft} maxLength={256} onChange={event => setSearchDraft(event.target.value)} /></label>
          <button className="button button-secondary">Search</button>{search && <button type="button" className="button button-secondary" onClick={() => { setSearchDraft(''); setParams({ dir: '' }) }}>Clear search</button>}
        </form>
        {!search && <nav aria-label="Artifact path" className="form-actions"><button className="text-button" onClick={() => openDirectory(null)}>Root</button>{crumbs.map((part, index) => <span key={index}> / <button className="text-button" onClick={() => openDirectory(crumbs.slice(0, index + 1).join('/'))}>{part}</button></span>)}</nav>}
        {inventory.loading ? <LoadingState label="Loading directory…" /> : inventory.error ? <ProblemNotice error={inventory.error} retry={inventory.retry} /> : <>
          {!inventory.value!.items.length && <EmptyState title={search ? 'No matching paths' : 'Empty directory'} detail={search ? 'Try another original-path search.' : 'This directory contains no files or folders.'} />}
          <div className="table-wrap"><table><thead><tr><th>Name</th><th>Size</th><th>Portable executable</th><th>Actions</th></tr></thead><tbody>
            {inventory.value!.items.map(entry => <tr key={entry.path}><td>{entry.type === 'Directory' ? <button className="text-button" onClick={() => openDirectory(entry.path)}>📁 {search ? entry.path : entry.path.split('/').pop()}</button> : <span>📄 {search ? entry.path : entry.path.split('/').pop()}</span>}</td>
              <td>{entry.type === 'File' ? `${entry.size?.toLocaleString() || 0} bytes` : '—'}</td><td>{entry.type === 'File' ? entry.executable ? 'Yes' : 'No' : '—'}</td>
              <td>{entry.type === 'File' && entry.fileId && <><a href={`/api/v1/artifacts/${enc(item.id)}/files/${enc(entry.fileId)}`} download={entry.path.split('/').pop()}>Download</a>{allowed && <button className="text-button" onClick={() => setSelected({ type: 'TreeFile', path: entry.path })}>Add entry point</button>}</>}
                <TechnicalDetails><code>{entry.path}</code>{entry.sha256 && <p>SHA-256: <code>{entry.sha256}</code></p>}</TechnicalDetails></td></tr>)}</tbody></table></div>
          {inventory.value!.nextCursor && <button className="button button-secondary" onClick={() => setCursor(inventory.value!.nextCursor!)}>Next page</button>}
        </>}
      </>}
      <TechnicalDetails><p>Artifact ID: <code>{item.id}</code></p><p>Association ID: <code>{associationId}</code></p></TechnicalDetails>
    </section>
    <section className="panel"><h2 id="entry-points-heading">Entry points</h2><p>Optional client hints. A valid path does not guarantee that a client can execute the file. Platform constraints narrow applicability; leave blank for any platform.</p>
      {validation.loading ? <LoadingState label="Checking saved references…" /> : validation.error ? <ProblemNotice error={validation.error} retry={validation.retry} /> : validation.value && <p role="status">Saved references: {validation.value.status}{validation.value.items.some(value => value.status !== 'Valid') && ` (${validation.value.items.filter(value => value.status !== 'Valid').map(value => `#${value.index + 1} ${value.status}`).join(', ')})`}</p>}
      {!structured && <p role="alert">This declaration does not use an editable entry-point structure. Its JSON is preserved. Use the raw declaration editor on the Release page.</p>}
        {selected && allowed && structured && <div className="form-actions"><p>Selected: <code>{selected.type === 'TreeFile' ? selected.path : 'This single-file Artifact'}</code></p><button className="button button-primary" onClick={applySelection}>{editingIndex === null ? 'Add entry point' : 'Use selected target'}</button><button className="button button-secondary" onClick={() => { setSelected(null); setEditingIndex(null) }}>Cancel selection</button></div>}
      {editingIndex !== null && !selected && <p role="status">Choose a file above to change entry point {editingIndex + 1}.</p>}
      {structured && !entries.length && <EmptyState title="No entry points" detail="Choose a file above, or leave this Artifact without entry points." />}
      {structured && entries.map((entry, index) => <div className="form-panel" key={String(entry.id || index)}><h3>Entry point {index + 1}</h3><p>Target: <code>{targetLabel(entry)}</code></p>
        <label>Label (optional)<input value={typeof entry.label === 'string' ? entry.label : ''} readOnly={!allowed} onChange={event => changeEntry(index, { label: event.target.value || undefined })} /></label>
        <label>Target operating systems and CPU architectures (one per line; optional)<textarea rows={3} value={Array.isArray(entry.platforms) ? entry.platforms.join('\n') : ''} readOnly={!allowed} placeholder={'linux/x86_64\nwindows/x86_64'} onChange={event => changeEntry(index, { platforms: event.target.value.split('\n').map(s => s.trim()).filter(Boolean) })} /></label>
        <TechnicalDetails><code>Entry ID: {String(entry.id)}</code></TechnicalDetails>
        {allowed && <div className="form-actions"><button className="button button-secondary" onClick={() => { setEditingIndex(index); setSelected(null); document.querySelector('[aria-label="Artifact path"]')?.scrollIntoView({ block: 'start' }) }}>Change target using Explorer</button><button className="button button-danger" onClick={() => setEntries(current => current.filter((_, position) => position !== index))}>Remove entry point</button></div>}
      </div>)}
      {error !== undefined && <ProblemNotice error={error} />}{notice && <p role="status" className="success-notice">{notice}</p>}
      {allowed && structured && <button className="button button-primary" disabled={busy} onClick={() => void save()}>{busy ? 'Saving…' : 'Save entry points'}</button>}
    </section>
  </>
}
