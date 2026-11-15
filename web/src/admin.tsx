import { useEffect, useRef, useState, type FormEvent } from 'react'
import { apiRequest, idempotencyKey, jsonBody, resource, type Diagnostics, type Page, type ServerSettings, type User } from './api'
import { ActionLink, EmptyState, LoadingState, PageHeader, ProblemNotice, StatusBadge, TechnicalDetails } from './components'
import { ConfirmDialog } from './management'
import { useAuth } from './portal'
import { useResource } from './use-resource'

type ImportSource = { id: string; displayName: string; kind: 'Inbox' | 'Configured'; containerPath: string;
  available: boolean; importReadOnly: boolean }

export function ImportSourcesPage() {
  const sources = useResource('admin-import-sources', signal => apiRequest<{ items: ImportSource[] }>('/admin/import-sources', { authenticated: true, signal }).then(result => result.value))
  return <><PageHeader title="Import sources" description="Approved locations for Server-side Artifact Import." actions={<button className="button button-secondary" onClick={sources.retry}>Refresh</button>} />
    <section className="panel"><h2>App Depot Inbox</h2><p>The default Inbox is a persistent landing directory next to the managed data root. App Depot creates the directory when permitted. The import browser never writes or deletes source content; the container needs filesystem write access only to create the directory. The Inbox is not CAS, a backup member, a ZFS dataset, or an SMB share.</p>
      <p>To expose it through SMB or NFS, an operator must configure a host share. A separate host dataset can be mounted at the Inbox container path; changing mounts requires a container restart.</p></section>
    {sources.loading ? <LoadingState /> : sources.error ? <ProblemNotice error={sources.error} retry={sources.retry} /> :
      <section className="panel"><h2>Configured sources</h2><div className="table-wrap"><table><thead><tr><th>Source</th><th>Availability</th><th>Import access</th><th>Container path</th></tr></thead><tbody>
        {sources.value!.items.map(source => <tr key={source.id}><td><strong>{source.displayName}</strong><small>{source.kind === 'Inbox' ? 'Built-in, Server Admin only' : 'Operator-configured root'}</small></td>
          <td><StatusBadge value={source.available ? 'Available' : 'Unavailable'} /></td><td>{source.importReadOnly ? 'Read-only operations' : 'Unknown'}</td><td><code>{source.containerPath}</code></td></tr>)}
      </tbody></table></div><p className="subtle">Configured roots are read by App Depot; the Server does not verify the host mount mode. Mount external sources read-only and set `APP_DEPOT_IMPORT_ROOTS` in the container configuration. An unavailable Inbox requires a writable parent or a mounted directory at its container path. These settings cannot be changed from the Portal.</p>
      {sources.value!.items.some(source => source.available) && <ActionLink to="/import/new-app" secondary>Import into a new application</ActionLink>}
      </section>}
  </>
}

function UserEditor({ id, changed }: { id: string; changed: () => void }) {
  const { state, refresh } = useAuth()
  const current = useResource(`admin-user:${id}`, signal => resource<User>(`/users/${id}`, signal))
  const [publish, setPublish] = useState(false)
  const [admin, setAdmin] = useState(false)
  const [error, setError] = useState<unknown>()
  const [notice, setNotice] = useState('')
  useEffect(() => { if (current.value) { setPublish(current.value.value.publishPermission); setAdmin(current.value.value.serverAdmin) } }, [current.value])
  async function save() {
    setError(undefined); setNotice('')
    try {
      await apiRequest(`/users/${id}`, { method: 'PATCH', authenticated: true, csrf: state.session!.csrfToken,
        ifMatch: current.value!.etag, ...jsonBody({ publishPermission: publish, serverAdmin: admin }) })
      setNotice('Permissions saved.'); current.retry(); changed()
      if (state.session!.user.id === id) refresh()
    } catch (failure) { setError(failure); throw failure }
  }
  async function revoke() {
    setError(undefined); setNotice('')
    try {
      await apiRequest<void>(`/users/${id}/token`, { method: 'DELETE', authenticated: true, csrf: state.session!.csrfToken })
      setNotice('Personal API token revoked, if one was active.')
    } catch (failure) { setError(failure); throw failure }
  }
  return <section className="panel"><h2>Manage user</h2>{current.loading ? <LoadingState /> : current.error ? <ProblemNotice error={current.error} retry={current.retry} /> : <>
    <p><strong>{current.value!.value.username}</strong> can read accessible resources as an authenticated user. Publish permission allows new Apps and Release publication; ownership of existing Apps remains after it is revoked. Server Admin can manage all resources and server settings.</p>
    <div className="form-stack form-panel"><label className="checkbox-row"><input type="checkbox" checked={publish} onChange={event => setPublish(event.target.checked)} />Publish permission</label>
      <label className="checkbox-row"><input type="checkbox" checked={admin} onChange={event => setAdmin(event.target.checked)} />Server Admin</label>
      {(publish !== current.value!.value.publishPermission || admin !== current.value!.value.serverAdmin) &&
        <ConfirmDialog label="Save permissions" detail={`Set ${current.value!.value.username} to ${admin ? 'Server Admin' : 'non-Admin'} and ${publish ? 'grant' : 'revoke'} Publish permission? Existing App ownership is preserved.`} confirm="Apply permissions" onConfirm={save} />}
    </div>
    <div className="section-action"><ConfirmDialog label="Revoke personal token" detail={`Immediately invalidate ${current.value!.value.username}'s personal API token, if active? This does not delete their account or browser sessions.`} confirm="Revoke token" onConfirm={revoke} /></div>
    {error !== undefined && <ProblemNotice error={error} />}{notice && <p className="success-notice" role="status">{notice}</p>}
    <TechnicalDetails><code>User ID: {id}</code></TechnicalDetails>
  </>}</section>
}

export function UsersPage() {
  const { state } = useAuth()
  const [cursor, setCursor] = useState('')
  const [previous, setPrevious] = useState<string[]>([])
  const [revision, setRevision] = useState(0)
  const [selected, setSelected] = useState('')
  const users = useResource(`users:${cursor}:${revision}`, signal => apiRequest<Page<User>>(`/users?pageSize=20${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`, { authenticated: true, signal }).then(result => result.value))
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [publish, setPublish] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const [notice, setNotice] = useState('')
  const attempt = useRef<{ fingerprint: string; key: string } | null>(null)
  async function create(event: FormEvent) {
    event.preventDefault()
    const fingerprint = JSON.stringify([username.trim(), password, publish])
    if (!attempt.current || attempt.current.fingerprint !== fingerprint) attempt.current = { fingerprint, key: idempotencyKey() }
    setBusy(true); setError(undefined); setNotice('')
    try {
      await apiRequest<User>('/users', { method: 'POST', authenticated: true, csrf: state.session!.csrfToken,
        idempotencyKey: attempt.current.key, ...jsonBody({ username: username.trim(), password, publishPermission: publish }) })
      attempt.current = null; setUsername(''); setPassword(''); setPublish(false); setCursor(''); setPrevious([])
      setRevision(value => value + 1); setNotice('User created.')
    } catch (failure) { setError(failure) } finally { setBusy(false) }
  }
  return <><PageHeader title="Users & Access" description="Manage local accounts and global permissions." />
    <section className="panel"><h2>Permission model</h2><p>Server Admin manages the server and all Apps. Publish permission allows creating Apps and publishing Releases. Owners can continue managing their own Apps after Publish permission is revoked. There are no groups or per-App grants.</p></section>
    <section className="panel form-panel"><h2>Create user</h2><form className="form-stack" onSubmit={create}>
      <label>Username<input value={username} onChange={event => setUsername(event.target.value)} required maxLength={255} autoComplete="off" /></label>
      <label>Initial password<input type="password" value={password} onChange={event => setPassword(event.target.value)} required maxLength={1024} autoComplete="new-password" /></label>
      <label className="checkbox-row"><input type="checkbox" checked={publish} onChange={event => setPublish(event.target.checked)} />Grant Publish permission</label>
      <button className="button button-primary" disabled={busy || !username.trim() || !password}>{busy ? 'Creating…' : 'Create user'}</button></form>
      {error !== undefined && <ProblemNotice error={error} />}{notice && <p className="success-notice" role="status">{notice}</p>}
    </section>
    <section className="panel"><h2>Accounts</h2>{users.loading ? <LoadingState /> : users.error ? <ProblemNotice error={users.error} retry={users.retry} /> : <>
      {!users.value!.items.length ? <EmptyState title="No users on this page" detail="Create a local account to add one." /> : <div className="table-wrap"><table><thead><tr><th>Username</th><th>Server Admin</th><th>Publish</th><th>Action</th></tr></thead><tbody>{users.value!.items.map(user => <tr key={user.id}><td><strong>{user.username}</strong><small>{user.id === state.session!.user.id ? 'Your account' : 'Local account'}</small></td><td>{user.serverAdmin ? 'Yes' : 'No'}</td><td>{user.publishPermission ? 'Allowed' : 'Not allowed'}</td><td><button className="text-button" onClick={() => setSelected(user.id)}>Manage</button></td></tr>)}</tbody></table></div>}
      <div className="pagination"><button className="button button-secondary" disabled={!cursor} onClick={() => { const stack = [...previous]; setCursor(stack.pop() || ''); setPrevious(stack) }}>Previous page</button>
        <button className="button button-secondary" disabled={!users.value!.nextCursor} onClick={() => { setPrevious(value => [...value, cursor]); setCursor(users.value!.nextCursor!) }}>Next page</button></div>
    </>}</section>
    {selected && <UserEditor key={selected} id={selected} changed={() => setRevision(value => value + 1)} />}
    <p className="subtle">The current Users API has no user-deletion operation. Revoking a personal token does not delete its account.</p>
  </>
}

export function DiagnosticsPage() {
  const diagnostics = useResource('admin-diagnostics', signal => apiRequest<Diagnostics>('/admin/diagnostics', { authenticated: true, signal }).then(result => result.value))
  const value = diagnostics.value
  return <><PageHeader title="Diagnostics" description="Current redacted Server aggregates." actions={<button className="button button-secondary" onClick={diagnostics.retry}>Refresh</button>} />
    {diagnostics.loading ? <LoadingState /> : diagnostics.error ? <ProblemNotice error={diagnostics.error} retry={diagnostics.retry} /> : <>
      <section className="panel"><h2>Server state</h2><dl className="facts"><div><dt>Readiness</dt><dd><StatusBadge value={value!.readiness} /></dd></div><div><dt>Recovery</dt><dd><StatusBadge value={value!.recovery} /></dd></div><div><dt>Migration</dt><dd><StatusBadge value={value!.migration} /></dd></div><div><dt>Storage pressure</dt><dd><StatusBadge value={value!.storagePressure} /></dd></div></dl></section>
      <section className="panel"><h2>Requests</h2><p className="subtle">Current measurement window: {value!.windowSeconds} seconds. These are aggregates, not a history.</p><dl className="facts"><div><dt>Request failures</dt><dd>{value!.requestFailures}</dd></div><div><dt>95th percentile latency</dt><dd>{value!.requestLatencyMsP95} ms</dd></div></dl></section>
      <section className="panel"><h2>Active work and cleanup</h2><dl className="facts"><div><dt>Active uploads</dt><dd>{value!.activeUploads}</dd></div><div><dt>Active transfers</dt><dd>{value!.activeTransfers}</dd></div><div><dt>Pending cleanup</dt><dd>{value!.pendingCleanup}</dd></div><div><dt>Retryable cleanup failures</dt><dd>{value!.retryableCleanup}</dd></div><div><dt>Backups running</dt><dd>{value!.backupsRunning}</dd></div><div><dt>Backups failed</dt><dd>{value!.backupsFailed}</dd></div></dl></section>
    </>}
  </>
}

export function ServerSettingsPage() {
  const { state } = useAuth()
  const current = useResource('admin-settings', signal => resource<ServerSettings>('/settings', signal))
  const [anonymous, setAnonymous] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const [notice, setNotice] = useState('')
  useEffect(() => { if (current.value) setAnonymous(current.value.value.anonymousReadEnabled) }, [current.value])
  async function save(event: FormEvent) {
    event.preventDefault(); setBusy(true); setError(undefined); setNotice('')
    try {
      await apiRequest('/settings', { method: 'PATCH', authenticated: true, csrf: state.session!.csrfToken,
        ifMatch: current.value!.etag, ...jsonBody({ anonymousReadEnabled: anonymous }) })
      current.retry(); setNotice('Server settings saved.')
    } catch (failure) { setError(failure) } finally { setBusy(false) }
  }
  return <><PageHeader title="Server Settings" description="Dynamic settings available through the Server API." />
    {current.loading ? <LoadingState /> : current.error ? <ProblemNotice error={current.error} retry={current.retry} /> : <section className="panel form-panel"><h2>Anonymous read access</h2>
      <p>When enabled, unauthenticated clients may read the Public Published catalog and content. Private Apps and Draft content remain protected. This setting does not make HTTP confidential.</p>
      <form className="form-stack" onSubmit={save}><label className="checkbox-row"><input type="checkbox" checked={anonymous} onChange={event => setAnonymous(event.target.checked)} />Enable anonymous read</label>
        <button className="button button-primary" disabled={busy || anonymous === current.value!.value.anonymousReadEnabled}>{busy ? 'Saving…' : 'Save setting'}</button></form>
      {error !== undefined && <ProblemNotice error={error} />}{notice && <p className="success-notice" role="status">{notice}</p>}
      <p className="subtle">TLS, proxy trust, data-root paths, container UID/GID and other deployment settings are configured outside this page and may require a container restart.</p>
    </section>}
  </>
}
