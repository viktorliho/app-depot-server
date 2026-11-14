import { useEffect, useId, useRef, useState, type FormEvent } from 'react'
import { Link, useNavigate, useOutletContext, useParams, useSearchParams } from 'react-router'
import { apiRequest, idempotencyKey, jsonBody, resource, type Artifact, type Association, type Channel, type ChannelMove,
  type Declaration, type DeclarationRevision, type DeclarationSnapshot, type DepotApp, type Operation,
  type Page, type Release } from './api'
import { ActionLink, EmptyState, LoadingState, PageHeader, ProblemNotice, StatusBadge, TechnicalDetails } from './components'
import { useAuth } from './portal'
import { useResource } from './use-resource'
import { StandardMetadataForm, supportsStandardFields, type MetadataScope } from './metadata'

const enc = encodeURIComponent
const canManage = (app: DepotApp, user: { id: string; serverAdmin: boolean }) => user.serverAdmin || user.id === app.ownerId

function Notice({ text }: { text: string }) { return <p className="success-notice" role="status">{text}</p> }

export function ConfirmDialog({ label, detail, confirm, onConfirm }: { label: string; detail: string; confirm: string; onConfirm: () => Promise<void> }) {
  const dialog = useRef<HTMLDialogElement>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  async function run() {
    setBusy(true); setError(undefined)
    try { await onConfirm(); dialog.current?.close() } catch (failure) { setError(failure) }
    finally { setBusy(false) }
  }
  return <><button className="button button-danger" onClick={() => dialog.current?.showModal()}>{label}</button>
    <dialog ref={dialog} onClose={() => setError(undefined)} aria-label={label}><h2>{label}</h2><p>{detail}</p>{error !== undefined && <ProblemNotice error={error} />}
      <div className="form-actions"><button className="button button-secondary" onClick={() => dialog.current?.close()} disabled={busy}>Cancel</button><button className="button button-danger" onClick={() => void run()} disabled={busy}>{busy ? 'Working…' : confirm}</button></div>
    </dialog></>
}

export function OperationNotice({ id, compact = false }: { id: string; compact?: boolean }) {
  const operation = useResource(`operation:${id}`, signal => apiRequest<Operation>(`/operations/${id}`, { authenticated: true, signal }).then(result => result.value))
  const polls = useRef(0)
  useEffect(() => { polls.current = 0 }, [id])
  useEffect(() => {
    if (!operation.value || operation.value.status === 'Completed' || operation.value.status === 'Failed' || polls.current >= 120) return
    const timer = setTimeout(() => { polls.current++; operation.retry() }, 1000)
    return () => clearTimeout(timer)
  }, [operation.value?.status, operation.value?.id, operation.retry])
  if (operation.loading) return <LoadingState label="Checking operation…" />
  if (operation.error) return <ProblemNotice error={operation.error} retry={operation.retry} />
  const value = operation.value!
  const labels: Record<Operation['kind'], string> = { Finalize: 'Finalize upload', CancelUpload: 'Cancel upload', DeleteApp: 'Delete application',
    DeleteRelease: 'Delete release', DeleteAssociation: 'Remove artifact association', Backup: 'Backup' }
  return <div className="panel" role="status"><h2>{labels[value.kind]} operation</h2><StatusBadge value={value.status} />
    {value.kind.startsWith('Delete') && <p>{value.accessRevoked ? 'Access has been revoked.' : 'Access revocation is not yet confirmed.'} Physical cleanup: {value.physicalCleanup || 'Not started'}.</p>}
    {value.kind === 'Backup' && <p>Backup output available now: {value.backupOutputAvailable ? 'Yes' : 'No'}. A historically Completed operation may have unavailable output after restore.</p>}
    {value.kind === 'Backup' && value.backupId && value.backupOutputAvailable && <ActionLink to={`/admin/backups?backup=${value.backupId}`} secondary>Inspect backup</ActionLink>}
    {value.kind === 'Finalize' && value.result?.phase === 'Completed' && value.result.appId && value.result.releaseId && <ActionLink to={`/apps/${value.result.appId}/releases/${value.result.releaseId}`} secondary>Open registered Release</ActionLink>}
    {value.status === 'Failed' && <p>{value.problem?.title || 'The operation failed.'}{value.problem?.detail ? ` ${value.problem.detail}` : ''}</p>}
    {polls.current >= 120 && value.status !== 'Completed' && value.status !== 'Failed' && <button className="button button-secondary" onClick={() => { polls.current = 0; operation.retry() }}>Check again</button>}
    {!compact && <Link to={`/operations/${id}`}>Open operation page</Link>}
    <TechnicalDetails><code>{value.id}</code></TechnicalDetails></div>
}

export function OperationPage() {
  const { id } = useParams()
  return <><PageHeader title="Operation status" description="The Server reports this known operation ID. There is no global operation list." /><OperationNotice id={id!} compact /></>
}

export function AppSettings() {
  const app = useOutletContext<DepotApp>()
  const { state } = useAuth()
  const navigate = useNavigate()
  const current = useResource(`app-settings:${app.id}`, signal => resource<DepotApp>(`/apps/${app.id}`, signal))
  const [name, setName] = useState(app.name)
  const [slug, setSlug] = useState(app.slug)
  const [visibility, setVisibility] = useState(app.visibility)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const [message, setMessage] = useState('')
  const deleteKey = useRef(idempotencyKey())
  useEffect(() => { if (current.value) { setName(current.value.value.name); setSlug(current.value.value.slug); setVisibility(current.value.value.visibility) } }, [current.value])
  const allowed = canManage(app, state.session!.user)
  async function save(event: FormEvent) {
    event.preventDefault(); setBusy(true); setError(undefined); setMessage('')
    try {
      await apiRequest(`/apps/${app.id}`, { method: 'PATCH', csrf: state.session!.csrfToken, ifMatch: current.value!.etag,
        authenticated: true, ...jsonBody({ name: name.trim(), slug: slug.trim(), visibility }) })
      current.retry(); setMessage('Application settings saved. Reload to update the workspace name.')
    } catch (failure) { setError(failure) } finally { setBusy(false) }
  }
  async function remove() {
    const operation = await apiRequest<Operation>(`/apps/${app.id}`, { method: 'DELETE', csrf: state.session!.csrfToken, authenticated: true,
      ifMatch: current.value!.etag, idempotencyKey: deleteKey.current })
    navigate(`/apps?operation=${operation.value.id}`, { replace: true })
  }
  return <><PageHeader title="Application settings" description={app.name} />
    {current.loading ? <LoadingState /> : current.error ? <ProblemNotice error={current.error} retry={current.retry} /> : <>
      <section className="panel form-panel"><h2>Identity and visibility</h2><p>Visibility controls catalog access. It does not publish Draft releases.</p>
        {!allowed ? <p>You can view this application but cannot change its settings.</p> : <form className="form-stack" onSubmit={save}>
          <label>Name<input value={name} onChange={event => setName(event.target.value)} required /></label>
          <label>Slug<input value={slug} onChange={event => setSlug(event.target.value)} required /></label>
          <label>Visibility<select value={visibility} onChange={event => setVisibility(event.target.value as DepotApp['visibility'])}><option>Private</option><option>Public</option></select></label>
          {error !== undefined && <ProblemNotice error={error} />}{message && <Notice text={message} />}
          <button className="button button-primary" disabled={busy || !name.trim() || !slug.trim()}>{busy ? 'Saving…' : 'Save changes'}</button>
        </form>}</section>
      {allowed && <section className="panel"><h2>Permanent deletion</h2><p>Deleting this application immediately revokes access to it and its releases. Cleanup of unreferenced content may continue asynchronously. This cannot be undone.</p>
        <ConfirmDialog label="Delete application" detail={`Permanently delete ${app.name} and its release associations? Access is revoked immediately; physical cleanup may continue.`} confirm="Delete application" onConfirm={remove} /></section>}
    </>}
  </>
}

export function NewRelease() {
  const app = useOutletContext<DepotApp>()
  const { state } = useAuth()
  const navigate = useNavigate()
  const [version, setVersion] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const attempt = useRef<{ version: string; key: string } | null>(null)
  const allowed = canManage(app, state.session!.user)
  async function create(event: FormEvent) {
    event.preventDefault()
    if (!attempt.current || attempt.current.version !== version) attempt.current = { version, key: idempotencyKey() }
    setBusy(true); setError(undefined)
    try {
      const result = await apiRequest<Release>(`/apps/${app.id}/releases`, { method: 'POST', authenticated: true,
        csrf: state.session!.csrfToken, idempotencyKey: attempt.current.key, ...jsonBody({ version: version.trim() || null }) })
      navigate(`/apps/${app.id}/releases/${result.value.id}`, { replace: true })
    } catch (failure) { setError(failure) } finally { setBusy(false) }
  }
  return <><PageHeader title="Create Draft release" description={`Add a release to ${app.name}. A version and artifact are optional until publication.`} />
    {!allowed ? <ProblemNotice error={new Error('Only the owner or Server Admin can create a release here.')} /> : <section className="panel form-panel"><form className="form-stack" onSubmit={create}>
      <label>Version (optional)<input value={version} onChange={event => setVersion(event.target.value)} /></label>
      {error !== undefined && <ProblemNotice error={error} />}
      <button className="button button-primary" disabled={busy}>{busy ? 'Creating…' : 'Create Draft release'}</button>
    </form></section>}
  </>
}

export function DeclarationEditor({ path, editable, title, scope }: { path: string; editable: boolean; title: string; scope: MetadataScope }) {
  const { state } = useAuth()
  const current = useResource(`declaration:${path}`, signal => resource<Declaration>(path, signal))
  const [text, setText] = useState('')
  const [textRevision, setTextRevision] = useState('')
  const [error, setError] = useState<unknown>()
  const [message, setMessage] = useState('')
  const [busy, setBusy] = useState(false)
  const [awaitingRevision, setAwaitingRevision] = useState('')
  const [cursor, setCursor] = useState('')
  const [historyOpen, setHistoryOpen] = useState(false)
  const revisions = useResource(`revisions:${path}:${cursor}:${historyOpen}:${message}`, signal => historyOpen
    ? apiRequest<Page<DeclarationRevision>>(`${path}/revisions?pageSize=10${cursor ? `&cursor=${enc(cursor)}` : ''}`, { authenticated: true, signal }).then(result => result.value)
    : Promise.resolve({ items: [], nextCursor: null }))
  const [selected, setSelected] = useState<DeclarationRevision>()
  const [editor, setEditor] = useState<'json' | 'standard'>('standard')
  useEffect(() => { if (current.value) {
    const value = JSON.stringify(current.value.value.data, null, 2)
    setText(value); setTextRevision(current.value.value.revisionId)
    if (!supportsStandardFields(value, scope)) setEditor('json')
    if (awaitingRevision === current.value.value.revisionId) {
      setAwaitingRevision(''); setMessage('Saved as a new declaration revision.')
    }
  } }, [current.value?.value.revisionId, scope, awaitingRevision])
  async function put(data: unknown) {
    setBusy(true); setError(undefined); setMessage('')
    try {
      const saved = await apiRequest<Declaration>(path, { method: 'PUT', authenticated: true, csrf: state.session!.csrfToken,
        ifMatch: current.value!.etag, ...jsonBody({ data }) })
      setAwaitingRevision(saved.value.revisionId); current.retry(); revisions.retry()
    } catch (failure) { setError(failure) } finally { setBusy(false) }
  }
  function save(event: FormEvent) {
    event.preventDefault()
    try { void put(JSON.parse(text)) } catch { setError(new Error('Enter valid JSON. The value null is allowed.')) }
  }
  async function inspect(id: string) {
    try { setSelected((await apiRequest<DeclarationRevision>(`${path}/revisions/${id}`, { authenticated: true })).value) }
    catch (failure) { setError(failure) }
  }
  return <section className="panel"><h2>{title}</h2>
    {current.loading ? <LoadingState /> : current.error ? <ProblemNotice error={current.error} retry={current.retry} /> : awaitingRevision ? <LoadingState label="Loading saved revision…" /> : <>
      <p className="subtle">Standard fields are optional. Advanced JSON supports every valid value, including <code>null</code>; an explicit null differs from absent data.</p>
      {!supportsStandardFields(textRevision === current.value!.value.revisionId ? text : JSON.stringify(current.value!.value.data, null, 2), scope) && <p role="status">This declaration cannot be edited as standard fields. Its value is preserved in Advanced JSON.</p>}
      <div className="form-actions section-action"><button className="button button-secondary" aria-pressed={editor === 'standard'} disabled={!supportsStandardFields(textRevision === current.value!.value.revisionId ? text : JSON.stringify(current.value!.value.data, null, 2), scope)} onClick={() => setEditor('standard')}>Standard fields</button>
        <button className="button button-secondary" aria-pressed={editor === 'json'} onClick={() => setEditor('json')}>Advanced JSON</button></div>
      <div hidden={editor !== 'standard'}><StandardMetadataForm key={current.value!.value.revisionId} scope={scope}
        documentText={textRevision === current.value!.value.revisionId ? text : JSON.stringify(current.value!.value.data, null, 2)}
        editable={editable} busy={busy} onSave={put} /></div>
      <div hidden={editor !== 'json'}><form className="form-stack" onSubmit={save}><label>Current JSON value<textarea rows={8} className="json-editor" value={text} onChange={event => setText(event.target.value)} readOnly={!editable} spellCheck={false} /></label>
          {editable && <button className="button button-primary" disabled={busy}>{busy ? 'Saving…' : 'Save new revision'}</button>}</form></div>
      {error !== undefined && <ProblemNotice error={error} />}{message && <Notice text={message} />}
      <TechnicalDetails><code>Current revision: {current.value!.value.revisionId}</code></TechnicalDetails>
      <button className="button button-secondary section-action" onClick={() => setHistoryOpen(value => !value)}>{historyOpen ? 'Hide revision history' : 'Show revision history'}</button>
      {historyOpen && (revisions.loading ? <LoadingState /> : revisions.error ? <ProblemNotice error={revisions.error} retry={revisions.retry} /> : <>
        <ul className="compact-list">{revisions.value!.items.map(revision => <li key={revision.id}><span>{new Date(revision.createdAt).toLocaleString()}</span><button className="text-button" onClick={() => void inspect(revision.id)}>Inspect revision</button></li>)}</ul>
        {revisions.value!.nextCursor && <button className="button button-secondary" onClick={() => setCursor(revisions.value!.nextCursor!)}>More revisions</button>}
      </>)}
      {selected && <div className="history-preview"><h3>Previous value</h3><pre>{JSON.stringify(selected.data, null, 2)}</pre><TechnicalDetails><code>{selected.id}</code></TechnicalDetails>
        {editable && <button className="button button-secondary" disabled={busy} onClick={() => void put(selected.data)}>Restore as new revision</button>}</div>}
    </>}
  </section>
}

export function AppDeclarations() {
  const app = useOutletContext<DepotApp>()
  const { state } = useAuth()
  return <><PageHeader title="Application declarations" description={`Optional JSON data for ${app.name}.`} />
    <DeclarationEditor title="Application data" scope="app" path={`/apps/${app.id}/declarations`} editable={canManage(app, state.session!.user)} /></>
}

function ArtifactRow({ appId, association, editable, removable, onRemoved }: { appId: string; association: Association; editable: boolean; removable: boolean; onRemoved: (id: string) => void }) {
  const artifact = useResource(`artifact:${association.artifactId}`, signal => apiRequest<Artifact>(`/artifacts/${enc(association.artifactId)}`, { authenticated: true, signal }).then(result => result.value))
  const current = useResource(`association:${association.id}`, signal => resource<Association>(`/associations/${association.id}`, signal))
  const { state } = useAuth()
  const deleteKey = useRef(idempotencyKey())
  async function remove() {
    const result = await apiRequest<Operation>(`/associations/${association.id}`, { method: 'DELETE', authenticated: true,
      csrf: state.session!.csrfToken, ifMatch: current.value!.etag, idempotencyKey: deleteKey.current })
    onRemoved(result.value.id)
  }
  return <section className="panel"><h3>Artifact association</h3>
    {artifact.loading ? <LoadingState /> : artifact.error ? <ProblemNotice error={artifact.error} retry={artifact.retry} /> : <>
      <div className="identity-line"><StatusBadge value={artifact.value!.kind} />{artifact.value!.size != null && <span>{artifact.value!.size.toLocaleString()} bytes</span>}</div>
      <Link className="button button-primary" to={`/apps/${appId}/releases/${association.releaseId}/artifacts/${association.id}`}>Browse files and entry points</Link>
      <TechnicalDetails><dl className="facts"><div><dt>Artifact ID</dt><dd><code>{artifact.value!.id}</code></dd></div><div><dt>Association ID</dt><dd><code>{association.id}</code></dd></div></dl></TechnicalDetails>
      {removable && current.value && <div className="section-action"><ConfirmDialog label="Remove association" detail="Remove this Artifact from the Draft release? This revokes the association; immutable content may remain shared elsewhere." confirm="Remove association" onConfirm={remove} /></div>}
    </>}
    <DeclarationEditor title="Association declarations" scope="association" path={`/associations/${association.id}/declarations`} editable={editable} />
  </section>
}

export function ReleaseDetail() {
  const app = useOutletContext<DepotApp>()
  const { releaseId } = useParams()
  const { state } = useAuth()
  const navigate = useNavigate()
  const current = useResource(`release:${releaseId}`, signal => resource<Release>(`/releases/${releaseId}`, signal))
  const [cursor, setCursor] = useState('')
  const [refresh, setRefresh] = useState(0)
  const associations = useResource(`associations:${releaseId}:${cursor}:${refresh}`, signal => apiRequest<Page<Association>>(`/releases/${releaseId}/artifacts?pageSize=10${cursor ? `&cursor=${enc(cursor)}` : ''}`, { authenticated: true, signal }).then(result => result.value))
  const [version, setVersion] = useState('')
  const [artifactId, setArtifactId] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const [notice, setNotice] = useState('')
  const [operation, setOperation] = useState('')
  const attempt = useRef<{ id: string; key: string } | null>(null)
  const publishKey = useRef(idempotencyKey())
  const deleteKey = useRef(idempotencyKey())
  const snapshot = useResource(`snapshot:${releaseId}:${refresh}`, signal => apiRequest<DeclarationSnapshot>(`/releases/${releaseId}/declaration-snapshot`, { authenticated: true, signal }).then(result => result.value))
  const allowed = canManage(app, state.session!.user)
  const canPublish = state.session!.user.publishPermission || state.session!.user.serverAdmin
  const release = current.value?.value
  const mutable = allowed && release?.state === 'Draft'
  useEffect(() => { if (release) setVersion(release.version || '') }, [release?.id, release?.version])
  async function action(run: () => Promise<void>) {
    setBusy(true); setError(undefined); setNotice('')
    try { await run() } catch (failure) { setError(failure) } finally { setBusy(false) }
  }
  async function updateVersion(event: FormEvent) { event.preventDefault(); await action(async () => {
    await apiRequest(`/releases/${releaseId}`, { method: 'PATCH', authenticated: true, csrf: state.session!.csrfToken,
      ifMatch: current.value!.etag, ...jsonBody({ version: version.trim() || null }) }); current.retry(); setNotice('Release version saved.')
  }) }
  async function associate(event: FormEvent) { event.preventDefault(); await action(async () => {
    const id = artifactId.trim()
    if (!attempt.current || attempt.current.id !== id) attempt.current = { id, key: idempotencyKey() }
    await apiRequest(`/releases/${releaseId}/artifacts`, { method: 'POST', authenticated: true, csrf: state.session!.csrfToken,
      idempotencyKey: attempt.current.key, ...jsonBody({ artifactId: id }) })
    setArtifactId(''); setCursor(''); setRefresh(value => value + 1); current.retry(); setNotice('Existing Artifact associated with this Draft release.')
  }) }
  async function publish() { await action(async () => {
    await apiRequest(`/releases/${releaseId}/publish`, { method: 'POST', authenticated: true, csrf: state.session!.csrfToken,
      ifMatch: current.value!.etag, idempotencyKey: publishKey.current }); current.retry(); setNotice('Release published. Artifact composition is now immutable.')
  }) }
  async function remove() {
    const result = await apiRequest<Operation>(`/releases/${releaseId}`, { method: 'DELETE', authenticated: true,
      csrf: state.session!.csrfToken, ifMatch: current.value!.etag, idempotencyKey: deleteKey.current })
    navigate(`/apps/${app.id}/releases?operation=${result.value.id}`, { replace: true })
  }
  if (current.loading) return <LoadingState label="Loading release…" />
  if (current.error) return <><PageHeader title="Release unavailable" /><ProblemNotice error={current.error} retry={current.retry} /></>
  if (release!.appId !== app.id) return <ProblemNotice error={new Error('This release does not belong to the selected application.')} />
  return <><PageHeader title={release!.version || 'Unversioned release'} description="Release details" actions={mutable ? <ActionLink to={`/apps/${app.id}/releases/${releaseId}/uploads/new`}>Upload Artifact</ActionLink> : undefined} />
    <section className="panel"><div className="identity-line"><StatusBadge value={release!.state} /><StatusBadge value={app.visibility} /></div>
      <p>Publication fixes the Artifact composition. Application visibility controls whether Published content is public.</p>
      <TechnicalDetails><code>Release ID: {releaseId}</code></TechnicalDetails></section>
    {error !== undefined && <ProblemNotice error={error} />}{notice && <Notice text={notice} />}{operation && <OperationNotice id={operation} />}
    {mutable && <section className="panel form-panel"><h2>Draft release</h2><form className="form-stack" onSubmit={updateVersion}>
      <label>Version (optional)<input value={version} onChange={event => setVersion(event.target.value)} /></label><button className="button button-secondary" disabled={busy}>Save version</button></form>
      <div className="form-actions section-action"><ActionLink to={`/apps/${app.id}/releases/${releaseId}/uploads/new`} secondary>Upload Artifact</ActionLink>
        {canPublish && <button className="button button-primary" onClick={() => void publish()} disabled={busy}>Publish release</button>}</div>
      {!canPublish && <p className="subtle">Current Publish permission is required to publish.</p>}</section>}
    <DeclarationEditor title="Release declarations" scope="release" path={`/releases/${releaseId}/declarations`} editable={allowed} />
    <section className="panel"><h2>Contents / Artifacts</h2><p>Browse original files, download content and manage entry points for each Artifact.</p>{associations.loading ? <LoadingState /> : associations.error ? <ProblemNotice error={associations.error} retry={associations.retry} /> : <>
      {!associations.value!.items.length && <EmptyState title="No Artifacts associated" detail="Upload or associate an Artifact before publishing." />}
      {associations.value!.items.length === 1 && !associations.value!.nextCursor && <p><Link className="button button-primary" to={`/apps/${app.id}/releases/${releaseId}/artifacts/${associations.value!.items[0].id}`}>Explore content</Link></p>}
      {associations.value!.items.map(item => <ArtifactRow key={item.id} appId={app.id} association={item} editable={allowed} removable={!!mutable} onRemoved={id => { setOperation(id); setRefresh(value => value + 1); current.retry() }} />)}
      {associations.value!.nextCursor && <button className="button button-secondary" onClick={() => setCursor(associations.value!.nextCursor!)}>Next Artifacts page</button>}
    </>}</section>
    {mutable && <section className="panel form-panel"><h2>Associate an existing Artifact</h2><p>Enter the identity of an Artifact already accessible to your account. Registration does not publish the Release.</p>
      <form className="form-stack" onSubmit={associate}><label>Artifact ID<input value={artifactId} onChange={event => setArtifactId(event.target.value)} required placeholder="adfile:sha256:… or adtree:v1:sha256:…" /></label><button className="button button-secondary" disabled={busy || !artifactId.trim()}>Associate Artifact</button></form></section>}
    <section className="panel"><h2>Combined declaration snapshot</h2>{snapshot.loading ? <LoadingState /> : snapshot.error ? <ProblemNotice error={snapshot.error} retry={snapshot.retry} /> : <pre className="json-preview">{JSON.stringify(snapshot.value, null, 2)}</pre>}</section>
    {allowed && <section className="panel"><h2>Permanent deletion</h2><p>Deleting this Release revokes access and starts asynchronous cleanup. This cannot be undone.</p><ConfirmDialog label="Delete release" detail="Permanently delete this Release and its Artifact associations? Shared immutable content may remain available through other releases." confirm="Delete release" onConfirm={remove} /></section>}
  </>
}

export function ChannelsPage() {
  const app = useOutletContext<DepotApp>()
  const { state } = useAuth()
  const allowed = canManage(app, state.session!.user)
  const [cursor, setCursor] = useState('')
  const [revision, setRevision] = useState(0)
  const channels = useResource(`channels:${app.id}:${cursor}:${revision}`, signal => apiRequest<Page<Channel>>(`/apps/${app.id}/channels?pageSize=20${cursor ? `&cursor=${enc(cursor)}` : ''}`, { authenticated: true, signal }).then(result => result.value))
  const [name, setName] = useState('')
  const [releaseId, setReleaseId] = useState('')
  const [error, setError] = useState<unknown>()
  const [message, setMessage] = useState('')
  const [busy, setBusy] = useState(false)
  async function create(event: FormEvent) {
    event.preventDefault(); setBusy(true); setError(undefined); setMessage('')
    try {
      if (!name.trim() || name.includes('/')) throw new Error('Channel name must be one nonempty path segment.')
      await apiRequest(`/apps/${app.id}/channels/${enc(name.trim())}`, { method: 'PUT', authenticated: true, csrf: state.session!.csrfToken,
        headers: { 'Content-Type': 'application/json', 'If-None-Match': '*' }, body: JSON.stringify({ releaseId: releaseId.trim() }) })
      setName(''); setReleaseId(''); setRevision(value => value + 1); setMessage('Channel created.')
    } catch (failure) { setError(failure) } finally { setBusy(false) }
  }
  return <><PageHeader title="Channels" description={`Optional pointers to Published releases in ${app.name}.`} />
    {error !== undefined && <ProblemNotice error={error} />}{message && <Notice text={message} />}
    {allowed && <section className="panel form-panel"><h2>Create channel</h2><p>Choose any name. Channels are separate from the Server image's GHCR latest tag.</p>
      <form className="form-stack" onSubmit={create}><label>Channel name<input value={name} onChange={event => setName(event.target.value)} required /></label>
        <ReleaseChoice appId={app.id} label="Published Release" value={releaseId} onChange={setReleaseId} />
        <button className="button button-primary" disabled={busy}>{busy ? 'Creating…' : 'Create channel'}</button></form></section>}
    {channels.loading ? <LoadingState /> : channels.error ? <ProblemNotice error={channels.error} retry={channels.retry} /> : <>
      {!channels.value!.items.length && <EmptyState title="No channels" detail="Channels are optional; none is created automatically." />}
      {channels.value!.items.map(channel => <ChannelRow key={channel.name} channel={channel} editable={allowed} onChange={notice => { setMessage(notice); setRevision(value => value + 1) }} />)}
      {channels.value!.nextCursor && <button className="button button-secondary" onClick={() => setCursor(channels.value!.nextCursor!)}>Next channels page</button>}
    </>}
  </>
}

function ReleaseChoice({ appId, label, value, onChange }: { appId: string; label: string; value: string; onChange: (value: string) => void }) {
  const listId = useId()
  const [cursor, setCursor] = useState('')
  const choices = useResource(`channel-release-choices:${appId}:${cursor}`, signal => apiRequest<Page<Release>>(`/apps/${appId}/releases?pageSize=20${cursor ? `&cursor=${enc(cursor)}` : ''}`, { authenticated: true, signal }).then(result => result.value))
  return <div className="form-stack"><label>{label}<input list={listId} value={value} onChange={event => onChange(event.target.value)} required placeholder="Select or paste a Published Release ID" /></label>
    <datalist id={listId}>{choices.value?.items.filter(item => item.state === 'Published').map(item => <option key={item.id} value={item.id} label={item.version || 'Unversioned Published release'} />)}</datalist>
    {choices.error !== undefined && <ProblemNotice error={choices.error} retry={choices.retry} />}
    {choices.value?.nextCursor && <button type="button" className="text-button" onClick={() => setCursor(choices.value!.nextCursor!)}>More Release choices</button>}
    {cursor && <button type="button" className="text-button" onClick={() => setCursor('')}>First Release choices</button>}
  </div>
}

function ChannelRow({ channel, editable, onChange }: { channel: Channel; editable: boolean; onChange: (notice: string) => void }) {
  const { state } = useAuth()
  const path = `/apps/${channel.appId}/channels/${enc(channel.name)}`
  const current = useResource(`channel:${path}`, signal => resource<Channel>(path, signal))
  const release = useResource(`channel-target:${current.value?.value.releaseId || channel.releaseId}`, signal => apiRequest<Release>(`/releases/${current.value?.value.releaseId || channel.releaseId}`, { authenticated: true, signal }).then(result => result.value))
  const [target, setTarget] = useState(channel.releaseId)
  const [historyOpen, setHistoryOpen] = useState(false)
  const [cursor, setCursor] = useState('')
  const history = useResource(`history:${path}:${cursor}:${historyOpen}`, signal => historyOpen
    ? apiRequest<Page<ChannelMove>>(`${path}/history?pageSize=10${cursor ? `&cursor=${enc(cursor)}` : ''}`, { authenticated: true, signal }).then(result => result.value)
    : Promise.resolve({ items: [], nextCursor: null }))
  const [error, setError] = useState<unknown>()
  const [message, setMessage] = useState('')
  const [busy, setBusy] = useState(false)
  async function move(event: FormEvent) {
    event.preventDefault(); setBusy(true); setError(undefined); setMessage('')
    try {
      await apiRequest(path, { method: 'PUT', authenticated: true, csrf: state.session!.csrfToken, ifMatch: current.value!.etag,
        ...jsonBody({ releaseId: target.trim() }) })
      current.retry(); history.retry(); onChange('Channel moved.'); setMessage('Channel moved.')
    } catch (failure) { setError(failure) } finally { setBusy(false) }
  }
  async function remove() {
    await apiRequest(path, { method: 'DELETE', authenticated: true, csrf: state.session!.csrfToken, ifMatch: current.value!.etag })
    onChange('Channel deleted.')
  }
  return <section className="panel"><h2>{channel.name}</h2>{current.loading ? <LoadingState /> : current.error ? <ProblemNotice error={current.error} retry={current.retry} /> : <>
    <p>Points to <Link to={`/apps/${channel.appId}/releases/${current.value!.value.releaseId}`}>{release.value?.version || 'Unversioned release'}</Link> <StatusBadge value="Published" /></p>
    <TechnicalDetails><code>Release ID: {current.value!.value.releaseId}</code></TechnicalDetails>
    {error !== undefined && <ProblemNotice error={error} />}{message && <Notice text={message} />}
    {editable && <><form className="form-stack form-panel" onSubmit={move}><ReleaseChoice appId={channel.appId} label="Move to Published Release" value={target} onChange={setTarget} />
      <button className="button button-secondary" disabled={busy || target === current.value!.value.releaseId}>Move channel</button></form>
      <div className="section-action"><ConfirmDialog label={`Delete channel ${channel.name}`} detail="Delete this optional pointer? Releases and Artifacts remain available through their own routes." confirm="Delete channel" onConfirm={remove} /></div></>}
    <button className="text-button section-action" onClick={() => setHistoryOpen(value => !value)}>{historyOpen ? 'Hide history' : 'Movement history'}</button>
    {historyOpen && (history.loading ? <LoadingState /> : history.error ? <ProblemNotice error={history.error} retry={history.retry} /> : <><ul className="compact-list">{history.value!.items.map(move => <li key={move.id}><span>{new Date(move.movedAt).toLocaleString()}</span><Link to={`/apps/${channel.appId}/releases/${move.releaseId}`}>Release {move.releaseId}</Link></li>)}</ul>
      {history.value!.nextCursor && <button className="button button-secondary" onClick={() => setCursor(history.value!.nextCursor!)}>More history</button>}</>)}
  </>}</section>
}

export function OperationFromQuery() {
  const [params] = useSearchParams()
  return params.get('operation') ? <OperationNotice id={params.get('operation')!} /> : null
}
