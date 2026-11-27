import { useEffect, useState } from 'react'
import { apiRequest, jsonBody } from './api'
import { PageHeader, ProblemNotice, LoadingState, StatusBadge } from './components'
import { useAuth } from './portal'
import { useResource } from './use-resource'

type Recipe = {
  id: string; title: string; version: string; revision: number; rights: string
  upstream: string; platform: string; availability: string
  components: { name: string; bytes: number; sha256: string }[]
}
type Catalog = { generation: number; recipes: Recipe[] }

type Observation = {
  id: string; title: string; revision: number; app?: string; enabled: boolean; automatic: boolean
  filesDone: number; filesTotal: number; bytesDone: number; bytesTotal: number
  hours: number; status: string; lastCheck?: number; detected?: string; error?: string
}

function ObservedApplication({ row, changed }: { row: Observation; changed: () => void }) {
  const { state } = useAuth()
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)
  const [hours, setHours] = useState(String(row.hours))
  const [enabled, setEnabled] = useState(row.enabled)
  const [automatic, setAutomatic] = useState(row.automatic)
  async function submit(action: 'check' | 'observation' | 'update') {
    setBusy(true)
    setError(undefined)
    try {
      await apiRequest('/admin/store/' + encodeURIComponent(row.id) + '/' + action, {
        method: action === 'observation' ? 'PUT' : 'POST', csrf: state.session?.csrfToken, authenticated: true,
        ...(action === 'observation' ? jsonBody({ hours: Number(hours), enabled, automatic }) : {})
      })
      changed()
    } catch (problem) { setError(problem) } finally { setBusy(false) }
  }
  return <section className="panel"><h3>{row.app ? <a href={'/apps/' + row.app}>{row.title}</a> : row.title}</h3>
    <p><StatusBadge value={row.status} /> · Recipe revision {row.revision}{row.detected && ' · ' + row.detected}</p>
    {row.status === 'Importing' && row.bytesTotal > 0 && <p><progress value={row.bytesDone} max={row.bytesTotal} /> {row.filesDone} of {row.filesTotal} files</p>}
    {row.lastCheck && <p>Last checked {new Date(row.lastCheck * 1000).toLocaleString()}</p>}
    {row.error && <p role="alert">{row.error}</p>}
    <form className="form-stack form-panel" onSubmit={event => { event.preventDefault(); void submit('observation') }}>
      <label className="checkbox-row"><input type="checkbox" checked={enabled} onChange={event => setEnabled(event.target.checked)} /> Periodic checks</label>
      <label>Check interval (hours) <input type="number" min="1" max="8760" required value={hours} onChange={event => setHours(event.target.value)} /></label>
      <label className="checkbox-row"><input type="checkbox" checked={automatic} onChange={event => setAutomatic(event.target.checked)} /> Publish verified updates automatically</label>
      <button className="button" disabled={busy}>Save settings</button>
      <button className="button" type="button" disabled={busy} onClick={() => submit('check')}>Check for updates</button>
      {row.status === 'Update available' && <button className="button button-primary" type="button" disabled={busy} onClick={() => submit('update')}>Publish this update</button>}
    </form>{!!error && <ProblemNotice error={error} />}</section>
}

export function StorePage() {
  const { state } = useAuth()
  const catalog = useResource('store', async signal =>
    (await apiRequest<Catalog>('/admin/store', { authenticated: true, signal })).value)
  const [query, setQuery] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const [observations, setObservations] = useState<Observation[]>([])
  const [revision, setRevision] = useState(0)
  const changed = () => setRevision(previous => previous + 1)
  useEffect(() => {
    const controller = new AbortController()
    let timeout: ReturnType<typeof setTimeout>
    async function poll() {
      try {
        const response = await apiRequest<Observation[]>('/admin/store/observations', { authenticated: true, signal: controller.signal })
        if (!controller.signal.aborted) setObservations(response.value)
      } catch (problem) { if (!controller.signal.aborted) setError(problem) }
      if (!controller.signal.aborted) timeout = setTimeout(poll, 3000)
    }
    void poll()
    return () => { controller.abort(); clearTimeout(timeout) }
  }, [revision])
  async function add(id: string, action = 'add') {
    setBusy(true)
    setError(undefined)
    try {
      await apiRequest('/admin/store/' + encodeURIComponent(id) + '/' + action, {method: 'POST', authenticated: true, csrf: state.session?.csrfToken})
      changed()
    } catch (problem) { setError(problem) } finally { setBusy(false) }
  }
  async function refresh() {
    setBusy(true)
    setError(undefined)
    try {
      await apiRequest('/admin/store/refresh', { method: 'POST', csrf: state.session?.csrfToken, authenticated: true })
      catalog.retry()
    } catch (problem) { setError(problem) }
    finally { setBusy(false) }
  }
  const matches = catalog.value?.recipes.filter(recipe => recipe.title.toLowerCase().includes(query.trim().toLowerCase())) || []
  return <><PageHeader title="Store" actions={<button className="button button-secondary" disabled={busy} onClick={refresh}>{busy ? 'Refreshing…' : 'Refresh catalog'}</button>} />
    {!!error && <ProblemNotice error={error} />}
    {catalog.error && <ProblemNotice error={catalog.error} retry={catalog.retry} />}
    {catalog.loading && <LoadingState label="Loading catalog…" />}
    {catalog.value && <section className="panel recipe-panel">
      <p>{catalog.value.generation ? 'Catalog generation ' + catalog.value.generation : 'Refresh the catalog to browse recipes.'}</p>
      <p>Files are acquired directly from upstream. App Depot grants no additional rights to use or redistribute software. Operators are responsible for compliance with applicable license terms.</p>
      <label>Search applications <input type="search" value={query} onChange={event => setQuery(event.target.value)} /></label>
      {query && !matches.length && <p>No matching applications.</p>}
      {matches.map(recipe =>
        <details className="technical-details" key={recipe.id}><summary>{recipe.title}</summary>
          <p>Upstream version {recipe.version} · {recipe.platform} · Recipe revision {recipe.revision}</p>
          <p>Acquisition: {recipe.availability === 'available' ? 'Available' : 'Pending recipe review'}</p>
          {observations.filter(row => row.id === recipe.id).map(row => <p key={row.id}>Observation: <StatusBadge value={row.status} /> · Periodic checks {row.enabled ? 'on' : 'off'}</p>)}
          <p>Upstream source: <a href={recipe.upstream} rel="noreferrer">{recipe.upstream}</a></p>
          <p><strong>Licensing</strong> · {recipe.rights}</p>
          {recipe.availability === 'available' &&
            <button className="button button-primary" disabled={busy || observations.some(row => row.id === recipe.id)} onClick={() => add(recipe.id)}>Add to server</button>}
          {observations.some(row => row.id === recipe.id && row.revision < recipe.revision) && recipe.availability === 'available' &&
            <button className="button" disabled={busy} onClick={() => add(recipe.id, 'adopt')}>Apply recipe revision {recipe.revision}</button>}
          <ul>{recipe.components.map(item => <li key={item.name}>{item.name} · {item.bytes.toLocaleString()} bytes · SHA-256 <code>{item.sha256}</code></li>)}</ul>
        </details>)}
    </section>}
    {observations.length > 0 && <><h2>Observed applications</h2>{observations.map(row => <ObservedApplication key={row.id} row={row} changed={changed} />)}</>}
  </>
}
