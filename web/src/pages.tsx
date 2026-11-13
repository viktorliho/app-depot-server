import { useEffect, useRef, useState, type FormEvent } from 'react'
import { Link, useNavigate, useOutletContext, useSearchParams } from 'react-router'
import { apiRequest, fileSha256, idempotencyKey, type AppPage, type DepotApp, type Diagnostics,
  type FinalizationResult, type ReleasePage, type UploadSession } from './api'
import { ActionLink, EmptyState, LoadingState, PageHeader, ProblemNotice, StatusBadge, TechnicalDetails } from './components'
import { AppIdentity, useAuth } from './portal'
import { useResource } from './use-resource'

function AdminStatus() {
  const diagnostics = useResource('diagnostics', signal => apiRequest<Diagnostics>('/admin/diagnostics', { signal, authenticated: true }).then(result => result.value))
  return <section className="panel"><h2>Operational status</h2>{diagnostics.loading ? <LoadingState label="Loading diagnostics…" /> : diagnostics.error ? <ProblemNotice error={diagnostics.error} retry={diagnostics.retry} /> :
    <dl className="facts"><div><dt>Storage pressure</dt><dd><StatusBadge value={diagnostics.value!.storagePressure} /></dd></div><div><dt>Active uploads</dt><dd>{diagnostics.value!.activeUploads}</dd></div><div><dt>Pending cleanup</dt><dd>{diagnostics.value!.pendingCleanup}</dd></div><div><dt>Backups running</dt><dd>{diagnostics.value!.backupsRunning}</dd></div></dl>}
    <p className="subtle">Current server aggregates. No historical activity feed is available.</p></section>
}

export function ServerOverview() {
  const { state } = useAuth()
  const canCreate = state.session!.user.publishPermission || state.session!.user.serverAdmin
  const apps = useResource('overview-apps', signal => apiRequest<AppPage>('/apps?pageSize=5', { signal, authenticated: true }).then(result => result.value))
  const ready = useResource('overview-ready', signal => apiRequest<{ status: string }>('/health/ready', { signal }).then(result => result.value))
  return <><PageHeader title="Server overview" description="Manage applications and check the current server state." actions={canCreate && !!apps.value?.items.length ? <ActionLink to="/apps/new">Create application</ActionLink> : undefined} />
    <div className="overview-grid"><section className="panel"><h2>Server readiness</h2>{ready.loading ? <LoadingState label="Checking readiness…" /> : ready.error ? <ProblemNotice error={ready.error} retry={ready.retry} /> : <><StatusBadge value={ready.value!.status} /><p className="subtle">Ready for catalog and management requests.</p></>}</section>
      <section className="panel"><h2>Applications</h2>{apps.loading ? <LoadingState label="Loading applications…" /> : apps.error ? <ProblemNotice error={apps.error} retry={apps.retry} /> : apps.value!.items.length ? <><ul className="compact-list">{apps.value!.items.map(app => <li key={app.id}><Link to={`/apps/${app.id}`}>{app.name}</Link><StatusBadge value={app.state} /></li>)}</ul><Link to="/apps">View all applications →</Link></> : <EmptyState title="No applications yet" detail="Create a Draft application or upload a file to start one." action={state.session!.user.publishPermission || state.session!.user.serverAdmin ? <ActionLink to="/apps/new">Create application</ActionLink> : undefined} />}</section></div>
    {state.session!.user.serverAdmin && <AdminStatus />}
    {canCreate && <section className="panel"><h2>Start from a file</h2><p>Upload one file to create a new application and Draft release, then choose whether to publish.</p><ActionLink to="/upload/new-app" secondary>Upload a new application</ActionLink></section>}
  </>
}

export function ApplicationsPage() {
  const { state } = useAuth()
  const [params, setParams] = useSearchParams()
  const query = params.get('q') || ''
  const cursor = params.get('cursor') || ''
  const [draft, setDraft] = useState(query)
  const [previous, setPrevious] = useState<string[]>([])
  useEffect(() => setDraft(query), [query])
  const apps = useResource(`apps:${query}:${cursor}`, signal => {
    const search = new URLSearchParams({ pageSize: '10' })
    if (query) search.set('q', query)
    if (cursor) search.set('cursor', cursor)
    return apiRequest<AppPage>(`/apps?${search}`, { signal, authenticated: true }).then(result => result.value)
  })
  function search(event: FormEvent) { event.preventDefault(); setPrevious([]); setParams(draft.trim() ? { q: draft.trim() } : {}) }
  function next() { if (!apps.value?.nextCursor) return; setPrevious(value => [...value, cursor]); setParams({ ...(query ? { q: query } : {}), cursor: apps.value.nextCursor! }) }
  function back() { const stack = [...previous]; const target = stack.pop() || ''; setPrevious(stack); setParams({ ...(query ? { q: query } : {}), ...(target ? { cursor: target } : {}) }) }
  const canCreate = state.session!.user.publishPermission || state.session!.user.serverAdmin
  return <><PageHeader title="Applications" description="Browse the applications you are allowed to see." actions={canCreate ? <ActionLink to="/apps/new">Create application</ActionLink> : undefined} />
    <div className="toolbar"><form role="search" onSubmit={search}><label htmlFor="app-search">Search applications</label><div className="search-row"><input id="app-search" value={draft} onChange={event => setDraft(event.target.value)} placeholder="Name or slug" /><button className="button button-secondary">Search</button></div></form>{canCreate && <Link to="/upload/new-app">Upload a new application</Link>}</div>
    {apps.loading ? <LoadingState label="Loading applications…" /> : apps.error ? <ProblemNotice error={apps.error} retry={apps.retry} /> : apps.value!.items.length ? <><div className="table-wrap"><table><thead><tr><th>Application</th><th>State</th><th>Visibility</th><th><span className="sr-only">Open</span></th></tr></thead><tbody>{apps.value!.items.map(app => <tr key={app.id}><td><Link className="row-title" to={`/apps/${app.id}`}>{app.name}</Link><small>{app.slug}</small></td><td><StatusBadge value={app.state} /></td><td><StatusBadge value={app.visibility} /></td><td><Link to={`/apps/${app.id}`} aria-label={`Open ${app.name}`}>Open →</Link></td></tr>)}</tbody></table></div><div className="pagination"><button className="button button-secondary" onClick={back} disabled={!cursor}>Previous page</button><button className="button button-secondary" onClick={next} disabled={!apps.value!.nextCursor}>Next page</button></div></> : <EmptyState title={query ? 'No matching applications' : 'No applications yet'} detail={query ? 'Try another name or slug.' : 'Create a Draft application without uploading an artifact.'} action={canCreate && !query ? <ActionLink to="/apps/new">Create application</ActionLink> : undefined} />}
  </>
}

export function CreateApplicationPage() {
  const { state } = useAuth()
  const navigate = useNavigate()
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const attempt = useRef<{ name: string; key: string } | null>(null)
  const canCreate = state.session!.user.publishPermission || state.session!.user.serverAdmin
  async function submit(event: FormEvent) {
    event.preventDefault()
    const value = name.trim()
    if (!value) { setError(new Error('Enter an application name.')); return }
    if (!attempt.current || attempt.current.name !== value) attempt.current = { name: value, key: idempotencyKey() }
    setBusy(true); setError(undefined)
    try {
      const result = await apiRequest<DepotApp>('/apps', { method: 'POST', headers: { 'Content-Type': 'application/json' }, csrf: state.session!.csrfToken,
        idempotencyKey: attempt.current.key, body: JSON.stringify({ name: value }), authenticated: true })
      navigate(`/apps/${result.value.id}`, { replace: true })
    } catch (failure) { setError(failure) }
    finally { setBusy(false) }
  }
  return <><PageHeader title="Create application" description="Start with a private Draft. An artifact is not required yet." />
    {!canCreate ? <ProblemNotice error={new Error('Publish permission is required to create an application.')} /> : <section className="panel form-panel">{error !== undefined && <ProblemNotice error={error} />}<form className="form-stack" onSubmit={submit}><label>Application name<input value={name} onChange={event => { setName(event.target.value); setError(undefined) }} required maxLength={255} autoFocus /></label><p className="subtle">The Server assigns the application ID and slug. You can add a release later.</p><div className="form-actions"><button className="button button-primary" disabled={busy || !name.trim()}>{busy ? 'Creating…' : 'Create Draft application'}</button><ActionLink to="/apps" secondary>Cancel</ActionLink></div></form></section>}
  </>
}

export function ApplicationOverview() {
  const app = useOutletContext<DepotApp>()
  const { state } = useAuth()
  const canManage = state.session!.user.serverAdmin || state.session!.user.id === app.ownerId
  const releases = useResource(`releases-overview:${app.id}`, signal => apiRequest<ReleasePage>(`/apps/${app.id}/releases?pageSize=5`, { signal, authenticated: true }).then(result => result.value))
  return <><PageHeader title={app.name} description="Application overview" actions={canManage && !!releases.value?.items.length ? <ActionLink to={`/apps/${app.id}/uploads/new`}>Add release from file</ActionLink> : undefined} />
    <section className="panel"><h2>Publication and access</h2><AppIdentity app={app} /><p>A Published release can still belong to a Private application. Visibility and release state are separate.</p><TechnicalDetails><dl className="facts"><div><dt>Application ID</dt><dd><code>{app.id}</code></dd></div><div><dt>Slug</dt><dd><code>{app.slug}</code></dd></div><div><dt>Owner ID</dt><dd><code>{app.ownerId}</code></dd></div></dl></TechnicalDetails></section>
    <section className="panel"><div className="panel-heading"><h2>Releases</h2><Link to={`/apps/${app.id}/releases`}>View all →</Link></div>{releases.loading ? <LoadingState label="Loading releases…" /> : releases.error ? <ProblemNotice error={releases.error} retry={releases.retry} /> : releases.value!.items.length ? <ul className="compact-list">{releases.value!.items.map(release => <li key={release.id}><span>{release.version || 'Unversioned release'}</span><StatusBadge value={release.state} /></li>)}</ul> : <EmptyState title="No releases yet" detail={canManage ? 'Add a file to create a Draft release in this application.' : 'This application has no releases.'} action={canManage ? <ActionLink to={`/apps/${app.id}/uploads/new`}>Add release from file</ActionLink> : undefined} />}</section>
  </>
}

export function ApplicationReleases() {
  const app = useOutletContext<DepotApp>()
  const { state } = useAuth()
  const canManage = state.session!.user.serverAdmin || state.session!.user.id === app.ownerId
  const [cursor, setCursor] = useState('')
  const releases = useResource(`releases:${app.id}:${cursor}`, signal => apiRequest<ReleasePage>(`/apps/${app.id}/releases?pageSize=10${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`, { signal, authenticated: true }).then(result => result.value))
  return <><PageHeader title="Releases" description={`Draft and Published releases for ${app.name}.`} actions={canManage && !!releases.value?.items.length ? <ActionLink to={`/apps/${app.id}/uploads/new`}>Add release from file</ActionLink> : undefined} />
    {releases.loading ? <LoadingState label="Loading releases…" /> : releases.error ? <ProblemNotice error={releases.error} retry={releases.retry} /> : releases.value!.items.length ? <><div className="table-wrap"><table><thead><tr><th>Version</th><th>State</th><th>Release ID</th></tr></thead><tbody>{releases.value!.items.map(release => <tr key={release.id}><td>{release.version || 'Unversioned release'}</td><td><StatusBadge value={release.state} /></td><td><code className="muted-code">{release.id}</code></td></tr>)}</tbody></table></div><div className="pagination"><button className="button button-secondary" onClick={() => setCursor('')} disabled={!cursor}>First page</button><button className="button button-secondary" onClick={() => setCursor(releases.value!.nextCursor!)} disabled={!releases.value!.nextCursor}>Next page</button></div></> : <EmptyState title="No releases yet" detail={canManage ? 'Upload a file to create a Draft release in this application.' : 'This application has no releases.'} action={canManage ? <ActionLink to={`/apps/${app.id}/uploads/new`}>Add release from file</ActionLink> : undefined} />}
  </>
}

type UploadKeys = { fingerprint: string; create: string; finalize: string; publish: string }

function UploadForm({ app }: { app?: DepotApp }) {
  const { state } = useAuth()
  const [name, setName] = useState('')
  const [file, setFile] = useState<File | null>(null)
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState('')
  const [error, setError] = useState<unknown>()
  const [resultAppId, setResultAppId] = useState<string>()
  const keys = useRef<UploadKeys | null>(null)
  const canCreate = state.session!.user.publishPermission || state.session!.user.serverAdmin
  const canManage = !app || state.session!.user.serverAdmin || state.session!.user.id === app.ownerId
  const canPublish = state.session!.user.publishPermission || state.session!.user.serverAdmin
  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!file || (!app && !name.trim())) return
    const publishNow = ((event.nativeEvent as SubmitEvent).submitter as HTMLButtonElement | null)?.value === 'publish'
    const csrf = state.session!.csrfToken
    setBusy(true); setError(undefined); setResultAppId(undefined); setMessage('Hashing file')
    try {
      const digest = await fileSha256(file)
      const fingerprint = `${app?.id || name.trim()}|${file.name}|${file.size}|${digest}`
      if (!keys.current || keys.current.fingerprint !== fingerprint) keys.current = { fingerprint, create: idempotencyKey(), finalize: idempotencyKey(), publish: idempotencyKey() }
      const currentKeys = keys.current
      setMessage('Creating upload session')
      const created = await apiRequest<UploadSession>('/uploads', { method: 'POST', authenticated: true, csrf, idempotencyKey: currentKeys.create,
        headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ mode: 'BareFile', target: app ? { appId: app.id, createRelease: true } : { newAppName: name.trim() }, file: { size: file.size, sha256: digest } }) })
      const upload = created.value
      for (let start = 0; start < file.size; start += 4 * 1024 * 1024) {
        const end = Math.min(start + 4 * 1024 * 1024, file.size)
        await apiRequest<void>(`/uploads/${upload.id}/files/${upload.file.fileId}`, { method: 'PUT', authenticated: true, csrf,
          headers: { 'Content-Type': 'application/octet-stream', 'Content-Range': `bytes ${start}-${end - 1}/${file.size}` }, body: file.slice(start, end) })
        setMessage(`Received ${end.toLocaleString()} of ${file.size.toLocaleString()} bytes`)
      }
      setMessage('Finalizing upload')
      let result = (await apiRequest<FinalizationResult>(`/uploads/${upload.id}/finalize`, { method: 'POST', authenticated: true, csrf, idempotencyKey: currentKeys.finalize })).value
      for (let attempt = 0; result.phase !== 'Completed' && result.phase !== 'Failed' && result.phase !== 'Cancelled' && attempt < 600; attempt++) {
        await new Promise(resolve => setTimeout(resolve, 1000))
        result = (await apiRequest<FinalizationResult>(`/uploads/${upload.id}/result`, { authenticated: true })).value
      }
      if (result.phase !== 'Completed' || !result.releaseId) throw new Error(result.problem?.title || 'Finalization did not complete')
      if (publishNow) {
        setMessage('Publishing release')
        const release = await apiRequest(`/releases/${result.releaseId}`, { authenticated: true })
        const etag = release.response.headers.get('ETag')
        if (!etag) throw new Error('The release has no version validator. Refresh before publishing.')
        await apiRequest(`/releases/${result.releaseId}/publish`, { method: 'POST', authenticated: true, csrf, idempotencyKey: currentKeys.publish,
          ifMatch: etag })
      }
      setResultAppId(result.appId || app?.id)
      setMessage(`${publishNow ? 'Published' : 'Saved Draft for'} ${app?.name || name.trim()}`)
    } catch (failure) { setError(failure) }
    finally { setBusy(false) }
  }
  return <section className="panel form-panel"><p>{app ? `The file will create a new Draft release in ${app.name}.` : 'The file will create a new application and an implicit Draft release.'} Publication is a separate action.</p>
    {!app && !canCreate ? <ProblemNotice error={new Error('Publish permission is required to create a new application.')} /> : !canManage ? <ProblemNotice error={new Error('You do not have permission to manage this application.')} /> : <><form className="form-stack" onSubmit={submit}>
      {!app && <label>App name<input value={name} onChange={event => { setName(event.target.value); setResultAppId(undefined) }} required /></label>}
      <label>File<input type="file" onChange={event => { setFile(event.target.files?.[0] || null); setResultAppId(undefined) }} required /></label>
      <div className="form-actions"><button className="button button-secondary" value="draft" disabled={busy || !!resultAppId || !file || (!app && !name.trim())}>Save Draft</button>{canPublish && <button className="button button-primary" value="publish" disabled={busy || !!resultAppId || !file || (!app && !name.trim())}>Upload and publish</button>}</div>
    </form>{message && <p role="status" className="upload-progress">{message}</p>}{error !== undefined && <ProblemNotice error={error} />}{resultAppId && <ActionLink to={`/apps/${resultAppId}`} secondary>Open application</ActionLink>}</>}
  </section>
}

export function UploadNewAppPage() { return <><PageHeader title="Upload a new application" description="Create an application from one file, then save a Draft or publish it." /><UploadForm /></> }
export function UploadExistingAppPage() { const app = useOutletContext<DepotApp>(); return <><PageHeader title="Add release from file" description={`Upload one file to ${app.name}.`} /><UploadForm app={app} /></> }

export function UnavailablePage() { return <><PageHeader title="Page not found" /><EmptyState title="This page is unavailable" detail="Open an implemented portal area from the navigation." action={<ActionLink to="/apps">Applications</ActionLink>} /></> }
