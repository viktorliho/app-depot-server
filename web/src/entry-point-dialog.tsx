import { platformName } from './installation-review'
import { useEffect, useRef, useState } from 'react'
import { ApiError, apiRequest, idempotencyKey, jsonBody, resource, type Artifact, type Declaration } from './api'
import { ArtifactPicker } from './artifact-browser'
import { EmptyState, LoadingState, ProblemNotice } from './components'
import { useAuth } from './portal'
import { depot, manifest, record, strings, text } from './release-information'
import { useResource } from './use-resource'

const enc = encodeURIComponent
const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === 'object' && !Array.isArray(value)
type Entry = Record<string, unknown>
type Assessment = { status: 'Absent' | 'Valid' | 'Issues' | 'Unsupported'; launchStatus: string; selectionRequiredFor: string[];
  items: { index: number; id: string; status: string; launchStatus: string; launchReason: string | null; effectivePlatforms: string[] }[] }

function readEntries(data: unknown): Entry[] | null {
  if (!object(data)) return null
  if (data.appDepot === undefined) return []
  if (!object(data.appDepot)) return null
  if (data.appDepot.entryPoints === undefined) return []
  if (!Array.isArray(data.appDepot.entryPoints) || data.appDepot.entryPoints.some(item => !object(item))) return null
  return data.appDepot.entryPoints as Entry[]
}

function withEntries(data: unknown, entries: Entry[]): unknown {
  if (!object(data) || (data.appDepot !== undefined && !object(data.appDepot))) throw new Error('Use the raw declaration editor for this JSON value.')
  const block = { ...(object(data.appDepot) ? data.appDepot : {}) }
  if (entries.length) block.entryPoints = entries
  else delete block.entryPoints
  const result = { ...data }
  if (Object.keys(block).length) result.appDepot = block
  else delete result.appDepot
  return result
}

function targetLabel(entry: Entry) {
  const target = entry.target
  if (!object(target)) return 'Unrecognized target'
  if (target.type === 'ArtifactFile') return 'This single-file Artifact'
  if (target.type === 'TreeFile') return typeof target.path === 'string' ? target.path : 'Invalid path'
  return 'Unrecognized target'
}

export type EntryTarget = { type: 'TreeFile'; path: string } | { type: 'ArtifactFile' }

export function EntryPointDialog({ item, releaseId, associationId, allowed, target, close, saved }: {
  item: Artifact; releaseId: string; associationId: string; allowed: boolean; target?: EntryTarget; close: () => void; saved: () => void
}) {
  const { state } = useAuth()
  const dialog = useRef<HTMLDialogElement>(null)
  const newId = useRef(idempotencyKey())
  const [revision, setRevision] = useState(0)
  const [entries, setEntries] = useState<Entry[]>([])
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const [notice, setNotice] = useState('')
  const releaseDeclaration = useResource(`explorer-release-declaration:${releaseId}`, signal =>
    apiRequest<Declaration>(`/releases/${enc(releaseId)}/declarations`, { authenticated: true, signal }).then(result => result.value))
  const declaration = useResource(`explorer-declaration:${associationId}:${revision}`, signal =>
    resource<Declaration>(`/associations/${enc(associationId)}/declarations`, signal))
  const validation = useResource(`entry-assessment:${declaration.value?.value.revisionId || ''}`, signal => declaration.value
    ? apiRequest<Assessment>(`/associations/${enc(associationId)}/entry-points/validate`, { method: 'POST', authenticated: true, signal,
      ...jsonBody({ data: declaration.value.value.data }) }).then(result => result.value)
    : Promise.resolve(null))
  const structured = declaration.value ? readEntries(declaration.value.value.data) !== null : false
  useEffect(() => { dialog.current?.showModal() }, [])
  useEffect(() => {
    if (!declaration.value) return
    const points = readEntries(declaration.value.value.data) || []
    setEntries(allowed && target ? [...points, { id: newId.current, target }] : points)
  }, [declaration.value?.value.revisionId, revision, allowed, target])

  function changeEntry(index: number, patch: Entry) {
    setEntries(current => current.map((item, position) => position === index ? { ...item, ...patch } : item))
  }
  function changeLaunch(index: number, patch: Record<string, unknown>) {
    setEntries(current => current.map((item, position) => position === index ? { ...item, launch: { ...(object(item.launch) ? item.launch : {}), ...patch } } : item))
  }
  async function save() {
    setBusy(true); setError(undefined); setNotice('')
    try {
      const data = withEntries(declaration.value!.value.data, entries)
      const check = await apiRequest<Assessment>(`/associations/${enc(associationId)}/entry-points/validate`, { method: 'POST', authenticated: true, ...jsonBody({ data }) })
      if (check.value.status !== 'Valid' && check.value.status !== 'Absent') throw new Error(`Entry points need correction: ${check.value.items.filter(item => item.status !== 'Valid').map(item => `#${item.index + 1} ${item.status}`).join(', ') || check.value.status}.`)
      const invalidLaunches = check.value.items.filter(item => {
        const launch = entries[item.index]?.launch
        return object(launch) && launch.method === 'NativePortableV1' && item.launchStatus !== 'Valid'
      })
      if (invalidLaunches.length) throw new Error(`Portable launch needs correction: ${invalidLaunches.map(item => `#${item.index + 1} ${item.launchReason || item.launchStatus}`).join(', ')}.`)
      await apiRequest(`/associations/${enc(associationId)}/declarations`, { method: 'PUT', authenticated: true, csrf: state.session!.csrfToken,
        ifMatch: declaration.value!.etag, ...jsonBody({ data }) })
      dialog.current?.close()
      saved()
    } catch (failure) { setError(failure) } finally { setBusy(false) }
  }

  const source = manifest(declaration.value?.value.data)
  const candidate = text(record(source?.user)?.candidateExecutable)
  const showCandidate = !!candidate && item.kind === 'Tree' &&
    strings(record(source?.detected)?.candidateExecutables).includes(candidate) &&
    !entries.some(entry => record(entry.target)?.path === candidate)
  const releasePlatforms = strings(depot(releaseDeclaration.value?.data)?.platforms)
  const windowsTargets = ['windows/x86_64', 'windows/aarch64'].filter(platform => !releasePlatforms.length || releasePlatforms.includes(platform))
  function useCandidate() {
    setEntries(current => [...current, {
      id: current.some(entry => entry.id === 'gog-launch') ? idempotencyKey() : 'gog-launch',
      label: Array.from(candidate).length <= 160 ? candidate : undefined, target: { type: 'TreeFile', path: candidate },
      platforms: [windowsTargets[0]], launch: { method: 'NativePortableV1' },
    }])
    setNotice('Review the new point and save the declaration to apply it.')
  }

  return <dialog className="entry-point-dialog" ref={dialog} onClose={close} onCancel={event => { if (busy) event.preventDefault() }} aria-labelledby="entry-points-heading">
    <div className="entry-point-header"><h2 id="entry-points-heading">Entry points</h2>
      <button type="button" className="button button-secondary" disabled={busy} onClick={() => dialog.current?.close()}>Close editor</button>
    </div>
    {declaration.loading ? <LoadingState label="Loading saved entry points…" /> : declaration.error ?
      <ProblemNotice error={declaration.error} retry={declaration.retry} /> : <>
      {validation.loading ? <LoadingState label="Checking saved references…" /> : validation.error ? <ProblemNotice error={validation.error} retry={validation.retry} /> : validation.value && <p role="status">Saved references: {validation.value.status}{validation.value.items.some(value => value.status !== 'Valid') && ` (${validation.value.items.filter(value => value.status !== 'Valid').map(value => `#${value.index + 1} ${value.status}`).join(', ')})`}</p>}
      {validation.value && <p role="status">Portable launches: {validation.value.launchStatus}{validation.value.selectionRequiredFor.length > 0 && ` · explicit selection required for ${validation.value.selectionRequiredFor.join(', ')}`}</p>}
      {allowed && structured && showCandidate && !releaseDeclaration.loading && !releaseDeclaration.error && (
        windowsTargets.length ? <button className="button button-secondary" onClick={useCandidate}>Create entry point from GOG candidate</button> :
          <p>Declare a supported Windows platform for this release before using the GOG candidate.</p>
      )}
      {!structured && <p role="alert">This declaration does not use an editable entry-point structure. Its JSON is preserved. Use the raw declaration editor on the Release page.</p>}
      {structured && !entries.length && <EmptyState title="No entry points" detail="Select a file in the Explorer to add an entry point." />}
      <fieldset disabled={busy} className="entry-point-forms">{structured && entries.map((entry, index) => {
        const launch = object(entry.launch) ? entry.launch : null
        return <div className="form-panel" key={String(entry.id || index)}><h3>{text(entry.label) || targetLabel(entry)}</h3><p>Launch executable · <code>{targetLabel(entry)}</code></p>
        <label>Label (optional)<input value={typeof entry.label === 'string' ? entry.label : ''} readOnly={!allowed} onChange={event => changeEntry(index, { label: event.target.value || undefined })} /></label>
        {allowed && <label>Add target platform<select value="" onChange={event => {
          if (event.target.value) changeEntry(index, { platforms: [...strings(entry.platforms), event.target.value] })
        }}><option value="">Choose a platform</option>
          {['windows/x86_64', 'windows/aarch64', 'linux/x86_64', 'linux/aarch64', 'macos/x86_64', 'macos/aarch64']
            .filter(platform => !strings(entry.platforms).includes(platform))
            .map(platform => <option key={platform} value={platform}>{platformName(platform)}{releasePlatforms.includes(platform) ? ' · Release target' : ''}</option>)}
        </select></label>}
        <div className="identity-line">{strings(entry.platforms).map(platform => <span key={platform}>
          {platformName(platform)} {allowed && <button type="button" className="text-button" aria-label={`Remove ${platformName(platform)} target`}
            onClick={() => changeEntry(index, { platforms: strings(entry.platforms).filter(value => value !== platform) })}>Remove</button>}
        </span>)}</div>
        <p className="subtle">These targets describe the executable OS and architecture. The client chooses its runtime.</p>
        <details><summary>Advanced entry point details</summary>
          <p><code>Entry ID: {String(entry.id)}</code></p>
        <label>Target operating systems and CPU architectures (one per line; optional)<textarea rows={3} value={Array.isArray(entry.platforms) ? entry.platforms.join('\n') : ''} readOnly={!allowed} placeholder={'linux/x86_64\nwindows/x86_64'} onChange={event => changeEntry(index, { platforms: event.target.value.split('\n').map(s => s.trim()).filter(Boolean) })} /></label>
        </details>
        {entry.launch !== undefined && (!launch || launch.method !== 'NativePortableV1') ?
          <p role="status">This entry has an unsupported launch declaration. It remains unchanged here; use Advanced JSON to inspect or replace it.</p> : <>
          <label>Execution method<select value={entry.launch === undefined ? 'none' : 'native'} disabled={!allowed} onChange={event => changeEntry(index, { launch: event.target.value === 'native' ? launch && launch.method === 'NativePortableV1' ? launch : { method: 'NativePortableV1' } : undefined })}>
            <option value="none">File reference only — no launch</option><option value="native">Native portable program (v1)</option></select></label>
          {launch && launch.method === 'NativePortableV1' && <>
            <p className="subtle">A compatible client can directly run this verified target on a listed platform. No installer, shell command or dependency setup is declared.</p>
            <label>Working directory<select value={launch.workingDirectory === undefined ? 'target' : launch.workingDirectory === '' ? 'root' : 'custom'} disabled={!allowed} onChange={event => {
              const parent = targetLabel(entry).split('/').slice(0, -1).join('/')
              changeLaunch(index, { workingDirectory: event.target.value === 'target' ? undefined : event.target.value === 'root' ? '' : parent || '.' })
            }}>
              <option value="target">Target file's directory (default)</option><option value="root">Artifact root</option>{item.kind === 'Tree' && <option value="custom">Specific Tree directory</option>}</select></label>
            {typeof launch.workingDirectory === 'string' && launch.workingDirectory !== '' && <><p>Selected working directory <code>{launch.workingDirectory === '.' ? 'Choose a folder' : launch.workingDirectory}</code></p>
              {allowed && <ArtifactPicker artifactId={item.id} mode="directory" label="Browse working directories" choose={path => changeLaunch(index, { workingDirectory: path })} />}</>}
            <fieldset><legend>Literal process arguments</legend><p className="subtle">Each field is one argument; an empty field is an empty argument. No shell or variable expansion.</p>
              {Array.isArray(launch.arguments) && launch.arguments.map((argument, argIndex) => <div className="form-actions" key={argIndex}><label>Argument {argIndex + 1}<input value={typeof argument === 'string' ? argument : ''} readOnly={!allowed} onChange={event => changeLaunch(index, { arguments: (launch.arguments as unknown[]).map((value, position) => position === argIndex ? event.target.value : value) })} /></label>{allowed && <button className="button button-secondary" onClick={() => changeLaunch(index, { arguments: (launch.arguments as unknown[]).filter((_, position) => position !== argIndex) })}>Remove argument</button>}</div>)}
              {allowed && <button className="button button-secondary" onClick={() => changeLaunch(index, { arguments: [...(Array.isArray(launch.arguments) ? launch.arguments : []), ''] })}>Add argument</button>}
            </fieldset>
            {validation.value?.items[index] && <p className="subtle">Saved launch: {validation.value.items[index].launchStatus}{validation.value.items[index].launchReason && ` (${validation.value.items[index].launchReason})`}</p>}
          </>}
        </>}

        {allowed && <div className="form-stack">
          {item.kind === 'Tree' && <ArtifactPicker artifactId={item.id} mode="file" label="Change target using Explorer" choose={path => {
            const previous = record(entry.target)
            changeEntry(index, { target: { ...(previous?.type === 'TreeFile' ? previous : {}), type: 'TreeFile', path } })
          }} />}
          <button type="button" className="button button-danger" onClick={() => setEntries(current => current.filter((_, position) => position !== index))}>Remove entry point</button>
        </div>}

      </div>})}</fieldset>
      {error !== undefined && <ProblemNotice error={error} />}
      {error instanceof ApiError && error.status === 412 && <button className="button button-secondary" onClick={() => {
        setError(undefined)
        setNotice('')
        setRevision(value => value + 1)
      }}>Reload saved entry points</button>}{notice && <p role="status" className="success-notice">{notice}</p>}
      {allowed && structured && <button className="button button-primary" disabled={busy} onClick={() => void save()}>{busy ? 'Saving…' : 'Save entry points'}</button>}

    </>}
  </dialog>
}
