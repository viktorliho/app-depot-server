import { useEffect, useRef, useState } from 'react'
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

function ArtworkCard({ app, item, role, label, guide, editable, onPick, onRemove }: { app: DepotApp; item?: Artwork; role: string; label: string; guide: string; editable: boolean; onPick: () => void; onRemove: () => Promise<void> }) {
  return <article className="artwork-card">{editable ? <button type="button" className="artwork-preview artwork-pick" onClick={onPick} aria-label={`${item ? 'Replace' : 'Add'} ${label} artwork`}>{item ? <img src={contentUrl(app.id, item.role)} alt={`${label} for ${app.name}`} /> : <span>Choose image</span>}</button> : <div className="artwork-preview">{item ? <img src={contentUrl(app.id, item.role)} alt={`${label} for ${app.name}`} /> : <span>No image</span>}</div>}
    <div><h3>{label}</h3><p className="subtle">{guide}</p><p className="subtle">Role: <code>{role}</code></p>{item ? <><p>{item.width} × {item.height} px · {(item.size / 1024).toFixed(1)} KiB · {item.mimeType}</p>
      <TechnicalDetails><dl className="facts"><div><dt>SHA-256</dt><dd><code>{item.digest}</code></dd></div></dl></TechnicalDetails>
      {editable && <ConfirmDialog label={`Remove ${label}`} detail={`Remove the current ${item.role} artwork binding? Other applications and artifacts that reference the same bytes are unaffected.`} confirm="Remove artwork" onConfirm={onRemove} />}</> : <p className="subtle">Optional artwork</p>}</div>
  </article>
}

export function ArtworkPage() {
  const app = useOutletContext<DepotApp>()
  const { state } = useAuth()
  const editable = state.session!.user.serverAdmin || state.session!.user.id === app.ownerId
  const [revision, setRevision] = useState(0)
  const current = useResource(`artwork:${app.id}:${revision}`, signal => apiRequest<{ items: Artwork[] }>(`/apps/${app.id}/artwork`, { authenticated: true, signal }).then(result => result.value.items))
  const [custom, setCustom] = useState('')
  const [role, setRole] = useState('')
  const [file, setFile] = useState<File>()
  const [preview, setPreview] = useState('')
  const input = useRef<HTMLInputElement>(null)
  const attempt = useRef<{ role: string; file: File; validator: string; key: string } | undefined>(undefined)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const [message, setMessage] = useState('')
  const selected = current.value?.find(item => item.role === role)
  const validRole = /^[a-z][a-z0-9._-]{0,79}$/.test(role) && !role.includes('..') && !/[._-]$/.test(role)
  useEffect(() => {
    if (!file || !['image/png', 'image/jpeg', 'image/webp'].includes(file.type)) { setPreview(''); return }
    const url = URL.createObjectURL(file); setPreview(url)
    return () => URL.revokeObjectURL(url)
  }, [file])
  function pick(nextRole: string) { setRole(nextRole); setFile(undefined); setError(undefined); if (input.current) { input.current.value = ''; input.current.click() } }
  async function upload() {
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
      setFile(undefined); setRole(''); if (input.current) input.current.value = ''
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
      {['General', 'Steam Library'].map(group => <section className="panel" key={group}><h2>{group}</h2><div className="artwork-grid">{standard.filter(entry => entry.group === group).map(entry => <ArtworkCard key={entry.role} app={app} item={items.find(item => item.role === entry.role)} role={entry.role} label={entry.label} guide={entry.guide} editable={editable} onPick={() => pick(entry.role)} onRemove={() => remove(items.find(item => item.role === entry.role)!)} />)}</div></section>)}
      {!!customItems.length && <section className="panel"><h2>Custom roles</h2><div className="artwork-grid">{customItems.map(item => <ArtworkCard key={item.role} app={app} item={item} role={item.role} label={item.role} guide="Custom artwork role" editable={editable} onPick={() => pick(item.role)} onRemove={() => remove(item)} />)}</div></section>}
      {editable && <><input hidden ref={input} type="file" accept="image/png,image/jpeg,image/webp" onChange={event => { setFile(event.target.files?.[0]); setError(undefined) }} />
        <section className="panel form-panel"><h2>Add custom role</h2><p className="subtle">PNG, JPEG or WebP; maximum 8 MiB. Suggested dimensions are guidance only.</p><div className="form-actions"><label>Role identifier<input value={custom} onChange={event => setCustom(event.target.value)} maxLength={80} placeholder="website.banner" /></label><button type="button" className="button button-secondary" disabled={!/^[a-z][a-z0-9._-]{0,79}$/.test(custom.trim()) || custom.includes('..') || /[._-]$/.test(custom.trim())} onClick={() => pick(custom.trim())}>Choose image</button></div></section>
        {file && <section className="panel form-panel" role="status"><h2>Confirm {selected ? 'replacement' : 'upload'}</h2><p>Role: <code>{role}</code> · {file.name} · {(file.size / 1024).toFixed(1)} KiB · {file.type || 'Unknown type'}</p>{preview && <img className="artwork-confirm-preview" src={preview} alt="Selected artwork preview" />}<p>The Server will validate the original image and its dimensions before making it current.</p><div className="form-actions"><button className="button button-primary" disabled={busy || !validRole} onClick={() => void upload()}>{busy ? 'Uploading…' : selected ? 'Replace artwork' : 'Add artwork'}</button><button className="button button-secondary" disabled={busy} onClick={() => { setFile(undefined); setRole('') }}>Cancel</button></div></section>}
        {error !== undefined && <ProblemNotice error={error} />}{message && <p className="success-notice" role="status">{message}</p>}</>}
      {!editable && <p className="subtle">Only the application owner or Server Admin can change artwork.</p>}
      <p className="subtle"><Link to={`/apps/${app.id}`}>Back to application overview</Link></p>
    </>}
  </>
}
