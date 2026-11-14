import { sha256 } from '@noble/hashes/sha256'
import { bytesToHex } from '@noble/hashes/utils'
import { useEffect, useRef, useState } from 'react'
import { useSearchParams } from 'react-router'
import { apiRequest, apiStream, idempotencyKey, type BackupManifest, type BackupMember, type BackupSummary, type Operation, type Page } from './api'
import { EmptyState, LoadingState, PageHeader, ProblemNotice, TechnicalDetails } from './components'
import { OperationNotice } from './management'
import { useAuth } from './portal'
import { useResource } from './use-resource'

const enc = encodeURIComponent

function BackupDetail({ id }: { id: string }) {
  const manifest = useResource(`backup-manifest:${id}`, signal => apiRequest<BackupManifest>(`/backups/${id}`, { authenticated: true, signal }).then(result => result.value))
  const [page, setPage] = useState(0)
  const [verification, setVerification] = useState<Record<string, string>>({})
  const controller = useRef<AbortController | null>(null)
  useEffect(() => () => controller.current?.abort(), [])
  async function verify(member: BackupMember) {
    const abort = new AbortController(); controller.current = abort
    setVerification(current => ({ ...current, [member.entryId]: 'Verifying…' }))
    try {
      if (!Number.isSafeInteger(member.size)) throw new Error('Member size exceeds exact browser integer range; verify it offline.')
      const response = await apiStream(`/backups/${id}/entries/${member.entryId}`, abort.signal)
      if (!response.body) throw new Error('Streaming response is unavailable in this browser.')
      const reader = response.body.getReader()
      const hash = sha256.create()
      let size = 0n
      while (true) {
        const chunk = await reader.read()
        if (chunk.done) break
        hash.update(chunk.value); size += BigInt(chunk.value.byteLength)
      }
      const digest = bytesToHex(hash.digest())
      if (size !== BigInt(member.size) || digest !== member.sha256) throw new Error('Size or SHA-256 mismatch; do not use this export for restore.')
      setVerification(current => ({ ...current, [member.entryId]: 'Verified size and SHA-256' }))
    } catch (error) {
      setVerification(current => ({ ...current, [member.entryId]: error instanceof Error ? error.message : 'Verification failed' }))
    } finally { if (controller.current === abort) controller.current = null }
  }
  function downloadManifest(value: BackupManifest) {
    const url = URL.createObjectURL(new Blob([JSON.stringify(value, null, 2)], { type: 'application/json' }))
    const link = document.createElement('a')
    link.href = url; link.download = 'manifest.json'; link.click()
    setTimeout(() => URL.revokeObjectURL(url), 1000)
  }
  const value = manifest.value
  const members = value?.members.slice(page * 50, (page + 1) * 50) || []
  return <section className="panel"><h2>Backup manifest</h2>{manifest.loading ? <LoadingState /> : manifest.error ? <ProblemNotice error={manifest.error} retry={manifest.retry} /> : <>
    <p>Format {value!.formatVersion}; captured {new Date(value!.capturedAt).toLocaleString()}; {value!.members.length} members. This manifest describes a portable backup directory.</p>
    <button className="button button-secondary" onClick={() => downloadManifest(value!)}>Download manifest.json</button>
    <p className="subtle">Download every member, place it at the exact relative path shown below under one backup directory, and validate the complete directory offline. Browser downloads alone are not a verified restorable backup. This page does not create a ZIP archive.</p>
    <div className="table-wrap section-action"><table><thead><tr><th>Portable relative path</th><th>Kind</th><th>Size</th><th>Actions</th></tr></thead><tbody>{members.map(member => <tr key={member.entryId}><td><code>{member.relativePath}</code><small>SHA-256: {member.sha256}</small></td><td>{member.kind === 'SqliteSnapshot' ? 'SQLite snapshot' : 'CAS object'}</td><td>{Number.isSafeInteger(member.size) ? `${member.size.toLocaleString()} bytes` : 'Verify offline'}</td>
      <td><div className="form-actions"><a className="button button-secondary" href={`/api/v1/backups/${enc(id)}/entries/${enc(member.entryId)}`} download={member.relativePath.split('/').pop()}>Download</a><button className="button button-secondary" onClick={() => void verify(member)}>Verify</button></div>
        {verification[member.entryId] && <small role="status">{verification[member.entryId]}</small>}</td></tr>)}</tbody></table></div>
    <div className="pagination"><button className="button button-secondary" disabled={page === 0} onClick={() => setPage(value => value - 1)}>Previous members</button><button className="button button-secondary" disabled={(page + 1) * 50 >= value!.members.length} onClick={() => setPage(value => value + 1)}>Next members</button></div>
    <TechnicalDetails><code>Backup ID: {value!.backupId}</code><p>Catalog format version: {value!.catalogFormatVersion}</p></TechnicalDetails>
  </>}</section>
}

export function BackupsPage() {
  const { state } = useAuth()
  const [cursor, setCursor] = useState('')
  const [previous, setPrevious] = useState<string[]>([])
  const [revision, setRevision] = useState(0)
  const backups = useResource(`backups:${cursor}:${revision}`, signal => apiRequest<Page<BackupSummary>>(`/backups?pageSize=10${cursor ? `&cursor=${enc(cursor)}` : ''}`, { authenticated: true, signal }).then(result => result.value))
  const [params, setParams] = useSearchParams()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const attempt = useRef(idempotencyKey())
  const selected = params.get('backup')
  const operation = params.get('operation')
  function select(next: URLSearchParams) { setParams(next) }
  async function start() {
    setBusy(true); setError(undefined)
    try {
      const result = await apiRequest<Operation>('/backups', { method: 'POST', authenticated: true, csrf: state.session!.csrfToken, idempotencyKey: attempt.current })
      attempt.current = idempotencyKey()
      select(new URLSearchParams({ operation: result.value.id })); setRevision(value => value + 1)
    } catch (failure) { setError(failure) } finally { setBusy(false) }
  }
  return <><PageHeader title="Backups" description="Completed backups with verified available output." actions={<button className="button button-secondary" onClick={() => setRevision(value => value + 1)}>Refresh list</button>} />
    <section className="panel"><h2>Online backup</h2><p>The Server creates a consistent SQLite/CAS snapshot. Starting a backup does not export it to your computer.</p>
      <p className="subtle">Backups contain password hashes and token verifiers. Export only over a trusted network; direct HTTP has no transport confidentiality.</p>
      <button className="button button-primary" onClick={() => void start()} disabled={busy}>{busy ? 'Starting…' : 'Start online backup'}</button>{error !== undefined && <ProblemNotice error={error} />}</section>
    {operation && <OperationNotice id={operation} />}
    <section className="panel"><h2>Available backups</h2>{backups.loading ? <LoadingState /> : backups.error ? <ProblemNotice error={backups.error} retry={backups.retry} /> : <>
      {!backups.value!.items.length ? <EmptyState title="No available backups" detail="A running, failed, missing, or incomplete backup is not listed as available." /> : <div className="table-wrap"><table><thead><tr><th>Completed</th><th>Members</th><th>Size</th><th>Action</th></tr></thead><tbody>{backups.value!.items.map(backup => <tr key={backup.id}><td>{new Date(backup.completedAt).toLocaleString()}<small>Captured {new Date(backup.capturedAt).toLocaleString()}</small></td><td>{backup.memberCount}</td><td>{Number.isSafeInteger(backup.totalBytes) ? `${backup.totalBytes.toLocaleString()} bytes` : 'Verify offline'}</td><td><button className="text-button" onClick={() => select(new URLSearchParams({ backup: backup.id }))}>Inspect</button></td></tr>)}</tbody></table></div>}
      <div className="pagination"><button className="button button-secondary" disabled={!cursor} onClick={() => { const stack = [...previous]; setCursor(stack.pop() || ''); setPrevious(stack) }}>Previous page</button><button className="button button-secondary" disabled={!backups.value!.nextCursor} onClick={() => { setPrevious(value => [...value, cursor]); setCursor(backups.value!.nextCursor!) }}>Next page</button></div>
    </>}</section>
    {selected && <BackupDetail key={selected} id={selected} />}
    <section className="panel"><h2>Restore</h2><p>Restore is an offline operation. Stop the Server and use the supported CLI with exclusive data-root ownership; no HTTP restore is available.</p>
      <a href="https://github.com/viktorliho/app-depot-server/blob/main/docs/deployment-operations.md#offline-restore-corruption-and-migration" target="_blank" rel="noreferrer">Read the recovery and restore instructions</a></section>
  </>
}
