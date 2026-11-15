import { useEffect, useMemo, useRef, useState, type FormEvent } from 'react'
import { useOutletContext, useParams } from 'react-router'
import { apiRequest, fileSha256, idempotencyKey, jsonBody, resource, type DepotApp, type FinalizationResult,
  SourceChangedError, type InventoryEntry, type Operation, type Page, type Release, type UploadProgress, type UploadSession } from './api'
import { ActionLink, LoadingState, PageHeader, ProblemNotice } from './components'
import { OperationNotice } from './management'
import { useAuth } from './portal'
import { useResource } from './use-resource'
import { sha256 } from '@noble/hashes/sha256'
import { bytesToHex } from '@noble/hashes/utils'
import { isOsMetadataPath, pathOrder, selectTreeFiles } from './upload-selection'

type Mode = 'BareFile' | 'BareTree' | 'ZipImport'
type Capabilities = { ingestionModes: Mode[]; features: string[]; limits: { maxPageSize: number; maxUploadBytes: number } }
type SelectedFile = { file: File; path: string; size: number; lastModified: number }
type PreparedFile = SelectedFile & { hash: string }
type Keys = { fingerprint: string; create: string; complete: string; finalize: string; publish: string; inventory: string[]; cancel: string }
type Phase = 'Preparing' | 'Hashing' | 'Inventory' | 'Transferring' | 'Verifying' | 'Committing' | 'Publishing' | 'Completed' | 'Failed' | 'Paused'
type UploadView = { phase: Phase; totalFiles: number; filesDone: number; totalBytes: number; hashedBytes: number; acknowledgedBytes: number; currentFile: string; transferStartedAt: number; newlyAcknowledged: number; resumed: boolean }
const enc = encodeURIComponent
const chunkSize = 4 * 1024 * 1024
function formattedBytes(value: number) { return value >= 1024 * 1024 * 1024 ? `${(value / (1024 ** 3)).toFixed(2)} GiB` : value >= 1024 * 1024 ? `${(value / (1024 ** 2)).toFixed(1)} MiB` : value >= 1024 ? `${(value / 1024).toFixed(1)} KiB` : `${Math.round(value)} B` }
function duration(seconds: number) { return seconds >= 60 ? `${Math.floor(seconds / 60)}m ${Math.round(seconds % 60)}s` : `${Math.round(seconds)}s` }

function sourceMetadata(item: SelectedFile) {
  if (item.file.size !== item.size || item.file.lastModified !== item.lastModified) {
    throw new SourceChangedError(`Source file ${item.path} changed size or modification time after selection. Reselect the source and start a new upload.`)
  }
}

function readKeys(key: string): Keys | null {
  try { const value = sessionStorage.getItem(key); return value ? JSON.parse(value) as Keys : null }
  catch { return null }
}
function saveKeys(key: string, value: Keys) { try { sessionStorage.setItem(key, JSON.stringify(value)) } catch { /* In-tab retries remain available. */ } }
function forgetKeys(key: string) { try { sessionStorage.removeItem(key) } catch { /* Storage is unavailable. */ } }

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

async function transfer(sessionId: string, fileId: string, item: PreparedFile, csrf: string, signal: AbortSignal, pageSize: number, report: (acknowledged: number, newlyAcknowledged: number, resumed: boolean) => void, freshInventory = false) {
  const { file, path, hash: expected } = item
  sourceMetadata(item)
  const ranges = file.size && !freshInventory ? await progress(sessionId, fileId, signal, pageSize) : []
  const gaps: { start: number; end: number }[] = []
  let cursor = 0
  let acknowledged = 0
  for (const range of ranges) {
    if (cursor < range.start) gaps.push({ start: cursor, end: range.start })
    acknowledged += range.end - range.start
    cursor = Math.max(cursor, range.end)
  }
  if (cursor < file.size) gaps.push({ start: cursor, end: file.size })
  report(acknowledged, 0, acknowledged > 0)
  const hash = sha256.create()
  let gapIndex = 0
  for (let offset = 0; offset < file.size; offset += chunkSize) {
    if (signal.aborted) throw new DOMException('Cancelled', 'AbortError')
    sourceMetadata(item)
    const end = Math.min(file.size, offset + chunkSize)
    let bytes: Uint8Array
    try { bytes = new Uint8Array(await file.slice(offset, end).arrayBuffer()) }
    catch { throw new SourceChangedError(`Source file ${path} became unreadable while transferring. Reselect it and start a new upload.`) }
    if (bytes.length !== end - offset) throw new SourceChangedError(`Source file ${path} changed while transferring. Reselect the directory or file and start a new upload.`)
    hash.update(bytes)
    while (gapIndex < gaps.length && gaps[gapIndex].start < end) {
      const gap = gaps[gapIndex]
      const start = Math.max(offset, gap.start)
      const stop = Math.min(end, gap.end)
      if (start < stop) {
        await apiRequest<void>(`/uploads/${sessionId}/files/${fileId}`, { method: 'PUT', authenticated: true, csrf, signal,
          headers: { 'Content-Type': 'application/octet-stream', 'Content-Range': `bytes ${start}-${stop - 1}/${file.size}` }, body: bytes.subarray(start - offset, stop - offset) })
        acknowledged += stop - start
        report(acknowledged, stop - start, false)
      }
      if (gap.end <= end) gapIndex++
      else break
    }
  }
  if (bytesToHex(hash.digest()) !== expected) throw new SourceChangedError(`Source file ${path} changed after hashing. The staged upload cannot be finalized; reselect the source and start a new upload.`)
  if (acknowledged !== file.size) throw new Error('The Server has not acknowledged the complete file. Retry the transfer.')
}

function treeEntries(files: PreparedFile[], extraDirectories: string, excludeOsMetadata: boolean): { path: string; type: 'File' | 'Directory'; size?: number; sha256?: string; executable?: boolean }[] {
  const directories = new Set<string>()
  for (const file of files) {
    const parts = file.path.split('/')
    for (let length = 1; length < parts.length; length++) directories.add(parts.slice(0, length).join('/'))
  }
  for (const raw of extraDirectories.split('\n')) {
    const path = raw.trim().replace(/\/$/, '')
    if (!path) continue
    if (excludeOsMetadata && isOsMetadataPath(path)) continue
    const parts = path.split('/')
    for (let length = 1; length <= parts.length; length++) directories.add(parts.slice(0, length).join('/'))
  }
  const entries = [
    ...[...directories].map(path => ({ path, type: 'Directory' as const })),
    ...files.map(({ path, hash, size }) => ({ path, type: 'File' as const, size, sha256: hash, executable: false }))
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
  const [excludeOsMetadata, setExcludeOsMetadata] = useState(true)
  const [extraDirectories, setExtraDirectories] = useState('')
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState('')
  const [error, setError] = useState<unknown>()
  const [result, setResult] = useState<FinalizationResult>()
  const [activeSession, setActiveSession] = useState('')
  const [cancelOperation, setCancelOperation] = useState('')
  const [view, setView] = useState<UploadView>()
  const [selectionVersion, setSelectionVersion] = useState(0)
  const [clock, setClock] = useState(0)
  const controller = useRef<AbortController | null>(null)
  useEffect(() => { if (!busy) return; const timer = window.setInterval(() => setClock(performance.now()), 1000); return () => window.clearInterval(timer) }, [busy])
  useEffect(() => () => controller.current?.abort(), [])
  const keys = useRef<Keys | null>(null)
  const canCreate = state.session!.user.serverAdmin || state.session!.user.publishPermission
  const canManage = !app || state.session!.user.serverAdmin || state.session!.user.id === app.ownerId
  const canPublish = canCreate
  const storageKey = `app-depot-upload:${target}:${app?.id || 'new'}:${releaseId || ''}`
  const treeSelection = useMemo(() => selectTreeFiles(directoryFiles, excludeOsMetadata), [directoryFiles, excludeOsMetadata])
  const validInput = mode === 'BareTree' ? treeSelection.files.length > 0 : !!single
  function reset() { setResult(undefined); setError(undefined); setMessage(''); setActiveSession(''); setCancelOperation(''); setView(undefined) }
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
    let sessionId = ''
    try {
      if (!capabilities.value) throw new Error('Server upload capabilities are unavailable.')
      if (!capabilities.value.ingestionModes.includes(mode)) throw new Error('The Server does not support the selected upload mode.')
      setMessage('Preparing selected files')
      const files: SelectedFile[] = (mode === 'BareTree' ? treeSelection.files : [{ file: single!, path: single!.name }])
        .map(item => ({ ...item, size: item.file.size, lastModified: item.file.lastModified }))
      const totalBytes = files.reduce((total, item) => total + item.size, 0)
      if (totalBytes > capabilities.value.limits.maxUploadBytes) throw new Error('Selected upload exceeds the Server byte limit.')
      setView({ phase: 'Preparing', totalFiles: files.length, filesDone: 0, totalBytes, hashedBytes: 0, acknowledgedBytes: 0, currentFile: '', transferStartedAt: 0, newlyAcknowledged: 0, resumed: false })
      await new Promise(resolve => setTimeout(resolve, 0))
      const pageSize = Math.min(100, capabilities.value.limits.maxPageSize)
      const prepared: PreparedFile[] = []
      setMessage('Hashing selected files'); setView(current => current && { ...current, phase: 'Hashing' })
      for (const [index, item] of files.entries()) {
        if (signal.aborted) throw new DOMException('Cancelled', 'AbortError')
        sourceMetadata(item)
        setView(current => current && { ...current, currentFile: item.path })
        prepared.push({ ...item, hash: await fileSha256(item.file, signal, count => setView(current => current && { ...current, hashedBytes: current.hashedBytes + count })) })
        sourceMetadata(item)
        setView(current => current && { ...current, filesDone: index + 1 })
      }
      const entries = mode === 'BareTree' ? treeEntries(prepared, extraDirectories, excludeOsMetadata) : []
      const targetValue = target === 'release' ? { releaseId } : target === 'app' ? { appId: app!.id, createRelease: true } : { newAppName: name.trim() }
      const fingerprint = JSON.stringify({ targetValue, mode, excludeOsMetadata: mode === 'BareTree' ? excludeOsMetadata : null,
        files: prepared.map(item => [item.path, item.size, item.hash]), entries })
      const previous = readKeys(storageKey)
      if (!keys.current || keys.current.fingerprint !== fingerprint) keys.current = previous?.fingerprint === fingerprint ? previous : {
        fingerprint, create: idempotencyKey(), complete: idempotencyKey(), finalize: idempotencyKey(), publish: idempotencyKey(),
        inventory: [], cancel: idempotencyKey()
      }
      const activeKeys = keys.current
      saveKeys(storageKey, activeKeys)
      setMessage('Creating or resuming upload session')
      setView(current => current && { ...current, phase: 'Inventory', filesDone: 0, currentFile: '' })
      const createBody = mode === 'BareTree' ? { mode, target: targetValue } : { mode, target: targetValue, file: { size: prepared[0].size, sha256: prepared[0].hash } }
      const session = (await apiRequest<UploadSession>('/uploads', { method: 'POST', authenticated: true, signal,
        csrf: state.session!.csrfToken, idempotencyKey: activeKeys.create, ...jsonBody(createBody) })).value
      sessionId = session.id
      setActiveSession(session.id)
      if (session.phase === 'Cancelled' || session.phase === 'Failed') throw new Error(`Upload session is ${session.phase}. Start a new upload with a fresh operation after checking the source files.`)
      let final: FinalizationResult
      if (session.phase === 'Receiving' && mode === 'BareTree') {
        const freshInventory = !session.inventoryComplete
        const returnedFiles = new Map<string, InventoryEntry>()
        if (!session.inventoryComplete) {
          for (let index = 0; index < entries.length; index += 100) {
            const batch = index / 100
            if (!activeKeys.inventory[batch]) { activeKeys.inventory[batch] = idempotencyKey(); saveKeys(storageKey, activeKeys) }
            setMessage(`Registering directory inventory ${Math.min(index + 100, entries.length)} of ${entries.length}`)
            const appended = (await apiRequest<{ entries: InventoryEntry[] }>(`/uploads/${session.id}/inventory`, { method: 'POST', authenticated: true, signal,
              csrf: state.session!.csrfToken, idempotencyKey: activeKeys.inventory[batch], ...jsonBody({ entries: entries.slice(index, index + 100) }) })).value
            for (const entry of appended.entries) returnedFiles.set(entry.path, entry)
            setView(current => current && { ...current, filesDone: Math.min(index + 100, entries.length), totalFiles: entries.length })
          }
          await apiRequest(`/uploads/${session.id}/inventory/complete`, { method: 'POST', authenticated: true, signal,
            csrf: state.session!.csrfToken, idempotencyKey: activeKeys.complete })
        }
        const assigned = freshInventory ? returnedFiles : await inventoryFiles(session.id, signal, pageSize)
        let settled = 0
        let transferFiles = 0
        setView(current => current && { ...current, phase: 'Transferring', filesDone: 0, totalFiles: prepared.length, transferStartedAt: performance.now(), currentFile: '' })
        for (const item of prepared) {
          const registered = assigned.get(item.path)
          if (!registered?.fileId || registered.sha256 !== item.hash) throw new Error(`Inventory mismatch for ${item.path}`)
          setView(current => current && { ...current, currentFile: item.path })
          if (registered.transferRequired !== false) await transfer(session.id, registered.fileId, item, state.session!.csrfToken, signal, pageSize, (acknowledged, delta, resumed) => {
            setView(current => current && { ...current, acknowledgedBytes: settled + acknowledged, newlyAcknowledged: current.newlyAcknowledged + delta, resumed: current.resumed || resumed })
          }, freshInventory)
          settled += item.size
          transferFiles++
          setView(current => current && { ...current, acknowledgedBytes: settled, filesDone: transferFiles })
        }
      } else if (session.phase === 'Receiving') {
        if (!session.file?.fileId) throw new Error('Server did not assign an upload file ID.')
        setView(current => current && { ...current, phase: 'Transferring', filesDone: 0, totalFiles: 1, transferStartedAt: performance.now(), currentFile: single!.name })
        await transfer(session.id, session.file.fileId, prepared[0], state.session!.csrfToken, signal, pageSize, (acknowledged, delta, resumed) => {
          setView(current => current && { ...current, acknowledgedBytes: acknowledged, newlyAcknowledged: current.newlyAcknowledged + delta, resumed: current.resumed || resumed })
        })
        setView(current => current && { ...current, filesDone: 1, acknowledgedBytes: single!.size })
      }
      setMessage('Server verification and registration')
      setView(current => current && { ...current, phase: 'Verifying', currentFile: '' })
      if (session.phase === 'Receiving') {
        let polling = true
        const poll = async () => {
          while (polling && !signal.aborted) {
            await new Promise(resolve => setTimeout(resolve, 1000))
            if (!polling || signal.aborted) break
            try {
              const current = (await apiRequest<UploadSession>(`/uploads/${session.id}`, { authenticated: true, signal })).value
              if (current.phase === 'Verifying' || current.phase === 'Committing') setView(view => view && { ...view, phase: current.phase as Phase })
            } catch { /* Finalization's own result reports errors. */ }
          }
        }
        const pollingTask = poll()
        try {
          final = (await apiRequest<FinalizationResult>(`/uploads/${session.id}/finalize`, { method: 'POST', authenticated: true, signal,
            csrf: state.session!.csrfToken, idempotencyKey: activeKeys.finalize })).value
        } finally { polling = false; await pollingTask }
      } else final = (await apiRequest<FinalizationResult>(`/uploads/${session.id}/result`, { authenticated: true, signal })).value
      for (let attempt = 0; final.phase !== 'Completed' && final.phase !== 'Failed' && final.phase !== 'Cancelled' && attempt < 600; attempt++) {
        await new Promise(resolve => setTimeout(resolve, 1000))
        if (signal.aborted) throw new DOMException('Cancelled', 'AbortError')
        final = (await apiRequest<FinalizationResult>(`/uploads/${session.id}/result`, { authenticated: true, signal })).value
        if (final.phase === 'Verifying' || final.phase === 'Committing') setView(current => current && { ...current, phase: final.phase as Phase })
      }
      if (final.phase !== 'Completed' || !final.releaseId) throw new Error(final.problem?.detail || final.problem?.title || 'Registration did not complete. Check session status before retrying.')
      if (publishNow) {
        setMessage('Publishing release')
        setView(current => current && { ...current, phase: 'Publishing' })
        const current = await resource<Release>(`/releases/${final.releaseId}`, signal)
        await apiRequest(`/releases/${final.releaseId}/publish`, { method: 'POST', authenticated: true, signal,
          csrf: state.session!.csrfToken, idempotencyKey: activeKeys.publish, ifMatch: current.etag })
      }
      setResult(final)
      setView(current => current && { ...current, phase: 'Completed', acknowledgedBytes: current.totalBytes, filesDone: current.totalFiles })
      setMessage(`${publishNow ? 'Published' : 'Saved Draft for'} ${app?.name || name.trim()}`)
      forgetKeys(storageKey)
    } catch (failure) {
      if (failure instanceof DOMException && failure.name === 'AbortError') {
        setView(current => current && { ...current, phase: 'Paused' })
        setMessage('Transfer stopped. The Server may still have acknowledged ranges; retry with the same files to inspect progress.')
      } else if (failure instanceof SourceChangedError) {
        if (sessionId) {
          try {
            const cancelled = await apiRequest<Operation | FinalizationResult>(`/uploads/${sessionId}`, { method: 'DELETE', authenticated: true,
              csrf: state.session!.csrfToken, idempotencyKey: keys.current?.cancel || idempotencyKey() })
            if ('id' in cancelled.value) setCancelOperation(cancelled.value.id)
          } catch { setMessage('Source changed; cancellation could not be confirmed. Inspect the session before another attempt.') }
        }
        setSingle(null); setDirectoryFiles([]); setSelectionVersion(value => value + 1)
        keys.current = null; forgetKeys(storageKey)
        setView(current => current && { ...current, phase: 'Failed' })
        setError(failure)
      } else { setView(current => current && { ...current, phase: 'Failed' }); setError(failure) }
    } finally { setBusy(false); controller.current = null }
  }
  const heading = target === 'release' ? 'Upload Artifact' : target === 'app' ? 'Add release from file' : 'Upload a new application'
  const importRoute = target === 'release' ? `/apps/${app?.id}/releases/${releaseId}/imports/new` : target === 'app' ? `/apps/${app?.id}/imports/new` : '/import/new-app'
  return <><PageHeader title={heading} description={target === 'release' ? `Register an Artifact in the selected Draft release of ${app?.name}.` : target === 'app' ? `Create a Draft release in ${app?.name} from an Artifact.` : 'Create an application and Draft release from an Artifact.'} />
    <div className="form-actions section-action"><strong>Upload from this device</strong>{capabilities.value?.features.includes('serverImport') && <ActionLink to={importRoute} secondary>Import from server storage</ActionLink>}</div>
    {target === 'release' && release.loading ? <LoadingState /> : target === 'release' && release.error ? <ProblemNotice error={release.error} retry={release.retry} /> : target === 'release' && (release.value?.state !== 'Draft' || release.value.appId !== app?.id) ? <ProblemNotice error={new Error('The selected Draft release is unavailable in this application.')} /> : capabilities.loading ? <LoadingState label="Checking upload capabilities…" /> : capabilities.error ? <ProblemNotice error={capabilities.error} retry={capabilities.retry} /> :
      !canManage || (target === 'new-app' && !canCreate) ? <ProblemNotice error={new Error('You do not have permission to upload to this target.')} /> : <section className="panel form-panel">
        <p>Choose one ingestion mode. Registration creates or updates a Draft release; publication is a separate explicit action.</p>
        <form className="form-stack" onSubmit={submit}>
          {target === 'new-app' && <label>App name<input value={name} disabled={busy} onChange={event => { setName(event.target.value); reset() }} required /></label>}
          <label>Upload mode<select value={mode} disabled={busy} onChange={event => { setMode(event.target.value as Mode); reset() }}><option value="BareFile" disabled={!capabilities.value?.ingestionModes.includes('BareFile')}>Single file</option><option value="BareTree" disabled={!capabilities.value?.ingestionModes.includes('BareTree')}>Directory tree</option><option value="ZipImport" disabled={!capabilities.value?.ingestionModes.includes('ZipImport')}>ZIP import</option></select></label>
          {mode === 'BareTree' ? <><label>Directory files<input key={selectionVersion} type="file" multiple disabled={busy} {...{ webkitdirectory: '' }} onChange={event => { setDirectoryFiles(Array.from(event.target.files || [])); reset() }} /></label>
            <label className="checkbox-row"><input type="checkbox" checked={excludeOsMetadata} disabled={busy} onChange={event => { setExcludeOsMetadata(event.target.checked); reset() }} /> Exclude common macOS and Windows metadata files</label>
            <p className="subtle" role="status">{directoryFiles.length ? `${treeSelection.files.length.toLocaleString()} files selected for upload; ${treeSelection.excludedCount.toLocaleString()} OS metadata files excluded.` : 'Select a directory to review the file count before uploading.'} Uncheck for an exact upload. Only .DS_Store, AppleDouble files, .Spotlight-V100, .Trashes, .fseventsd, Thumbs.db, ehthumbs.db, and desktop.ini are excluded.</p>
            <label>Empty directory paths (optional, one relative path per line)<textarea rows={3} value={extraDirectories} disabled={busy} onChange={event => { setExtraDirectories(event.target.value); reset() }} /></label>
            <p className="subtle">Browsers may omit empty folders and do not expose executable flags. Add empty paths above; uploaded files are marked non-executable. A tree needs at least one regular file.</p></> :
            <label>File<input key={selectionVersion} type="file" disabled={busy} accept={mode === 'ZipImport' ? '.zip,application/zip' : undefined} onChange={event => { setSingle(event.target.files?.[0] || null); reset() }} required /></label>}
          <div className="form-actions"><button className="button button-secondary" value="draft" disabled={busy || !!result || !validInput || (target === 'new-app' && !name.trim())}>Save Draft</button>
            {canPublish && <button className="button button-primary" value="publish" disabled={busy || !!result || !validInput || (target === 'new-app' && !name.trim())}>Upload and publish</button>}
            {busy && <button type="button" className="button button-danger" onClick={() => void cancel()}>Cancel upload</button>}
            {!busy && error !== undefined && <button type="button" className="button button-secondary" onClick={startNew}>Start a new upload</button>}
            {!busy && result && <button type="button" className="button button-secondary" onClick={startNew}>Start another upload</button>}</div>
        </form>{view && <section className="upload-status" aria-label="Upload progress">
          <h2>{view.phase === 'Inventory' ? 'Registering inventory' : view.phase === 'Committing' ? 'CAS commit and registration' : view.phase === 'Verifying' ? 'Server verification' : view.phase}</h2>
          {view.phase === 'Hashing' || view.phase === 'Transferring' ? <progress max={view.totalBytes || 1} value={view.phase === 'Hashing' ? view.hashedBytes : view.acknowledgedBytes} aria-label={`${view.phase} bytes`} /> : view.phase === 'Inventory' && view.totalFiles ? <progress max={view.totalFiles} value={view.filesDone} aria-label="Inventory entries registered" /> : view.phase === 'Completed' ? <progress max={1} value={1} aria-label="Upload complete" /> : view.phase === 'Verifying' || view.phase === 'Committing' || view.phase === 'Publishing' ? <progress aria-label={`${view.phase} in progress`} /> : null}
          <p>{view.filesDone.toLocaleString()} / {view.totalFiles.toLocaleString()} {view.phase === 'Inventory' ? 'inventory entries' : 'files'} · {view.phase === 'Hashing' ? formattedBytes(view.hashedBytes) : formattedBytes(view.acknowledgedBytes)} / {formattedBytes(view.totalBytes)}</p>
          {view.phase === 'Transferring' && <p>{view.totalBytes ? `${Math.floor(view.acknowledgedBytes / view.totalBytes * 100)}% of bytes acknowledged by Server` : 'No bytes to transfer'}{view.resumed ? ' · Resumed from Server progress' : ''}</p>}
          {view.phase === 'Transferring' && view.transferStartedAt && clock > view.transferStartedAt + 3000 && view.newlyAcknowledged > 0 && <p>{formattedBytes(view.newlyAcknowledged / ((clock - view.transferStartedAt) / 1000))}/s average{view.newlyAcknowledged > 0 && view.acknowledgedBytes < view.totalBytes ? ` · About ${duration((view.totalBytes - view.acknowledgedBytes) / (view.newlyAcknowledged / ((clock - view.transferStartedAt) / 1000)))} remaining at this rate` : ''}</p>}
          {view.currentFile && <p className="upload-current">Current file: {view.currentFile}</p>}
          {view.phase === 'Verifying' || view.phase === 'Committing' ? <p className="subtle">Transfer is complete; Artifact registration is still in progress.</p> : null}
        </section>}{message && <p role="status" className="upload-progress">{message}</p>}{error !== undefined && <ProblemNotice error={error} />}{cancelOperation && <OperationNotice id={cancelOperation} />}
        {result?.appId && <div className="form-actions section-action"><ActionLink to={`/apps/${result.appId}`} secondary>Open application</ActionLink><ActionLink to={`/apps/${result.appId}/releases/${result.releaseId}`} secondary>Open release</ActionLink></div>}
      </section>}
  </>
}
