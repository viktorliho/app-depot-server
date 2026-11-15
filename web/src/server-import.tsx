import { useEffect, useMemo, useRef, useState, type FormEvent } from 'react'
import { useOutletContext, useParams } from 'react-router'
import { apiRequest, idempotencyKey, resource, type DepotApp, type FinalizationResult, type Release } from './api'
import { ActionLink, LoadingState, PageHeader, ProblemNotice } from './components'
import { useAuth } from './portal'
import { useResource } from './use-resource'

type Root = { id: string; displayName: string; kind: 'Inbox' | 'Configured' }
type Entry = { name: string; entryType: 'File' | 'Directory'; size: number | null }
type Page = { items: Entry[]; nextCursor: string | null }
type Progress = { phase: string; sessionId?: string; filesTotal?: number; filesProcessed?: number;
  bytesTotal?: number; bytesProcessed?: number; result?: FinalizationResult; problem?: { code: string; title: string } }
const enc = encodeURIComponent
function bytes(value: number | null) {
  if (value === null) return 'Directory'
  return value >= 1048576 ? `${(value / 1048576).toFixed(1)} MiB` : value >= 1024 ? `${(value / 1024).toFixed(1)} KiB` : `${value} B`
}
function storedKey(fingerprint: string) {
  try {
    const saved = JSON.parse(sessionStorage.getItem('app-depot-server-import') || 'null') as { fingerprint: string; key: string } | null
    if (saved?.fingerprint === fingerprint) return saved.key
  } catch { /* A new key is safe when browser storage is unavailable. */ }
  const key = idempotencyKey()
  try { sessionStorage.setItem('app-depot-server-import', JSON.stringify({ fingerprint, key })) } catch { /* In-page retry still works. */ }
  return key
}

export function ServerImportPage({ target }: { target: 'new-app' | 'app' | 'release' }) {
  const app = useOutletContext<DepotApp | undefined>()
  const { releaseId } = useParams()
  const { state } = useAuth()
  const user = state.session?.user
  const canCreate = !!user && (user.serverAdmin || user.publishPermission)
  const canManage = !!user && (target === 'new-app' ? canCreate : !!app && (user.serverAdmin || user.id === app.ownerId))
  const release = useResource(`import-release:${releaseId || ''}`, signal => target === 'release'
    ? resource<Release>(`/releases/${releaseId}`, signal).then(result => result.value) : Promise.resolve(undefined))
  const roots = useResource('import-roots', signal => apiRequest<{ items: Root[] }>('/server-imports/roots', { authenticated: true, signal }).then(result => result.value))
  const [rootId, setRootId] = useState('')
  const [path, setPath] = useState('')
  const [cursor, setCursor] = useState('')
  const [previousCursors, setPreviousCursors] = useState<string[]>([])
  const [page, setPage] = useState<Page | null>(null)
  const [loading, setLoading] = useState(false)
  const [browseError, setBrowseError] = useState<unknown>()
  const [selected, setSelected] = useState<{ path: string; type: Entry['entryType']; size: number | null } | null>(null)
  const [interpretation, setInterpretation] = useState<'BareFile' | 'ZipImport'>('BareFile')
  const [name, setName] = useState('')
  const [progress, setProgress] = useState<Progress | null>(null)
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState('')
  const [result, setResult] = useState<FinalizationResult | null>(null)
  const [published, setPublished] = useState(false)
  const [cancelOperationId, setCancelOperationId] = useState<string | null>(null)
  const cancellationRequested = useRef(false)
  const controller = useRef<AbortController | null>(null)
  const keyRef = useRef<{ fingerprint: string; key: string } | null>(null)
  const availableRoots = roots.value?.items || []
  useEffect(() => {
    if (availableRoots.length && !availableRoots.some(root => root.id === rootId)) setRootId(availableRoots[0].id)
  }, [availableRoots, rootId])
  useEffect(() => {
    if (!rootId) return
    const signal = new AbortController()
    setLoading(true); setBrowseError(undefined); setPage(null)
    apiRequest<Page>(`/server-imports/roots/${enc(rootId)}/entries?path=${enc(path)}&pageSize=100${cursor ? `&cursor=${enc(cursor)}` : ''}`,
      { authenticated: true, signal: signal.signal })
      .then(response => setPage(response.value))
      .catch(failure => { if (!signal.signal.aborted) setBrowseError(failure) })
      .finally(() => { if (!signal.signal.aborted) setLoading(false) })
    return () => signal.abort()
  }, [rootId, path, cursor])
  useEffect(() => () => controller.current?.abort(), [])
  const targetValue = useMemo(() => target === 'release' ? { releaseId } : target === 'app'
    ? { appId: app?.id, createRelease: true } : { newAppName: name.trim() }, [target, releaseId, app?.id, name])
  const uploadRoute = target === 'release' ? `/apps/${app?.id}/releases/${releaseId}/uploads/new`
    : target === 'app' ? `/apps/${app?.id}/uploads/new` : '/upload/new-app'
  function navigateDirectory(next: string) { setPath(next); setCursor(''); setPreviousCursors([]); setSelected(null) }
  function choose(entry: Entry) {
    const chosen = path ? `${path}/${entry.name}` : entry.name
    if (entry.entryType === 'Directory') navigateDirectory(chosen)
    else { setSelected({ path: chosen, type: 'File', size: entry.size }); setInterpretation('BareFile') }
  }
  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!selected || !rootId || !user || !canManage) return
    const publish = (event.nativeEvent as SubmitEvent).submitter instanceof HTMLButtonElement &&
      (event.nativeEvent as SubmitEvent).submitter?.getAttribute('value') === 'publish'
    const mode = selected.type === 'Directory' ? 'BareTree' : interpretation
    const request = { rootId, path: selected.path, mode, target: targetValue }
    const fingerprint = JSON.stringify(request)
    const key = keyRef.current?.fingerprint === fingerprint ? keyRef.current.key : storedKey(fingerprint)
    keyRef.current = { fingerprint, key }
    const abort = new AbortController()
    controller.current = abort
    setBusy(true); setError(undefined); setResult(null); setMessage('')
    setPublished(false)
    cancellationRequested.current = false
    setCancelOperationId(null)
    setProgress({ phase: 'Scanning' })
    let newAttemptRequired = false
    let registered: FinalizationResult | null = null
    try {
      const response = await fetch('/api/v1/server-imports', { method: 'POST', credentials: 'same-origin', signal: abort.signal,
        headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': state.session!.csrfToken, 'Idempotency-Key': key }, body: JSON.stringify(request) })
      if (!response.ok) {
        const problem = await response.json().catch(() => ({})) as { title?: string; detail?: string }
        throw new Error(problem.detail || problem.title || `Import request failed (${response.status})`)
      }
      if (!response.body) throw new Error('The Server did not provide import progress.')
      const reader = response.body.getReader()
      const decoder = new TextDecoder()
      let buffered = ''
      let completed: FinalizationResult | null = null
      for (;;) {
        const next = await reader.read()
        if (next.done) break
        buffered += decoder.decode(next.value, { stream: true })
        let boundary: number
        while ((boundary = buffered.indexOf('\n\n')) >= 0) {
          const frame = buffered.slice(0, boundary); buffered = buffered.slice(boundary + 2)
          const data = frame.split('\n').find(line => line.startsWith('data: '))?.slice(6)
          if (!data) continue
          const update = JSON.parse(data) as Progress
          setProgress(update)
          if (update.phase === 'Failed') {
            if (update.problem?.code === 'session_cancelled') setProgress({ ...update, phase: 'Cancelled' })
            newAttemptRequired = ['source_changed', 'hash_mismatch', 'idempotency_conflict', 'session_cancelled'].includes(update.problem?.code || '')
            throw new Error(update.problem?.title || 'Server import failed.')
          }
          if (update.result) completed = update.result
        }
      }
      if (!completed || completed.phase !== 'Completed' || !completed.releaseId) {
        throw new Error('Import did not complete. Retry with the same source to inspect the session.')
      }
      registered = completed
      setResult(completed)
      if (publish) {
        setProgress({ phase: 'Publishing', sessionId: completed.sessionId })
        const current = await resource<Release>(`/releases/${completed.releaseId}`)
        await apiRequest(`/releases/${completed.releaseId}/publish`, { method: 'POST', authenticated: true,
          csrf: state.session!.csrfToken, idempotencyKey: key + '-publish', ifMatch: current.etag })
        setPublished(true)
      }
      setResult(completed); setProgress({ phase: 'Completed', result: completed })
      setMessage(publish ? 'Imported and published.' : 'Imported into a Draft release.')
      try { sessionStorage.removeItem('app-depot-server-import') } catch { /* Browser storage is optional. */ }
      keyRef.current = null
    } catch (failure) {
      if (newAttemptRequired) {
        try { sessionStorage.removeItem('app-depot-server-import') } catch { /* Browser storage is optional. */ }
        keyRef.current = null
        setMessage('The previous attempt cannot be resumed. Stabilize the source, then start a new import.')
      }
      if (registered) setMessage('Artifact registered in a Draft release; publication was not completed.')
      if (failure instanceof DOMException && failure.name === 'AbortError') setMessage(current => current || 'Import stream interrupted. Retry the exact request to inspect its upload session.')
      else setError(failure)
      setProgress(current => current && (current.phase === 'Completed' || current.phase === 'Cancelled' || cancellationRequested.current
        ? { ...current, phase: cancellationRequested.current && current.phase !== 'Cancelled' ? 'Cancelling' : current.phase }
        : { ...current, phase: 'Failed' }))
    } finally { setBusy(false); controller.current = null }
  }
  async function cancel() {
    const requestKey = keyRef.current?.key
    if (!requestKey) return
    try {
      const response = await apiRequest<{ phase?: string; operationId?: string } & Partial<FinalizationResult>>(`/server-imports`, { method: 'DELETE', authenticated: true,
        csrf: state.session!.csrfToken, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ requestKey }) })
      if (response.value.phase === 'Completed') {
        setResult(response.value as FinalizationResult)
        setProgress({ phase: 'Completed', result: response.value as FinalizationResult })
        setMessage('Registration completed before cancellation. Inspect the result before another action.')
      } else {
        cancellationRequested.current = true
        setProgress(current => current && { ...current, phase: 'Cancelling' })
        setCancelOperationId(response.value.operationId || null)
        setMessage('Cancellation requested. Follow the operation to confirm completion before starting a new import.')
      }
      try { sessionStorage.removeItem('app-depot-server-import') } catch { /* Browser storage is optional. */ }
      keyRef.current = null
      controller.current?.abort()
    } catch (failure) { setError(failure) }
  }
  return <><PageHeader title="Import from server storage" description="Select an approved read-only source. App Depot copies and verifies its bytes into managed storage." />
    <div className="form-actions section-action"><ActionLink to={uploadRoute} secondary>Upload from this device</ActionLink></div>
    {!canManage ? <ProblemNotice error={new Error('You do not have permission to import to this target.')} />
      : target === 'release' && release.loading ? <LoadingState />
      : target === 'release' && release.error ? <ProblemNotice error={release.error} retry={release.retry} />
      : target === 'release' && (release.value?.state !== 'Draft' || release.value.appId !== app?.id) ? <ProblemNotice error={new Error('Select a Draft release in this application to import content.')} />
      : roots.loading ? <LoadingState label="Loading import roots…" />
      : roots.error ? <ProblemNotice error={roots.error} retry={roots.retry} />
      : !availableRoots.length ? <section className="panel"><p>No server import roots are available to your account. Ask a Server Admin to configure and permit a read-only root.</p></section>
      : <section className="panel form-panel">
        <form className="form-stack" onSubmit={submit}>
          {target === 'new-app' && <label>App name<input value={name} disabled={busy} onChange={event => setName(event.target.value)} required /></label>}
          <label>Import root<select value={rootId} disabled={busy} onChange={event => { setRootId(event.target.value); navigateDirectory('') }}>
            {availableRoots.map(root => <option key={root.id} value={root.id}>{root.displayName}</option>)}</select></label>
          <div className="form-actions">
            <button type="button" className="button button-secondary" disabled={busy || !path} onClick={() => navigateDirectory(path.split('/').slice(0, -1).join('/'))}>Up one directory</button>
            <span>Location: /{path || ''}</span>
            <button type="button" className="button button-secondary" disabled={busy} onClick={() => setSelected({ path, type: 'Directory', size: null })}>Select this directory</button>
          </div>
          {loading ? <LoadingState label="Reading source directory…" /> : browseError ? <ProblemNotice error={browseError} /> :
            <table><thead><tr><th scope="col">Name</th><th scope="col">Type</th><th scope="col">Size</th></tr></thead><tbody>
              {page?.items.map(entry => <tr key={entry.name}><td><button type="button" className="link-button" disabled={busy} onClick={() => choose(entry)}>{entry.name}</button></td><td>{entry.entryType}</td><td>{bytes(entry.size)}</td></tr>)}
            </tbody></table>}
          {!loading && !page?.items.length && <p>This directory is empty.</p>}
          {(previousCursors.length > 0 || page?.nextCursor) && <div className="form-actions">
            {previousCursors.length > 0 && <button type="button" className="button button-secondary" disabled={busy} onClick={() => {
              const previous = previousCursors.at(-1) || ''
              setPreviousCursors(current => current.slice(0, -1)); setCursor(previous)
            }}>Previous page</button>}
            {page?.nextCursor && <button type="button" className="button button-secondary" disabled={busy} onClick={() => {
              setPreviousCursors(current => [...current, cursor]); setCursor(page.nextCursor!)
            }}>Next page</button>}
          </div>}
          {selected && <p role="status">Selected {selected.type.toLowerCase()}: {selected.path || '/'} · {bytes(selected.size)}</p>}
          {selected?.type === 'File' && <label>Import as<select value={interpretation} disabled={busy} onChange={event => setInterpretation(event.target.value as 'BareFile' | 'ZipImport')}>
            <option value="BareFile">Original file Artifact (ZIP remains an opaque file)</option>
            <option value="ZipImport">ZIP contents as a Tree Artifact</option>
          </select></label>}
          <p className="subtle">The Server never modifies the source. Importing ZIP contents is an explicit choice. Registration creates a Draft release; publication is separate.</p>
          {rootId === 'inbox' && <p className="subtle">App Depot Inbox is a persistent landing area outside managed CAS. Place files there using the host filesystem or an operator-configured share. The import browser is read-only.</p>}
          <div className="form-actions"><button className="button button-secondary" value="draft" disabled={busy || !!result || !selected || (target === 'new-app' && !name.trim())}>Import and save Draft</button>
            {canCreate && <button className="button button-primary" value="publish" disabled={busy || !!result || !selected || (target === 'new-app' && !name.trim())}>Import and publish</button>}
            {busy && <button type="button" className="button button-danger" onClick={() => void cancel()}>Cancel import</button>}</div>
        </form>
        {progress && <section className="upload-status" aria-label="Import progress"><h2>{progress.phase === 'Receiving' ? 'Copying source bytes' : progress.phase === 'Importing' ? 'Extracting ZIP' : progress.phase === 'Committing' ? 'CAS registration' : progress.phase}</h2>
          {progress.phase === 'Completed' ? <p>Artifact registration completed. {published ? 'The Release is Published.' : 'The Release remains Draft until you publish it.'} The original source remains untouched; removing it later will not remove the registered Artifact. Deleting the App also leaves Inbox content unchanged.</p>
            : progress.phase === 'Failed' ? <p>{result ? 'Artifact registration completed, but publication failed. The Release remains Draft.' : 'Import failed. No incomplete content was registered as an Artifact.'}</p>
            : progress.phase === 'Cancelled' ? <p>Import cancelled. No incomplete content was registered as an Artifact.</p>
            : progress.phase === 'Cancelling' ? <p>Cancellation is in progress. Follow the cancellation operation for its final state.</p>
            : progress.phase === 'Receiving' && progress.filesTotal !== undefined ? <p>{progress.filesProcessed?.toLocaleString()} / {progress.filesTotal.toLocaleString()} files · {bytes(progress.bytesProcessed || 0)} / {bytes(progress.bytesTotal || 0)} acknowledged in staging</p>
            : <p>Server-side work is in progress; this phase has no reliable percentage.</p>}
          {!['Completed', 'Failed', 'Cancelled', 'Cancelling'].includes(progress.phase) && <progress aria-label="Import in progress" />}</section>}
        {message && <p role="status">{message}</p>}
        {error !== undefined && <ProblemNotice error={error} />}
        {cancelOperationId && <ActionLink to={`/operations/${cancelOperationId}`} secondary>View cancellation operation</ActionLink>}
        {result?.appId && <div className="form-actions section-action"><ActionLink to={`/apps/${result.appId}`} secondary>Open application</ActionLink><ActionLink to={`/apps/${result.appId}/releases/${result.releaseId}`} secondary>Open release</ActionLink></div>}
      </section>}
  </>
}
