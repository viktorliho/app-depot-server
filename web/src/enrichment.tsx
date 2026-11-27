import { useState } from 'react'
import { apiRequest, jsonBody, type DepotApp } from './api'
import { ProblemNotice } from './components'
import { useAuth } from './portal'
import { useResource } from './use-resource'

const roles = [
  'steam.grid.vertical',
  'steam.grid.horizontal',
  'steam.hero',
  'logo',
  'icon',
] as const
const labels: Record<string, string> = {
  'steam.grid.vertical': 'Portrait cover',
  'steam.grid.horizontal': 'Horizontal grid',
  'steam.hero': 'Background',
  logo: 'Logo',
  icon: 'Application icon',
}
const toggle = (items: string[], key: string) =>
  items.includes(key) ? items.filter((v) => v !== key) : [...items, key]

export function FetchMetadata({
  app,
  existing,
  changed,
}: {
  app: DepotApp
  existing: Record<string, unknown> | null
  changed: () => void
}) {
  const { state } = useAuth()
  const [query, setQuery] = useState(app.name),
    [matches, setMatches] = useState<{ id: number; name: string }[]>()
  const [id, setId] = useState<number>(),
    [details, setDetails] = useState<Record<string, unknown>>(),
    [fields, setFields] = useState<string[]>([]),
    [images, setImages] = useState<string[]>([]),
    [missing, setMissing] = useState<string[]>([])
  const [assets, setAssets] = useState<Record<string, number>>({})
  const [alternatives, setAlternatives] = useState<Record<string, number[]>>({})
  const [busy, setBusy] = useState(false),
    [error, setError] = useState<unknown>(),
    [notice, setNotice] = useState('')
  async function run(f: () => Promise<void>) {
    setBusy(true)
    setError(undefined)
    setNotice('')
    try {
      await f()
    } catch (e) {
      setError(e)
    } finally {
      setBusy(false)
    }
  }
  async function choose(candidate: number) {
    setId(candidate)
    setDetails(undefined)
    setMissing([])
    setImages([])
    setAssets({})
    setAlternatives({})
    const { value } = await apiRequest<Record<string, unknown>>(
      '/metadata/steam/' + candidate,
      { authenticated: true }
    )
    setDetails(value)
    setFields(Object.keys(value).filter((k) => existing?.[k] == null))
  }
  return (
    <details className="technical-details">
      <summary>Fetch metadata</summary>
      <form
        className="form-stack form-panel"
        onSubmit={(e) => {
          e.preventDefault()
          void run(async () => {
            const r = await apiRequest<{ id: number; name: string }[]>(
              '/metadata/steam/search?q=' + encodeURIComponent(query),
              { authenticated: true }
            )
            setMatches(r.value)
            setDetails(undefined)
          })
        }}
      >
        <label>
          Application name or Steam App ID
          <input
            required
            maxLength={160}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </label>
        <button className="button" disabled={busy}>
          Search providers
        </button>
      </form>
      {matches && !matches.length && (
        <p>No matches. Try an alternative name or Steam App ID.</p>
      )}
      {matches?.map((m) => (
        <button
          className="button"
          key={m.id}
          disabled={busy}
          onClick={() => run(() => choose(m.id))}
        >
          {m.name} · {m.id}
        </button>
      ))}
      {id && details && (
        <>
          <fieldset>
            <legend>Metadata fields</legend>
            {Object.entries(details).map(([k, v]) => (
              <label key={k} className="checkbox-row">
                <input
                  type="checkbox"
                  checked={fields.includes(k)}
                  onChange={() => setFields(toggle(fields, k))}
                />
                {k} · {String(v)}{' '}
                {existing?.[k] != null && '(replace existing)'}
              </label>
            ))}
          </fieldset>
          <fieldset>
            <legend>Artwork to apply or replace</legend>
            {roles.map((role) => {
              const asset = assets[role]
              const src = '/api/v1/metadata/steam/' + id + '/artwork/' + role
              return (
                <div key={role}>
                  {!missing.includes(role) && (
                    <label>
                      <input
                        type="checkbox"
                        checked={images.includes(role)}
                        onChange={() => setImages(toggle(images, role))}
                      />
                      {labels[role]}{' '}
                      <img
                        width="96"
                        alt=""
                        src={src + (asset ? '?asset=' + asset : '')}
                        onError={() => {
                          setMissing((v) => [...v, role])
                          setImages((v) => v.filter((x) => x !== role))
                        }}
                      />
                    </label>
                  )}
                  <details
                    onToggle={(e) => {
                      if (!e.currentTarget.open || alternatives[role]) return
                      void run(async () => {
                        const result = await apiRequest<number[]>(
                          '/metadata/steam/' + id + '/candidates/' + role,
                          { authenticated: true }
                        )
                        setAlternatives((previous) => ({
                          ...previous,
                          [role]: result.value,
                        }))
                      })
                    }}
                  >
                    <summary>
                      Alternative {labels[role].toLowerCase()} images
                    </summary>
                    {alternatives[role]?.length === 0 && (
                      <p>No additional images available.</p>
                    )}
                    {alternatives[role]?.map((candidate, i) => (
                      <button
                        type="button"
                        key={candidate}
                        aria-label={'Select ' + labels[role] + ' ' + (i + 1)}
                        aria-pressed={asset === candidate}
                        onClick={() => {
                          setAssets((previous) => ({
                            ...previous,
                            [role]: candidate,
                          }))
                          setMissing((previous) =>
                            previous.filter((r) => r !== role)
                          )
                          setImages((previous) =>
                            previous.includes(role)
                              ? previous
                              : [...previous, role]
                          )
                        }}
                      >
                        <img
                          loading="lazy"
                          width="72"
                          alt=""
                          src={src + '?asset=' + candidate}
                        />
                      </button>
                    ))}
                  </details>
                </div>
              )
            })}
          </fieldset>
          <button
            className="button button-primary"
            disabled={busy || (!fields.length && !images.length)}
            onClick={() =>
              run(async () => {
                await apiRequest('/apps/' + app.id + '/metadata/apply', {
                  method: 'POST',
                  authenticated: true,
                  csrf: state.session?.csrfToken,
                  ...jsonBody({ id, fields, artwork: images, assets }),
                })
                changed()
                setNotice('Selected metadata and artwork saved.')
              })
            }
          >
            Apply selected items
          </button>
        </>
      )}
      {notice && <p role="status">{notice}</p>}
      {!!error && <ProblemNotice error={error} />}
    </details>
  )
}
type Config = { steam: boolean; steamGridDb: boolean; hasKey: boolean }
export function MetadataSettings() {
  const { state } = useAuth()
  const cfg = useResource('metadata-providers', (signal) =>
    apiRequest<Config>('/admin/metadata-providers', {
      authenticated: true,
      signal,
    }).then((r) => r.value)
  )
  const [key, setKey] = useState(''),
    [error, setError] = useState<unknown>(),
    [msg, setMsg] = useState('')
  async function submit(
    values: Partial<Config> & { key?: string },
    test = false
  ) {
    setError(undefined)
    try {
      await apiRequest('/admin/metadata-providers' + (test ? '/test' : ''), {
        method: test ? 'POST' : 'PUT',
        authenticated: true,
        csrf: state.session?.csrfToken,
        ...(test ? {} : jsonBody(values)),
      })
      setKey('')
      setMsg(test ? 'Connection verified.' : 'Provider settings saved.')
      cfg.retry()
    } catch (e) {
      setError(e)
    }
  }
  return (
    <section className="panel form-panel">
      <h2>Metadata providers</h2>
      {cfg.value && (
        <div className="form-stack">
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={cfg.value.steam}
              onChange={(e) => submit({ steam: e.target.checked })}
            />
            Steam Store and Steam artwork
          </label>
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={cfg.value.steamGridDb}
              onChange={(e) => submit({ steamGridDb: e.target.checked })}
            />
            SteamGridDB artwork
          </label>
          <label>
            SteamGridDB API key
            <input
              type="password"
              autoComplete="new-password"
              value={key}
              placeholder={cfg.value.hasKey ? 'Key configured' : 'Optional'}
              onChange={(e) => setKey(e.target.value)}
            />
          </label>
          <div className="form-actions">
            <button
              className="button"
              disabled={!key}
              onClick={() => submit({ key })}
            >
              Save key
            </button>
            {cfg.value.hasKey && (
              <button className="button" onClick={() => submit({ key: '' })}>
                Remove key
              </button>
            )}
            <button className="button" onClick={() => submit({}, true)}>
              Test connection
            </button>
          </div>
        </div>
      )}
      {msg && <p role="status">{msg}</p>}
      {!!(cfg.error || error) && <ProblemNotice error={cfg.error || error} />}
    </section>
  )
}
