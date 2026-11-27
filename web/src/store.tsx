import { useEffect, useState } from 'react'
import { apiRequest, jsonBody } from './api'
import {
  PageHeader,
  ProblemNotice,
  LoadingState,
  StatusBadge,
} from './components'
import { useAuth } from './portal'
import { platformName, platforms } from './installation-review'
import { fileSize } from './release-information'

type Build = { platform: string; version: string }
type Product = {
  id: string
  title: string
  summary: string
  publisher: string
  aliases?: string[]
  steamId?: number
  revision: number
  rights: string
  upstream: string
  availability: string
  builds: Build[]
  components: { name: string; sha256: string; bytes: number }[]
}
type Catalog = {
  generation: number
  recipes: Product[]
  refreshedAt?: number
  source: string
}
type TaskProgress = {
  stage: string
  component?: string
  componentsDone?: number
  componentsTotal?: number
  done?: number
  total?: number
  bytesPerSecond?: number
}
type Row = {
  id: string
  product: string
  title: string
  platform: string
  currentVersion?: string
  detected?: string
  status: string
  error?: string
  app: string
  revision: number
  enabled: boolean
  automatic: boolean
  hours: number
  lastCheck?: number
  successfulCheck?: number
  progress?: TaskProgress
  filesDone: number
  filesTotal: number
  bytesDone: number
  bytesTotal: number
}
type Activity = Pick<Row, 'id' | 'title' | 'platform' | 'status' | 'error'> & {
  finished: number
}
const date = (n?: number) => (n ? new Date(n * 1000).toLocaleString() : '—')
const active = (r: Row) =>
  ['Queued', 'Checking', 'Preparing', 'Importing'].includes(r.status)
function Art({ product }: { product: Product }) {
  const [missing, setMissing] = useState(false)
  return product.steamId && !missing ? (
    <img
      className="store-cover"
      loading="lazy"
      alt=""
      src={
        '/api/v1/metadata/steam/' +
        product.steamId +
        '/artwork/steam.grid.vertical'
      }
      onError={() => setMissing(true)}
    />
  ) : (
    <div className="store-cover store-placeholder" aria-hidden="true">
      {product.title
        .split(' ')
        .map((p) => p[0])
        .slice(0, 3)
        .join('')}
    </div>
  )
}
function Progress({ row }: { row: Row }) {
  const p =
    row.status === 'Preparing' || row.status === 'Importing'
      ? row.progress
      : undefined
  const total = p?.total ?? (row.status === 'Importing' ? row.bytesTotal : 0)
  const done = p?.done ?? row.bytesDone
  const bytes = !p || /Downloading|cached/.test(p.stage)
  return (
    <div className="store-progress">
      <strong>
        {p?.stage ??
          {
            Checking: 'Resolving upstream versions',
            Importing: 'Registering artifact',
          }[row.status] ??
          row.status}
      </strong>
      {p?.component && (
        <span>
          {p.component} · {p.componentsDone}/{p.componentsTotal} components
          complete
        </span>
      )}
      {row.status !== 'Queued' && (
        <progress
          aria-label={p?.stage ?? row.status}
          value={total ? done : undefined}
          max={total || undefined}
        />
      )}
      {total > 0 && (
        <span>
          {bytes ? fileSize(done) : done} / {bytes ? fileSize(total) : total}
        </span>
      )}
      {p?.stage === 'Extracting component' && done > 0 && (
        <span>{fileSize(done)} written by extractor</span>
      )}
      {row.status === 'Importing' && row.filesTotal > 0 && (
        <span>
          {row.filesDone} / {row.filesTotal} files
        </span>
      )}
      {p?.bytesPerSecond && <small>{fileSize(p.bytesPerSecond)}/s</small>}
      {row.lastCheck && (
        <small>
          Started {date(row.lastCheck)} ·{' '}
          {Math.max(0, Math.floor(Date.now() / 1000) - row.lastCheck)} s elapsed
        </small>
      )}
    </div>
  )
}
type Action = (id: string, action: string, body?: unknown) => Promise<void>
function Settings({ row, act }: { row: Row; act: Action }) {
  const [hours, setHours] = useState(String(row.hours)),
    [enabled, setEnabled] = useState(row.enabled),
    [automatic, setAutomatic] = useState(row.automatic)
  return (
    <details>
      <summary>Observation settings</summary>
      <form
        className="form-stack form-panel"
        onSubmit={(e) => {
          e.preventDefault()
          void act(row.product, 'observation', {
            hours: Number(hours),
            enabled,
            automatic,
          })
        }}
      >
        <label className="checkbox-row">
          <input
            type="checkbox"
            checked={enabled}
            onChange={(e) => setEnabled(e.target.checked)}
          />{' '}
          Periodic checks
        </label>
        <label>
          Check interval (hours)
          <input
            type="number"
            min="1"
            max="8760"
            required
            value={hours}
            onChange={(e) => setHours(e.target.value)}
          />
        </label>
        <label className="checkbox-row">
          <input
            type="checkbox"
            checked={automatic}
            onChange={(e) => setAutomatic(e.target.checked)}
          />{' '}
          Publish verified updates automatically
        </label>
        <button className="button">Save settings</button>
      </form>
    </details>
  )
}
function Selection({
  product,
  rows,
  act,
}: {
  product: Product
  rows: Row[]
  act: Action
}) {
  const available = product.builds.filter(
    (b) => !rows.some((r) => r.platform === b.platform)
  )
  const [unchecked, setUnchecked] = useState<string[]>([])
  const chosen = available
    .map((b) => b.platform)
    .filter((p) => !unchecked.includes(p))
  if (!available.length) return null
  return (
    <fieldset>
      <legend>Platforms to acquire</legend>
      {available.map((b) => (
        <label key={b.platform} className="checkbox-row">
          <input
            type="checkbox"
            checked={chosen.includes(b.platform)}
            onChange={(e) =>
              setUnchecked(
                e.target.checked
                  ? unchecked.filter((p) => p !== b.platform)
                  : [...unchecked, b.platform]
              )
            }
          />
          {platformName(b.platform)} · {b.version}
        </label>
      ))}
      <button
        type="button"
        className="button button-primary"
        disabled={!chosen.length || product.availability !== 'available'}
        onClick={() => act(product.id, 'add', { platforms: chosen })}
      >
        {rows.length ? 'Add platforms' : 'Add to server'}
      </button>
    </fieldset>
  )
}
export function StorePage() {
  const { state } = useAuth()
  const [catalog, setCatalog] = useState<Catalog>(),
    [rows, setRows] = useState<Row[]>([]),
    [history, setHistory] = useState<Activity[]>([])
  const [tab, setTab] = useState('Browse'),
    [query, setQuery] = useState(''),
    [platform, setPlatform] = useState(''),
    [detail, setDetail] = useState<string>()
  const [error, setError] = useState<unknown>(),
    [busy, setBusy] = useState(false),
    [revision, setRevision] = useState(0)
  useEffect(() => {
    const c = new AbortController()
    apiRequest<Catalog>('/admin/store', {
      authenticated: true,
      signal: c.signal,
    })
      .then((r) => {
        if (!c.signal.aborted)
          setCatalog((previous) =>
            previous && previous.generation > r.value.generation
              ? previous
              : r.value
          )
      })
      .catch((e) => {
        if (!c.signal.aborted) setError(e)
      })
    return () => c.abort()
  }, [])
  useEffect(() => {
    const c = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    async function poll() {
      try {
        const r = await apiRequest<Row[]>('/admin/store/observations', {
          authenticated: true,
          signal: c.signal,
        })
        if (c.signal.aborted) return
        setRows(r.value)
        if (tab === 'Activity') {
          const a = await apiRequest<Activity[]>('/admin/store/activity', {
            authenticated: true,
            signal: c.signal,
          })
          if (!c.signal.aborted) setHistory(a.value)
        }
      } catch (e) {
        if (!c.signal.aborted) setError(e)
      }
      if (!c.signal.aborted) timer = setTimeout(poll, 3000)
    }
    void poll()
    return () => {
      c.abort()
      clearTimeout(timer)
    }
  }, [revision, tab])
  async function refresh() {
    setBusy(true)
    setError(undefined)
    try {
      const r = await apiRequest<Catalog>('/admin/store/refresh', {
        method: 'POST',
        csrf: state.session?.csrfToken,
        authenticated: true,
      })
      setCatalog(r.value)
    } catch (e) {
      setError(e)
    } finally {
      setBusy(false)
    }
  }
  const act: Action = async (id, action, body) => {
    if (busy) return
    setBusy(true)
    setError(undefined)
    try {
      await apiRequest(
        '/admin/store/' + encodeURIComponent(id) + '/' + action,
        {
          method: action === 'observation' ? 'PUT' : 'POST',
          csrf: state.session?.csrfToken,
          authenticated: true,
          ...(body === undefined ? {} : jsonBody(body)),
        }
      )
      setRevision((x) => x + 1)
      if (action === 'add') {
        setTab('My Store Apps')
        setDetail(id)
      }
    } catch (e) {
      setError(e)
    } finally {
      setBusy(false)
    }
  }
  const products = catalog?.recipes ?? []
  const selected = products.find((p) => p.id === detail)
  const groups = Array.from(new Set(rows.map((r) => r.product)))
  return (
    <>
      <PageHeader
        title="Store"
        actions={
          <button
            className="button button-secondary"
            disabled={busy}
            onClick={refresh}
          >
            {busy ? 'Working…' : 'Refresh catalog'}
          </button>
        }
      />
      <nav className="form-actions" aria-label="Store sections">
        {['Browse', 'My Store Apps', 'Activity'].map((t) => (
          <button
            key={t}
            className={
              'button ' + (tab === t ? 'button-primary' : 'button-secondary')
            }
            aria-pressed={tab === t}
            onClick={() => {
              setTab(t)
              setDetail(undefined)
            }}
          >
            {t}
          </button>
        ))}
      </nav>
      {!!error && <ProblemNotice error={error} />}
      {tab === 'Browse' && (
        <>
          <div className="store-filters">
            <label>
              Search applications
              <input
                type="search"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
            <label>
              Platform
              <select
                value={platform}
                onChange={(e) => setPlatform(e.target.value)}
              >
                <option value="">All platforms</option>
                {platforms.map((p) => (
                  <option key={p} value={p}>
                    {platformName(p)}
                  </option>
                ))}
              </select>
            </label>
          </div>
          {!catalog && !error && <LoadingState label="Loading catalog…" />}
          {catalog && (
            <>
              <details className="technical-details">
                <summary>Catalog information</summary>
                <p>
                  Catalog generation {catalog.generation} · Last refreshed{' '}
                  {date(catalog.refreshedAt)}
                </p>
                <p>{catalog.source}</p>
              </details>
              {!catalog.generation ? (
                <p>Refresh the catalog to browse recipes.</p>
              ) : !products.length ? (
                <p>This catalog has no entries.</p>
              ) : null}
            </>
          )}
          <div className="store-grid">
            {products
              .filter(
                (p) =>
                  [p.title, ...(p.aliases ?? [])].some((name) =>
                    name.toLowerCase().includes(query.trim().toLowerCase())
                  ) &&
                  (!platform || p.builds.some((b) => b.platform === platform))
              )
              .map((p) => {
                const productRows = rows.filter((r) => r.product === p.id)
                const acquired = productRows.length > 0
                return (
                  <article key={p.id} className="panel store-card">
                    <Art product={p} />
                    <div>
                      <small>{p.publisher}</small>
                      <h2>{p.title}</h2>
                      <p>{p.summary}</p>
                      <p>
                        {Array.from(
                          new Set(p.builds.map((b) => b.platform.split('/')[0]))
                        ).join(' · ')}{' '}
                        ·{' '}
                        {Array.from(
                          new Set(p.builds.map((b) => b.version))
                        ).join(', ')}
                      </p>
                      <p>
                        {p.availability !== 'available'
                          ? 'Preparation under review'
                          : acquired
                            ? productRows.some(active)
                              ? 'Acquisition in progress'
                              : 'In library'
                            : 'Available to acquire'}
                      </p>
                      <button
                        className="button"
                        onClick={() => {
                          setDetail(p.id)
                          if (acquired) setTab('My Store Apps')
                        }}
                      >
                        {acquired ? 'Manage' : 'View application'}
                      </button>
                    </div>
                  </article>
                )
              })}
          </div>
        </>
      )}
      {tab === 'My Store Apps' && (
        <>
          {!groups.length && <p>No Store applications acquired.</p>}
          {groups.map((id) => {
            const items = rows.filter((r) => r.product === id),
              first = items[0],
              p = products.find((x) => x.id === id)
            return (
              <section className="panel" key={id}>
                <div className="panel-heading">
                  <div className="store-owned-title">
                    {p && <Art product={p} />}
                    <h2>{first.title}</h2>
                  </div>
                  <a href={'/apps/' + first.app}>Open application</a>
                </div>
                <p>
                  {first.enabled
                    ? 'Periodic checks enabled'
                    : 'Periodic checks paused'}{' '}
                  ·{' '}
                  {first.automatic
                    ? 'Automatic publication'
                    : 'Manual publication'}
                </p>
                {items.map((r) => (
                  <div key={r.id} className="store-build">
                    <strong>{platformName(r.platform)}</strong>{' '}
                    {r.currentVersion || 'Awaiting first release'} ·{' '}
                    <StatusBadge value={r.status} />
                    {r.detected !== r.currentVersion && r.detected && (
                      <span> · Detected {r.detected}</span>
                    )}
                    {active(r) ? (
                      <Progress row={r} />
                    ) : (
                      <p>
                        Last successful check {date(r.successfulCheck)} · Next
                        check{' '}
                        {r.enabled && r.lastCheck
                          ? date(r.lastCheck + r.hours * 3600)
                          : '—'}
                      </p>
                    )}
                    {r.error && <p role="alert">{r.error}</p>}
                    {!active(r) && (
                      <button
                        className="button"
                        disabled={busy}
                        onClick={() => act(r.id, 'check')}
                      >
                        {r.status === 'Failed' ? 'Retry' : 'Check for updates'}
                      </button>
                    )}
                    {r.status === 'Update available' && (
                      <button
                        className="button"
                        disabled={busy}
                        onClick={() => act(r.id, 'update')}
                      >
                        Publish this update
                      </button>
                    )}
                    {p && p.revision > r.revision && !active(r) && (
                      <button
                        className="button"
                        disabled={busy}
                        onClick={() => act(r.id, 'adopt')}
                      >
                        Apply recipe revision {p.revision}
                      </button>
                    )}
                  </div>
                ))}
                <Settings row={first} act={act} />
                {p && items.length < p.builds.length && (
                  <details>
                    <summary>Manage platforms</summary>
                    <Selection product={p} rows={items} act={act} />
                  </details>
                )}
              </section>
            )
          })}
        </>
      )}
      {tab === 'Activity' && (
        <section className="panel">
          <h2>Active tasks</h2>
          {!rows.some(active) && <p>No active tasks.</p>}
          {rows.filter(active).map((r) => (
            <div key={r.id}>
              <h3>
                {r.title} · {platformName(r.platform)}
              </h3>
              <Progress row={r} />
            </div>
          ))}
          <h2>Recent activity</h2>
          {history.map((e, i) => (
            <p key={i}>
              {date(e.finished)} · {e.title} · {platformName(e.platform)} ·{' '}
              {e.status} {e.error}
              {e.status === 'Failed' &&
                rows.find((r) => r.id === e.id)?.status === 'Failed' && (
                  <button className="button" onClick={() => act(e.id, 'check')}>
                    Retry
                  </button>
                )}
            </p>
          ))}
        </section>
      )}
      {tab === 'Browse' && selected && (
        <section className="panel" aria-label="Application details">
          <div className="panel-heading">
            <h2>{selected.title}</h2>
            <button className="button" onClick={() => setDetail(undefined)}>
              Close details
            </button>
          </div>
          <p>{selected.summary}</p>
          <a href={selected.upstream}>Official upstream</a>
          <p>
            Acquisition:{' '}
            {selected.availability === 'available'
              ? 'Available'
              : 'Pending recipe review'}
          </p>
          <Selection
            product={selected}
            rows={rows.filter((r) => r.product === selected.id)}
            act={act}
          />
          <details>
            <summary>Licensing and provenance</summary>
            <p>{selected.rights}</p>
            <p>
              Files are acquired directly from upstream. Operators are
              responsible for applicable license terms.
            </p>
          </details>
          <details>
            <summary>Technical details</summary>
            {selected.components.map((c) => (
              <p key={c.name}>
                {c.name} · {fileSize(c.bytes)} · <code>{c.sha256}</code>
              </p>
            ))}
          </details>
        </section>
      )}
    </>
  )
}
