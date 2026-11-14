import { useEffect, useRef, useState, type FormEvent } from 'react'
import { Link, useOutletContext } from 'react-router'
import { apiRequest, idempotencyKey, type Artwork, type DepotApp } from './api'
import { EmptyState, LoadingState, PageHeader, ProblemNotice, TechnicalDetails } from './components'
import { ConfirmDialog } from './management'
import { useAuth } from './portal'
import { useResource } from './use-resource'

const standard = [
  { role: 'icon', label: 'Icon', group: 'General', guide: 'Square image recommended' },
  { role: 'logo', label: 'Logo', group: 'General', guide: 'Wide transparent image recommended' },
  { role: 'steam.grid.vertical', label: 'Vertical Grid', group: 'Steam Library', guide: '600 × 900 (2:3) recommended' },
  { role: 'steam.grid.horizontal', label: 'Horizontal Grid', group: 'Steam Library', guide: '920 × 430 (about 2.14:1) recommended' },
  { role: 'steam.hero', label: 'Hero', group: 'Steam Library', guide: '3840 × 1240 (about 3.10:1) recommended' },
  { role: 'steam.logo', label: 'Logo', group: 'Steam Library', guide: '1280 px wide or 720 px high; transparent PNG recommended' },
] as const

const contentUrl = (appId: string, role: string) => `/api/v1/apps/${encodeURIComponent(appId)}/artwork/${encodeURIComponent(role)}/content`

export function ArtworkThumbnail({ app, role = 'icon' }: { app: DepotApp; role?: string }) {
  const [available, setAvailable] = useState(true)
  useEffect(() => setAvailable(true), [app.id, role])
  return <span className="artwork-thumb" aria-hidden="true">{available ? <img src={contentUrl(app.id, role)} alt="" onError={() => setAvailable(false)} /> : app.name.slice(0, 1).toUpperCase()}</span>
}

function ArtworkCard({ app, item, label, guide, editable, onRemove }: { app: DepotApp; item?: Artwork; label: string; guide: string; editable: boolean; onRemove: () => Promise<void> }) {
  return <article className="artwork-card"><div className="artwork-preview">{item ? <img src={contentUrl(app.id, item.role)} alt={`${label} for ${app.name}`} /> : <span>No image</span>}</div>
    <div><h3>{label}</h3><p className="subtle">{guide}</p>{item ? <><p>{item.width} × {item.height} px · {(item.size / 1024).toFixed(1)} KiB · {item.mimeType}</p>
      <TechnicalDetails><dl className="facts"><div><dt>Role</dt><dd><code>{item.role}</code></dd></div><div><dt>SHA-256</dt><dd><code>{item.digest}</code></dd></div></dl></TechnicalDetails>
      {editable && <ConfirmDialog label={`Remove ${label}`} detail={`Remove the current ${item.role} artwork binding? Other applications and artifacts that reference the same bytes are unaffected.`} confirm="Remove artwork" onConfirm={onRemove} />}</> : <p className="subtle">Optional artwork</p>}</div>
  </article>
}

export function ArtworkPage() {
  const app = useOutletContext<DepotApp>()
  const { state } = useAuth()
  const editable = state.session!.user.serverAdmin || state.session!.user.id === app.ownerId
  const [revision, setRevision] = useState(0)
  const current = useResource(`artwork:${app.id}:${revision}`, signal => apiRequest<{ items: Artwork[] }>(`/apps/${app.id}/artwork`, { authenticated: true, signal }).then(result => result.value.items))
  const [choice, setChoice] = useState<string>('icon')
  const [custom, setCustom] = useState('')
  const [file, setFile] = useState<File>()
  const input = useRef<HTMLInputElement>(null)
  const attempt = useRef<{ role: string; file: File; validator: string; key: string } | undefined>(undefined)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const [message, setMessage] = useState('')
  const role = choice === 'custom' ? custom.trim() : choice
  const selected = current.value?.find(item => item.role === role)
  const validRole = /^[a-z][a-z0-9._-]{0,79}$/.test(role) && !role.includes('..') && !/[._-]$/.test(role)
  async function upload(event: FormEvent) {
    event.preventDefault()
    if (!file || !validRole) { setError(new Error('Choose an image and a valid role identifier.')); return }
    if (file.size > 8 * 1024 * 1024) { setError(new Error('Images must be 8 MiB or smaller.')); return }
    const validator = selected?.etag || '*'
    if (!attempt.current || attempt.current.role !== role || attempt.current.file !== file || attempt.current.validator !== validator) {
      attempt.current = { role, file, validator, key: idempotencyKey() }
    }
    setBusy(true); setError(undefined); setMessage('')
    try {
      await apiRequest<Artwork>(`/apps/${app.id}/artwork/${encodeURIComponent(role)}`, {
        method: 'PUT', body: file, authenticated: true, csrf: state.session!.csrfToken,
        idempotencyKey: attempt.current.key,
        headers: selected ? { 'If-Match': validator } : { 'If-None-Match': '*' },
      })
      setMessage(selected ? 'Artwork replaced.' : 'Artwork added.')
      setFile(undefined); if (input.current) input.current.value = ''
      attempt.current = undefined; setRevision(value => value + 1)
    } catch (failure) { setError(failure) } finally { setBusy(false) }
  }
  async function remove(item: Artwork) {
    await apiRequest<void>(`/apps/${app.id}/artwork/${encodeURIComponent(item.role)}`, { method: 'DELETE', authenticated: true,
      csrf: state.session!.csrfToken, ifMatch: item.etag })
    setRevision(value => value + 1); setMessage(`${item.role} removed.`)
  }
  const items = current.value || []
  const customItems = items.filter(item => !standard.some(entry => entry.role === item.role))
  return <><PageHeader title="Artwork" description={`Optional images for ${app.name}, independent of releases and original artifacts.`} />
    {current.loading ? <LoadingState label="Loading artwork…" /> : current.error ? <ProblemNotice error={current.error} retry={current.retry} /> : <>
      {!items.length && <EmptyState title="No artwork yet" detail="Add an icon, logo, Steam Library image, or custom role. Artwork is optional for every application." />}
      {['General', 'Steam Library'].map(group => <section className="panel" key={group}><h2>{group}</h2><div className="artwork-grid">{standard.filter(entry => entry.group === group).map(entry => <ArtworkCard key={entry.role} app={app} item={items.find(item => item.role === entry.role)} label={entry.label} guide={entry.guide} editable={editable} onRemove={() => remove(items.find(item => item.role === entry.role)!)} />)}</div></section>)}
      {!!customItems.length && <section className="panel"><h2>Custom roles</h2><div className="artwork-grid">{customItems.map(item => <ArtworkCard key={item.role} app={app} item={item} label={item.role} guide="Custom artwork role" editable={editable} onRemove={() => remove(item)} />)}</div></section>}
      {editable && <section className="panel form-panel"><h2>Add or replace artwork</h2><p className="subtle">PNG, JPEG, or WebP. Maximum 8 MiB, 8192 px on either side, and 16 megapixels. Suggested sizes are guidance only.</p>
        <form className="form-stack" onSubmit={upload}><label>Artwork role<select value={choice} onChange={event => { setChoice(event.target.value); setError(undefined) }}>{standard.map(entry => <option key={entry.role} value={entry.role}>{entry.group} · {entry.label}</option>)}{customItems.map(item => <option key={item.role} value={item.role}>Custom · {item.role}</option>)}<option value="custom">New custom role…</option></select></label>
          {choice === 'custom' && <label>Custom role identifier<input value={custom} onChange={event => setCustom(event.target.value)} maxLength={80} placeholder="e.g. website.banner" required /><span className="subtle">Lowercase letters, digits, dots, dashes and underscores.</span></label>}
          <label>Image file<input ref={input} type="file" accept="image/png,image/jpeg,image/webp" onChange={event => { setFile(event.target.files?.[0]); setError(undefined) }} required /></label>
          {selected && <p className="subtle">This will replace the current image for <code>{role}</code> after validation.</p>}
          {error !== undefined && <ProblemNotice error={error} />}{message && <p className="success-notice" role="status">{message}</p>}
          <button className="button button-primary" disabled={busy || !file || !validRole}>{busy ? 'Uploading…' : selected ? 'Replace artwork' : 'Add artwork'}</button></form></section>}
      {!editable && <p className="subtle">Only the application owner or Server Admin can change artwork.</p>}
      <p className="subtle"><Link to={`/apps/${app.id}`}>Back to application overview</Link></p>
    </>}
  </>
}
