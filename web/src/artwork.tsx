import { useEffect, useRef, useState } from 'react'
import { Link, useOutletContext } from 'react-router'
import { ApiError, apiRequest, idempotencyKey, type Artwork, type DepotApp } from './api'
import { EmptyState, LoadingState, PageHeader, ProblemNotice, TechnicalDetails } from './components'
import { ConfirmDialog } from './management'
import { useAuth } from './portal'
import { useResource } from './use-resource'

const standard = [
  { role: 'icon', label: 'Icon', group: 'General', guide: 'Square image recommended' },
  { role: 'logo', label: 'Logo', group: 'General', guide: 'Wide transparent image recommended; also usable by Steam-compatible clients' },
  { role: 'steam.grid.vertical', label: 'Vertical Grid', group: 'Steam Library', guide: '600 × 900 (2:3) recommended' },
  { role: 'steam.grid.horizontal', label: 'Horizontal Grid', group: 'Steam Library', guide: '920 × 430 (about 2.14:1) recommended' },
  { role: 'steam.hero', label: 'Hero', group: 'Steam Library', guide: '3840 × 1240 (about 3.10:1) recommended' },
] as const

const contentUrl = (appId: string, role: string, version = '') =>
  `/api/v1/apps/${encodeURIComponent(appId)}/artwork/${encodeURIComponent(role)}/content${version ? `?v=${encodeURIComponent(version)}` : ''}`

function artworkChanged(appId: string, role: string) {
  window.dispatchEvent(new CustomEvent('app-depot-artwork-changed', { detail: { appId, role } }))
}

export function ArtworkThumbnail({ app, role = 'icon' }: { app: DepotApp; role?: string }) {
  const [failed, setFailed] = useState(false)
  const [version, setVersion] = useState(0)
  useEffect(() => { setFailed(false); setVersion(0) }, [app.id, role])
  useEffect(() => {
    const changed = (event: Event) => {
      const detail = (event as CustomEvent<{ appId: string; role: string }>).detail
      if (detail?.appId === app.id && detail.role === role) { setFailed(false); setVersion(value => value + 1) }
    }
    window.addEventListener('app-depot-artwork-changed', changed)
    return () => window.removeEventListener('app-depot-artwork-changed', changed)
  }, [app.id, role])
  return <span className="artwork-thumb" aria-hidden="true">{failed ? app.name.slice(0, 1).toUpperCase() || '?' :
    <img src={contentUrl(app.id, role, String(version))} alt="" loading="lazy" decoding="async" onError={() => setFailed(true)} />}</span>
}

type CardStatus = { busy?: boolean; error?: unknown; message?: string; retry?: boolean }

function ArtworkCard({ app, item, role, label, guide, editable, status, legacy, onPick, onRetry, onRemove, onRemoveLegacy }: {
  app: DepotApp; item?: Artwork; role: string; label: string; guide: string; editable: boolean; status?: CardStatus; legacy?: Artwork;
  onPick: () => void; onRetry: () => void; onRemove: () => Promise<void>; onRemoveLegacy?: () => Promise<void>
}) {
  const [imageFailed, setImageFailed] = useState(false)
  useEffect(() => setImageFailed(false), [item?.role, item?.etag])
  const content = item && !imageFailed ? <img src={contentUrl(app.id, item.role, item.etag)} alt={`${label} for ${app.name}`} loading="lazy" decoding="async" onError={() => setImageFailed(true)} />
    : <span>{item ? 'Image unavailable' : 'Choose image'}</span>
  return <article className="artwork-card">{editable ? <button type="button" className="artwork-preview artwork-pick" onClick={onPick} disabled={status?.busy}
    aria-label={`${item ? 'Replace' : 'Add'} ${label} artwork`}>{content}</button> : <div className="artwork-preview">{content}</div>}
    <div className="artwork-card-details"><h3>{label}</h3><p className="subtle">{guide}</p>
      {item ? <p>{item.mimeType === 'image/jpeg' ? 'JPEG' : item.mimeType === 'image/webp' ? 'WebP' : 'PNG'} · {item.width} × {item.height} px · {(item.size / 1024).toFixed(1)} KiB</p> : <p className="subtle">Optional artwork</p>}
      {role === 'logo' && item?.role === 'steam.logo' && <p className="subtle">Showing an existing Steam logo. Replacing it creates the canonical Logo and keeps the older binding.</p>}
      {status?.busy && <div role="status"><progress aria-label={`Uploading ${label} artwork`} /> Uploading and validating image…</div>}
      {status?.message && <p className="success-notice" role="status">{status.message}</p>}
      {status?.error !== undefined && <><ProblemNotice error={status.error} />{status.retry && <button className="button button-secondary" onClick={onRetry}>Retry upload</button>}</>}
      {item && <TechnicalDetails><dl className="facts"><div><dt>Role</dt><dd><code>{item.role}</code></dd></div><div><dt>MIME type</dt><dd>{item.mimeType}</dd></div>
        <div><dt>SHA-256</dt><dd><code>{item.digest}</code></dd></div><div><dt>ETag</dt><dd><code>{item.etag}</code></dd></div></dl>
        {legacy && <p>Older <code>steam.logo</code> binding remains available to existing API clients. SHA-256: <code>{legacy.digest}</code>.</p>}</TechnicalDetails>}
      {editable && !status?.busy && item && <div className="artwork-card-actions"><ConfirmDialog label={`Remove ${label}`} detail={`Remove the current ${item.role} artwork binding? Shared CAS bytes and other roles are unaffected.`} confirm="Remove artwork" onConfirm={onRemove} />
        {legacy && onRemoveLegacy && <ConfirmDialog label="Remove legacy Steam logo" detail="Remove only the older steam.logo binding? The canonical Logo remains available." confirm="Remove legacy binding" onConfirm={onRemoveLegacy} />}</div>}
    </div></article>
}

type Attempt = { role: string; targetRole: string; file: File; validator: string; key: string }
const validRole = (role: string) => /^[a-z][a-z0-9._-]{0,79}$/.test(role) && !role.includes('..') && !/[._-]$/.test(role)

export function ArtworkPage() {
  const app = useOutletContext<DepotApp>()
  const { state } = useAuth()
  const editable = state.session!.user.serverAdmin || state.session!.user.id === app.ownerId
  const [revision, setRevision] = useState(0)
  const current = useResource(`artwork:${app.id}:${revision}`, signal => apiRequest<{ items: Artwork[] }>(`/apps/${app.id}/artwork`, { authenticated: true, signal }).then(result => result.value.items))
  const [optimistic, setOptimistic] = useState<Artwork[]>()
  const [statuses, setStatuses] = useState<Record<string, CardStatus>>({})
  const [custom, setCustom] = useState('')
  const [newCustomRole, setNewCustomRole] = useState('')
  const input = useRef<HTMLInputElement>(null)
  const selectedRole = useRef('')
  const attempts = useRef<Map<string, Attempt>>(new Map())
  const inFlight = useRef<Set<string>>(new Set())
  useEffect(() => { if (current.value) setOptimistic(undefined) }, [current.value])
  useEffect(() => { setOptimistic(undefined); setStatuses({}); attempts.current.clear() }, [app.id])
  const items = optimistic ?? current.value ?? []
  const exact = (role: string) => items.find(item => item.role === role)
  const logo = exact('logo') || exact('steam.logo')
  const legacy = exact('logo') && exact('steam.logo')
  function status(role: string, value: CardStatus) { setStatuses(previous => ({ ...previous, [role]: value })) }
  function pick(role: string) {
    if (inFlight.current.has(role) || !input.current) return
    selectedRole.current = role
    input.current.accept = role === 'icon' ? 'image/png,image/jpeg,image/webp,image/x-icon,image/vnd.microsoft.icon,.ico' : 'image/png,image/jpeg,image/webp'
    input.current.value = ''
    input.current.click()
  }
  async function upload(attempt: Attempt) {
    if (inFlight.current.has(attempt.role)) return
    inFlight.current.add(attempt.role)
    status(attempt.role, { busy: true })
    try {
      const result = await apiRequest<Artwork>(`/apps/${app.id}/artwork/${encodeURIComponent(attempt.targetRole)}`, {
        method: 'PUT', body: attempt.file, authenticated: true, csrf: state.session!.csrfToken,
        idempotencyKey: attempt.key,
        headers: attempt.validator === '*' ? { 'If-None-Match': '*' } : { 'If-Match': attempt.validator },
      })
      setOptimistic(previous => [...(previous ?? current.value ?? []).filter(item => item.role !== attempt.targetRole), result.value])
      status(attempt.role, { message: attempt.validator === '*' ? 'Artwork added.' : 'Artwork replaced.' })
      attempts.current.delete(attempt.role)
      setNewCustomRole(previous => previous === attempt.role ? '' : previous)
      artworkChanged(app.id, attempt.targetRole)
      setRevision(value => value + 1)
    } catch (error) {
      const stale = error instanceof ApiError && error.status === 412
      status(attempt.role, { error, retry: !stale })
      if (stale) { attempts.current.delete(attempt.role); setRevision(value => value + 1) }
    } finally { inFlight.current.delete(attempt.role) }
  }
  function selected(file: File | undefined) {
    if (!file) return
    const role = selectedRole.current
    if (!validRole(role)) { status(role, { error: new Error('Choose a valid artwork role.') }); return }
    if (file.size > 8 * 1024 * 1024) { status(role, { error: new Error('Images must be 8 MiB or smaller.') }); return }
    const validator = exact(role)?.etag || '*'
    const attempt = { role, targetRole: role, file, validator, key: idempotencyKey() }
    attempts.current.set(role, attempt)
    void upload(attempt)
  }
  async function remove(item: Artwork) {
    await apiRequest<void>(`/apps/${app.id}/artwork/${encodeURIComponent(item.role)}`, { method: 'DELETE', authenticated: true,
      csrf: state.session!.csrfToken, ifMatch: item.etag })
    setOptimistic(previous => (previous ?? current.value ?? []).filter(existing => existing.role !== item.role))
    status(item.role === 'steam.logo' ? 'logo' : item.role, { message: 'Artwork removed.' })
    artworkChanged(app.id, item.role)
    setRevision(value => value + 1)
  }
  const customItems = items.filter(item => !standard.some(entry => entry.role === item.role) && item.role !== 'steam.logo')
  const customRoles = [...new Set([...customItems.map(item => item.role), ...(newCustomRole ? [newCustomRole] : [])])]
  return <><PageHeader title="Artwork" description={`Optional images for ${app.name}, independent of releases and original artifacts.`} />
    {current.loading && !optimistic ? <LoadingState label="Loading artwork…" /> : current.error && !optimistic ? <ProblemNotice error={current.error} retry={current.retry} /> : <>
      {!items.length && <EmptyState title="No artwork yet" detail="Add an icon, logo, Steam Library image, or custom role. Artwork is optional for every application." />}
      {['General', 'Steam Library'].map(group => <section className="panel" key={group}><h2>{group}</h2><div className="artwork-grid">{standard.filter(entry => entry.group === group).map(entry => {
        const item = entry.role === 'logo' ? logo : exact(entry.role)
        return <ArtworkCard key={entry.role} app={app} item={item} role={entry.role} label={entry.label} guide={entry.guide} editable={editable}
          status={statuses[entry.role]} legacy={entry.role === 'logo' && legacy ? legacy : undefined}
          onPick={() => pick(entry.role)} onRetry={() => { const attempt = attempts.current.get(entry.role); if (attempt) void upload(attempt) }}
          onRemove={() => remove(item!)} onRemoveLegacy={legacy ? () => remove(legacy) : undefined} />
      })}</div></section>)}
      {!!customRoles.length && <section className="panel"><h2>Custom roles</h2><div className="artwork-grid">{customRoles.map(role => {
        const item = exact(role)
        return <ArtworkCard key={role} app={app} item={item} role={role} label={role} guide="Custom artwork role" editable={editable}
          status={statuses[role]} onPick={() => pick(role)} onRetry={() => { const attempt = attempts.current.get(role); if (attempt) void upload(attempt) }} onRemove={() => remove(item!)} />
      })}</div></section>}
      {editable && <><input hidden ref={input} type="file" aria-label="Choose artwork image" onChange={event => { const file = event.target.files?.[0]; event.target.value = ''; selected(file) }} />
        <section className="panel form-panel"><h2>Add custom role</h2><p className="subtle">PNG, JPEG or WebP; up to 8 MiB. ICO is accepted for Icon only. Suggested dimensions are guidance.</p>
          <div className="form-actions"><label>Role identifier<input value={custom} onChange={event => setCustom(event.target.value)} maxLength={80} placeholder="website.banner" /></label>
            <button type="button" className="button button-secondary" disabled={!validRole(custom.trim()) || standard.some(entry => entry.role === custom.trim()) || custom.trim() === 'steam.logo'}
              onClick={() => { const role = custom.trim(); setNewCustomRole(role); pick(role) }}>Add custom role</button></div></section></>}
      {!editable && <p className="subtle">Only the application owner or Server Admin can change artwork.</p>}
      <p className="subtle"><Link to={`/apps/${app.id}`}>Back to application overview</Link></p>
    </>}
  </>
}
