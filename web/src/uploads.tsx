import { useRef, useState, type FormEvent } from 'react'
import { useOutletContext, useParams } from 'react-router'
import { apiRequest, fileSha256, idempotencyKey, jsonBody, resource, type DepotApp, type FinalizationResult,
  type InventoryEntry, type Operation, type Page, type Release, type UploadProgress, type UploadSession } from './api'
import { ActionLink, LoadingState, PageHeader, ProblemNotice } from './components'
import { OperationNotice } from './management'
import { useAuth } from './portal'
import { useResource } from './use-resource'

type Mode = 'BareFile' | 'BareTree' | 'ZipImport'
type Capabilities = { ingestionModes: Mode[]; limits: { maxPageSize: number; maxUploadBytes: number } }
type PreparedFile = { file: File; path: string; hash: string }
type Keys = { fingerprint: string; create: string; complete: string; finalize: string; publish: string; inventory: string[]; cancel: string }
const enc = encodeURIComponent
const chunkSize = 4 * 1024 * 1024
const textEncoder = new TextEncoder()

function pathOrder(left: string, right: string) {
  const a = textEncoder.encode(left)
  const b = textEncoder.encode(right)
  for (let index = 0; index < Math.min(a.length, b.length); index++) if (a[index] !== b[index]) return a[index] - b[index]
  return a.length - b.length
}

function readKeys(key: string): Keys | null {
  try { const value = sessionStorage.getItem(key); return value ? JSON.parse(value) as Keys : null }
  catch { return null }
}
function saveKeys(key: string, value: Keys) { try { sessionStorage.setItem(key, JSON.stringify(value)) } catch { /* In-tab retries remain available. */ } }
function forgetKeys(key: string) { try { sessionStorage.removeItem(key) } catch { /* Storage is unavailable. */ } }

function rangesCover(ranges: { start: number; end: number }[], size: number) {
  let offset = 0
  for (const range of ranges) { if (range.start > offset) return false; offset = Math.max(offset, range.end) }
  return offset >= size
}

async function progress(sessionId: string, fileId: string, signal: AbortSignal, pageSize: number) {
  let cursor = ''
  let version: number | undefined
  const ranges: { start: number; end: number }[] = []
  do {
    const result = (await apiRequest<UploadProgress>(`/uploads/${sessionId}/files/${fileId}/progress?pageSize=${pageSize}${cursor ? `&cursor=${enc(cursor)}` : ''}`, { authenticated: true, signal })).value
    if (version !== undefined && result.progressVersion !== version) throw new Error('Upload progress changed during inspection. Retry the transfer.')
    version = result.progressVersion
    ranges.push(...result.ranges)
    cursor = result.nextCursor || ''
  } while (cursor)
  return ranges.sort((a, b) => a.start - b.start)
}

async function transfer(sessionId: string, fileId: string, file: File, csrf: string, signal: AbortSignal, pageSize: number, report: (value: string) => void) {
  if (file.size === 0) return
  let ranges = await progress(sessionId, fileId, signal, pageSize)
  if (rangesCover(ranges, file.size)) return
  let offset = 0
  for (const range of [...ranges, { start: file.size, end: file.size }]) {
    while (offset < range.start) {
      const end = Math.min(range.start, offset + chunkSize)
      await apiRequest<void>(`/uploads/${sessionId}/files/${fileId}`, { method: 'PUT', authenticated: true, csrf, signal,
        headers: { 'Content-Type': 'application/octet-stream', 'Content-Range': `bytes ${offset}-${end - 1}/${file.size}` }, body: file.slice(offset, end) })
      report(`Server acknowledged ${end.toLocaleString()} of ${file.size.toLocaleString()} bytes for ${file.name}`)
      offset = end
    }
    offset = Math.max(offset, range.end)
  }
  ranges = await progress(sessionId, fileId, signal, pageSize)
  if (!rangesCover(ranges, file.size)) throw new Error('The Server has not acknowledged the complete file. Retry the transfer.')
}

function selectedPaths(files: File[]) {
  return files.map(file => ({ file, path: file.webkitRelativePath || file.name })).sort((a, b) => pathOrder(a.path, b.path))
}

function treeEntries(files: PreparedFile[], extraDirectories: string): { path: string; type: 'File' | 'Directory'; size?: number; sha256?: string; executable?: boolean }[] {
  const directories = new Set<string>()
  for (const file of files) {
    const parts = file.path.split('/')
    for (let length = 1; length < parts.length; length++) directories.add(parts.slice(0, length).join('/'))
  }
  for (const raw of extraDirectories.split('\n')) {
    const path = raw.trim().replace(/\/$/, '')
    if (!path) continue
    const parts = path.split('/')
    for (let length = 1; length <= parts.length; length++) directories.add(parts.slice(0, length).join('/'))
  }
  const entries = [
    ...[...directories].map(path => ({ path, type: 'Directory' as const })),
    ...files.map(({ file, path, hash }) => ({ path, type: 'File' as const, size: file.size, sha256: hash, executable: false }))
  ]
  if (new Set(entries.map(entry => entry.path)).size !== entries.length) throw new Error('File and directory paths must be unique.')
  if (entries.some(entry => !entry.path || entry.path.startsWith('/') || entry.path.split('/').some(part => !part || part === '.' || part === '..' || part.includes('\\')))) {
    throw new Error('Directory paths must be relative and contain no empty, dot, or parent segments.')
  }
  return entries.sort((a, b) => pathOrder(a.path, b.path))
}

async function inventoryFiles(sessionId: string, signal: AbortSignal, pageSize: number) {
  const map = new Map<string, InventoryEntry>()
  let cursor = ''
  do {
    const result = (await apiRequest<Page<InventoryEntry>>(`/uploads/${sessionId}/inventory?pageSize=${pageSize}${cursor ? `&cursor=${enc(cursor)}` : ''}`, { authenticated: true, signal })).value
    for (const entry of result.items) map.set(entry.path, entry)
    cursor = result.nextCursor || ''
  } while (cursor)
  return map
}

export function UploadPage({ target }: { target: 'new-app' | 'app' | 'release' }) {
  const app = useOutletContext<DepotApp | undefined>()
  const { releaseId } = useParams()
  const { state } = useAuth()
  const capabilities = useResource('upload-capabilities', signal => apiRequest<Capabilities>('/capabilities', { signal }).then(result => result.value))
  const release = useResource(`upload-release:${releaseId || ''}`, signal => target === 'release'
    ? resource<Release>(`/releases/${releaseId}`, signal).then(result => result.value) : Promise.resolve(undefined))
  const [mode, setMode] = useState<Mode>('BareFile')
  const [name, setName] = useState('')
  const [single, setSingle] = useState<File | null>(null)
  const [directoryFiles, setDirectoryFiles] = useState<File[]>([])
  const [extraDirectories, setExtraDirectories] = useState('')
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState('')
  const [error, setError] = useState<unknown>()
  const [result, setResult] = useState<FinalizationResult>()
  const [activeSession, setActiveSession] = useState('')
  const [cancelOperation, setCancelOperation] = useState('')
  const controller = useRef<AbortController | null>(null)
  const keys = useRef<Keys | null>(null)
  const canCreate = state.session!.user.serverAdmin || state.session!.user.publishPermission
  const canManage = !app || state.session!.user.serverAdmin || state.session!.user.id === app.ownerId
  const canPublish = canCreate
  const storageKey = `app-depot-upload:${target}:${app?.id || 'new'}:${releaseId || ''}`
  const validInput = mode === 'BareTree' ? directoryFiles.length > 0 : !!single
  function reset() { setResult(undefined); setError(undefined); setMessage(''); setActiveSession(''); setCancelOperation('') }
  function startNew() { keys.current = null; forgetKeys(storageKey); reset() }
  async function cancel() {
    controller.current?.abort()
    if (!activeSession) { setMessage('Stopped before an upload session was created.'); return }
    try {
      const result = await apiRequest<Operation | FinalizationResult>(`/uploads/${activeSession}`, { method: 'DELETE', authenticated: true,
        csrf: state.session!.csrfToken, idempotencyKey: keys.current?.cancel || idempotencyKey() })
      if ('phase' in result.value && result.value.phase === 'Completed') {
        setResult(result.value); setMessage('Registration completed before cancellation. Check the Release.')
      } else if ('id' in result.value) {
        setCancelOperation(result.value.id); setMessage('Cancellation requested. The operation status below is authoritative.')
      }
    } catch (failure) { setError(failure) }
  }
  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!validInput || (target === 'new-app' && !name.trim())) return
    const publishNow = ((event.nativeEvent as SubmitEvent).submitter as HTMLButtonElement | null)?.value === 'publish'
    setBusy(true); setError(undefined); setResult(undefined)
    const abort = new AbortController(); controller.current = abort
    const signal = abort.signal
    try {
      if (!capabilities.value) throw new Error('Server upload capabilities are unavailable.')
      if (!capabilities.value.ingestionModes.includes(mode)) throw new Error('The Server does not support the selected upload mode.')
      setMessage('Hashing selected files')
      const files = mode === 'BareTree' ? selectedPaths(directoryFiles) : selectedPaths([single!])
      if (files.reduce((total, item) => total + item.file.size, 0) > capabilities.value.limits.maxUploadBytes) throw new Error('Selected upload exceeds the Server byte limit.')
      const pageSize = Math.min(100, capabilities.value.limits.maxPageSize)
      const prepared: PreparedFile[] = []
      for (const item of files) {
        if (signal.aborted) throw new DOMException('Cancelled', 'AbortError')
        prepared.push({ ...item, hash: await fileSha256(item.file) })
      }
      const entries = mode === 'BareTree' ? treeEntries(prepared, extraDirectories) : []
      const targetValue = target === 'release' ? { releaseId } : target === 'app' ? { appId: app!.id, createRelease: true } : { newAppName: name.trim() }
      const fingerprint = JSON.stringify({ targetValue, mode, files: prepared.map(item => [item.path, item.file.size, item.hash]), entries })
      const previous = readKeys(storageKey)
      if (!keys.current || keys.current.fingerprint !== fingerprint) keys.current = previous?.fingerprint === fingerprint ? previous : {
        fingerprint, create: idempotencyKey(), complete: idempotencyKey(), finalize: idempotencyKey(), publish: idempotencyKey(),
        inventory: [], cancel: idempotencyKey()
      }
      const activeKeys = keys.current
      saveKeys(storageKey, activeKeys)
      setMessage('Creating or resuming upload session')
      const createBody = mode === 'BareTree' ? { mode, target: targetValue } : { mode, target: targetValue, file: { size: single!.size, sha256: prepared[0].hash } }
      const session = (await apiRequest<UploadSession>('/uploads', { method: 'POST', authenticated: true, signal,
        csrf: state.session!.csrfToken, idempotencyKey: activeKeys.create, ...jsonBody(createBody) })).value
      setActiveSession(session.id)
      if (session.phase === 'Cancelled' || session.phase === 'Failed') throw new Error(`Upload session is ${session.phase}. Select a changed file or target to start a new upload.`)
      let final: FinalizationResult
      if (session.phase === 'Receiving' && mode === 'BareTree') {
        if (!session.inventoryComplete) {
          for (let index = 0; index < entries.length; index += 100) {
            const batch = index / 100
            if (!activeKeys.inventory[batch]) { activeKeys.inventory[batch] = idempotencyKey(); saveKeys(storageKey, activeKeys) }
            setMessage(`Registering directory inventory ${Math.min(index + 100, entries.length)} of ${entries.length}`)
            await apiRequest(`/uploads/${session.id}/inventory`, { method: 'POST', authenticated: true, signal,
              csrf: state.session!.csrfToken, idempotencyKey: activeKeys.inventory[batch], ...jsonBody({ entries: entries.slice(index, index + 100) }) })
          }
          await apiRequest(`/uploads/${session.id}/inventory/complete`, { method: 'POST', authenticated: true, signal,
            csrf: state.session!.csrfToken, idempotencyKey: activeKeys.complete })
        }
        const assigned = await inventoryFiles(session.id, signal, pageSize)
        for (const item of prepared) {
          const registered = assigned.get(item.path)
          if (!registered?.fileId || registered.sha256 !== item.hash) throw new Error(`Inventory mismatch for ${item.path}`)
          if (registered.transferRequired !== false) await transfer(session.id, registered.fileId, item.file, state.session!.csrfToken, signal, pageSize, setMessage)
        }
      } else if (session.phase === 'Receiving') {
        if (!session.file?.fileId) throw new Error('Server did not assign an upload file ID.')
        await transfer(session.id, session.file.fileId, single!, state.session!.csrfToken, signal, pageSize, setMessage)
      }
      setMessage('Finalizing registration')
      final = session.phase === 'Receiving'
        ? (await apiRequest<FinalizationResult>(`/uploads/${session.id}/finalize`, { method: 'POST', authenticated: true, signal,
          csrf: state.session!.csrfToken, idempotencyKey: activeKeys.finalize })).value
        : (await apiRequest<FinalizationResult>(`/uploads/${session.id}/result`, { authenticated: true, signal })).value
      for (let attempt = 0; final.phase !== 'Completed' && final.phase !== 'Failed' && final.phase !== 'Cancelled' && attempt < 600; attempt++) {
        await new Promise(resolve => setTimeout(resolve, 1000))
        if (signal.aborted) throw new DOMException('Cancelled', 'AbortError')
        final = (await apiRequest<FinalizationResult>(`/uploads/${session.id}/result`, { authenticated: true, signal })).value
      }
      if (final.phase !== 'Completed' || !final.releaseId) throw new Error(final.problem?.title || 'Registration did not complete. Check session status before retrying.')
      if (publishNow) {
        setMessage('Publishing release')
        const current = await resource<Release>(`/releases/${final.releaseId}`, signal)
        await apiRequest(`/releases/${final.releaseId}/publish`, { method: 'POST', authenticated: true, signal,
          csrf: state.session!.csrfToken, idempotencyKey: activeKeys.publish, ifMatch: current.etag })
      }
      setResult(final)
      setMessage(`${publishNow ? 'Published' : 'Saved Draft for'} ${app?.name || name.trim()}`)
      forgetKeys(storageKey)
    } catch (failure) {
      if (failure instanceof DOMException && failure.name === 'AbortError') setMessage('Transfer stopped. The Server may still have acknowledged ranges; retry with the same files to inspect progress.')
      else setError(failure)
    } finally { setBusy(false); controller.current = null }
  }
  const heading = target === 'release' ? 'Upload Artifact' : target === 'app' ? 'Add release from file' : 'Upload a new application'
  return <><PageHeader title={heading} description={target === 'release' ? `Register an Artifact in the selected Draft release of ${app?.name}.` : target === 'app' ? `Create a Draft release in ${app?.name} from an Artifact.` : 'Create an application and Draft release from an Artifact.'} />
    {target === 'release' && release.loading ? <LoadingState /> : target === 'release' && release.error ? <ProblemNotice error={release.error} retry={release.retry} /> : target === 'release' && (release.value?.state !== 'Draft' || release.value.appId !== app?.id) ? <ProblemNotice error={new Error('The selected Draft release is unavailable in this application.')} /> : capabilities.loading ? <LoadingState label="Checking upload capabilities…" /> : capabilities.error ? <ProblemNotice error={capabilities.error} retry={capabilities.retry} /> :
      !canManage || (target === 'new-app' && !canCreate) ? <ProblemNotice error={new Error('You do not have permission to upload to this target.')} /> : <section className="panel form-panel">
        <p>Choose one ingestion mode. Registration creates or updates a Draft release; publication is a separate explicit action.</p>
        <form className="form-stack" onSubmit={submit}>
          {target === 'new-app' && <label>App name<input value={name} disabled={busy} onChange={event => { setName(event.target.value); reset() }} required /></label>}
          <label>Upload mode<select value={mode} disabled={busy} onChange={event => { setMode(event.target.value as Mode); reset() }}><option value="BareFile" disabled={!capabilities.value?.ingestionModes.includes('BareFile')}>Single file</option><option value="BareTree" disabled={!capabilities.value?.ingestionModes.includes('BareTree')}>Directory tree</option><option value="ZipImport" disabled={!capabilities.value?.ingestionModes.includes('ZipImport')}>ZIP import</option></select></label>
          {mode === 'BareTree' ? <><label>Directory files<input type="file" multiple disabled={busy} {...{ webkitdirectory: '' }} onChange={event => { setDirectoryFiles(Array.from(event.target.files || [])); reset() }} /></label>
            <label>Empty directory paths (optional, one relative path per line)<textarea rows={3} value={extraDirectories} disabled={busy} onChange={event => { setExtraDirectories(event.target.value); reset() }} /></label>
            <p className="subtle">Browsers may omit empty folders and do not expose executable flags. Add empty paths above; uploaded files are marked non-executable. A tree needs at least one regular file.</p></> :
            <label>File<input type="file" disabled={busy} accept={mode === 'ZipImport' ? '.zip,application/zip' : undefined} onChange={event => { setSingle(event.target.files?.[0] || null); reset() }} required /></label>}
          <div className="form-actions"><button className="button button-secondary" value="draft" disabled={busy || !!result || !validInput || (target === 'new-app' && !name.trim())}>Save Draft</button>
            {canPublish && <button className="button button-primary" value="publish" disabled={busy || !!result || !validInput || (target === 'new-app' && !name.trim())}>Upload and publish</button>}
            {busy && <button type="button" className="button button-danger" onClick={() => void cancel()}>Cancel upload</button>}
            {!busy && error !== undefined && <button type="button" className="button button-secondary" onClick={startNew}>Start a new upload</button>}
            {!busy && result && <button type="button" className="button button-secondary" onClick={startNew}>Start another upload</button>}</div>
        </form>{message && <p role="status" className="upload-progress">{message}</p>}{error !== undefined && <ProblemNotice error={error} />}{cancelOperation && <OperationNotice id={cancelOperation} />}
        {result?.appId && <div className="form-actions section-action"><ActionLink to={`/apps/${result.appId}`} secondary>Open application</ActionLink><ActionLink to={`/apps/${result.appId}/releases/${result.releaseId}`} secondary>Open release</ActionLink></div>}
      </section>}
  </>
}
