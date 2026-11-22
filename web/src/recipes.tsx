import { useId, useRef, useState } from 'react'
import { apiRequest, idempotencyKey, jsonBody, resource, type AppPage, type Association, type Channel, type DeclarationSnapshot, type DepotApp, type InventoryEntry, type Page, type Release } from './api'
import { ArtworkThumbnail } from './artwork'
import { LoadingState, ProblemNotice } from './components'
import { useAuth } from './portal'
import { useResource } from './use-resource'

const enc = encodeURIComponent
const platforms = ['linux/x86_64', 'linux/aarch64', 'windows/x86_64', 'windows/aarch64', 'macos/x86_64', 'macos/aarch64']
const platformLabel = (platform: string) => {
  const [os, arch] = platform.split('/')
  const names: Record<string, string> = { linux: 'Linux', windows: 'Windows', macos: 'macOS' }
  return `${names[os]} (${arch})`
}
type Selector = { type: 'latest' } | { type: 'release'; releaseId: string } | { type: 'channel'; name: string }
type Dependency = { appId: string; selector: Selector; platforms?: string[] }
type Copy = { dependency: string; associationId?: string; associationLabel?: string; source: string; destination: string; executable: boolean; collision: 'reject' | 'replaceFile' }
type Target = { operations: Copy[]; launch: { path: string; workingDirectory: string; arguments: string[] } }
type Recipe = { version: 1; dependencies: Record<string, Dependency>; targets: Record<string, Target> }
type Layout = { files: string[]; directories: string[] }
type Preview = Layout & { launchCandidates: string[] }
type Lock = { releases: Record<string, { appId: string; version: string | null; dependencies: Record<string, string> }>; files: Record<string, unknown>; directories: string[] }

const newTarget = (): Target => ({ operations: [], launch: { path: '', workingDirectory: '', arguments: [] } })
const initialRecipe = (): Recipe => ({ version: 1, dependencies: {}, targets: { [platforms[0]]: newTarget() } })
const newOperation = (dependency: string): Copy => ({ dependency, source: '', destination: '', executable: false, collision: 'reject' })

const read = <T,>(path: string, signal?: AbortSignal) => apiRequest<T>(path, { authenticated: true, signal }).then(result => result.value)
function usePage<T>(path: string) {
  const [position, setPosition] = useState<{ path: string; cursor: string }>({ path, cursor: '' })
  const cursor = position.path === path ? position.cursor : ''
  const page = useResource(`${path}:${cursor}`, signal => path
    ? read<Page<T>>(`${path}${path.includes('?') ? '&' : '?'}pageSize=25${cursor ? `&cursor=${enc(cursor)}` : ''}`, signal)
    : Promise.resolve({ items: [], nextCursor: null }))
  const controls = <div className="form-actions">
    {cursor && <button type="button" className="text-button" onClick={() => setPosition({ path, cursor: '' })}>First page</button>}
    {page.value?.nextCursor && <button type="button" className="text-button" onClick={() => setPosition({ path, cursor: page.value!.nextCursor! })}>More results</button>}
  </div>
  return { ...page, controls }
}

function FindDependency({ exclude, add }: { exclude: string; add: (app: DepotApp) => void }) {
  const [draft, setDraft] = useState('')
  const [query, setQuery] = useState('')
  const [open, setOpen] = useState(false)
  const apps = usePage<AppPage['items'][number]>(open ? `/apps?q=${enc(query)}` : '')
  return <details onToggle={event => setOpen(event.currentTarget.open)}>
    <summary>Add dependency application</summary>
    <form className="form-stack" onSubmit={event => { event.preventDefault(); setQuery(draft.trim()) }}>
      <label>Search dependency applications<input value={draft} onChange={event => setDraft(event.target.value)} /></label>
      <button className="button button-secondary">Search</button>
    </form>
    {apps.error ? <ProblemNotice error={apps.error} retry={apps.retry} /> : apps.loading ? <LoadingState /> : <ul className="compact-list">
      {apps.value!.items.filter(app => app.id !== exclude).map(app => <li key={app.id}>
        <span className="app-list-identity"><ArtworkThumbnail app={app} />{app.name}</span>
        <button type="button" className="button button-secondary" onClick={() => add(app)} aria-label={`Add ${app.name}`}>Add</button>
      </li>)}
    </ul>}
    {apps.controls}
  </details>
}

function DependencyPolicy({ dependency, change, remove }: { dependency: Dependency; change: (selector: Selector) => void; remove: () => void }) {
  const { appId, selector } = dependency
  const app = useResource(`dependency:${appId}`, signal => read<DepotApp>(`/apps/${appId}`, signal))
  const channels = usePage<Channel>(selector.type === 'channel' ? `/apps/${appId}/channels` : '')
  const releases = usePage<Release>(selector.type === 'release' ? `/apps/${appId}/releases?order=published` : '')
  const chosen = useResource(`dependency-release:${appId}:${JSON.stringify(selector)}`, async signal => {
    if (selector.type === 'latest') return { release: (await read<Page<Release>>(`/apps/${appId}/releases?order=published&pageSize=1`, signal)).items[0] }
    if (selector.type === 'release' && selector.releaseId) return { release: await read<Release>(`/releases/${selector.releaseId}`, signal) }
    if (selector.type === 'channel' && selector.name) {
      const channel = await read<Channel>(`/apps/${appId}/channels/${enc(selector.name)}`, signal)
      return { release: await read<Release>(`/releases/${channel.releaseId}`, signal) }
    }
    return {}
  })
  return <fieldset className="form-stack">
    <legend>{app.value?.name || 'Dependency application'}</legend>
    {app.value && <ArtworkThumbnail app={app.value} />}
    {app.error !== undefined && <ProblemNotice error={app.error} retry={app.retry} />}
    <label>Release policy<select value={selector.type} onChange={event => {
      const type = event.target.value
      change(type === 'release' ? { type, releaseId: '' } : type === 'channel' ? { type, name: '' } : { type: 'latest' })
    }}>
      <option value="latest">Latest published release</option><option value="channel">Follow a channel</option><option value="release">Pin a release</option>
    </select></label>
    {selector.type === 'release' && <><label>Published release<select aria-label="Published release" value={selector.releaseId} onChange={event => change({ type: 'release', releaseId: event.target.value })}>
      <option value="">Choose a release</option>{selector.releaseId && !releases.value?.items.some(item => item.id === selector.releaseId) && <option value={selector.releaseId}>{chosen.value?.release?.version || 'Selected unversioned release'}</option>}
      {releases.value?.items.map((release, i) => <option key={release.id} value={release.id}>{release.version || `Unversioned release ${i + 1} on this page`}</option>)}
    </select></label>{releases.controls}</>}
    {selector.type === 'channel' && <><label>Channel<select value={selector.name} onChange={event => change({ type: 'channel', name: event.target.value })}>
      <option value="">Choose a channel</option>{selector.name && !channels.value?.items.some(item => item.name === selector.name) && <option>{selector.name}</option>}
      {channels.value?.items.map(channel => <option key={channel.name}>{channel.name}</option>)}
    </select></label>{channels.controls}</>}
    {(chosen.error || releases.error || channels.error) ? <ProblemNotice error={chosen.error || releases.error || channels.error} /> : null}
    {chosen.value?.release && <small>Currently selected: {chosen.value.release.version || 'Unversioned release'}</small>}
    <button type="button" className="text-button" onClick={remove}>Remove dependency</button>
  </fieldset>
}

function TreePicker({ artifactId, choose }: { artifactId: string; choose: (path: string, file: boolean) => void }) {
  const [parent, setParent] = useState('')
  const [open, setOpen] = useState(false)
  const entries = usePage<InventoryEntry>(open ? `/artifacts/${enc(artifactId)}/inventory?parent=${enc(parent)}` : '')
  return <details onToggle={event => setOpen(event.currentTarget.open)}>
    <summary>Browse source tree</summary>
    <p><strong>{parent || 'Artifact root'}</strong></p>
    <div className="form-actions">
      {parent && <button type="button" className="text-button" onClick={() => setParent(parent.split('/').slice(0, -1).join('/'))}>Parent directory</button>}
      <button type="button" className="button button-secondary" onClick={() => choose(parent, false)}>{parent ? 'Use this directory' : 'Use entire tree'}</button>
    </div>
    {entries.error ? <ProblemNotice error={entries.error} retry={entries.retry} /> : entries.loading ? <LoadingState /> : <ul className="compact-list">
      {entries.value!.items.map(entry => <li key={entry.path}><button type="button" className="text-button" onClick={() => entry.type === 'Directory' ? setParent(entry.path) : choose(entry.path, true)}>
        {entry.type === 'Directory' ? 'Open' : 'Use'} {entry.path.split('/').at(-1)}
      </button></li>)}
    </ul>}
    {entries.controls}
  </details>
}

function SourceControls({ dependency, operation, change, platform }: { dependency: Dependency; operation: Copy; change: (op: Copy) => void; platform: string }) {
  const { appId, selector } = dependency
  const app = useResource(`recipe-source-app:${appId}`, signal => read<DepotApp>(`/apps/${appId}`, signal))
  const selected = useResource(`recipe-source-release:${appId}:${JSON.stringify(selector)}`, async signal => {
    if (selector.type === 'release') return { id: selector.releaseId }
    if (selector.type === 'channel') return { id: selector.name ? (await read<Channel>(`/apps/${appId}/channels/${enc(selector.name)}`, signal)).releaseId : '' }
    return { id: (await read<Page<Release>>(`/apps/${appId}/releases?order=published&pageSize=1`, signal)).items[0]?.id }
  })
  const releaseId = selected.value?.id
  const associations = usePage<Association>(releaseId && app.value?.kind === 'standard' ? `/releases/${releaseId}/artifacts` : '')
  const snapshot = useResource(`recipe-source-snapshot:${releaseId || ''}`, signal => releaseId
    ? read<DeclarationSnapshot>(`/releases/${releaseId}/declaration-snapshot`, signal) : Promise.resolve(null))
  const items = associations.value?.items ?? []
  const labelsByAssociation = new Map(snapshot.value?.associations.flatMap(entry => {
    const data = entry.declaration.data as { appDepot?: { label?: unknown } } | undefined
    const label = data?.appDepot?.label
    return typeof label === 'string' && label.trim() ? [[entry.associationId, label] as const] : []
  }))
  const labels = [...new Set(items.map(item => labelsByAssociation.get(item.id)).filter((label): label is string => label !== undefined))]
  const labelMatches = operation.associationLabel
    ? [...labelsByAssociation].filter(([, label]) => label === operation.associationLabel)
    : []
  const selectedAssociation = operation.associationId || (labelMatches.length === 1 ? labelMatches[0][0] : '')
  const automatic = !operation.associationLabel && !operation.associationId
  const chosen = useResource(`recipe-source-association:${releaseId}:${selectedAssociation}`, signal => selectedAssociation
    ? read<Association>(`/associations/${selectedAssociation}`, signal) : Promise.resolve(null))
  const bound = selectedAssociation
    ? items.find(item => item.id === selectedAssociation) || (chosen.value?.releaseId === releaseId ? chosen.value : undefined)
    : automatic && items.length === 1 && !associations.value?.nextCursor ? items[0] : undefined
  const kind = (id: string) => id.startsWith('adtree:') ? 'Tree Artifact' : 'File Artifact'
  const choose = (source: string, file: boolean) => change({ ...operation, source, executable: file && operation.executable,
    destination: file && !operation.destination ? source.split('/').at(-1) || '' : operation.destination })
  return <>
    {(selected.error || associations.error || chosen.error) ? <ProblemNotice error={selected.error || associations.error || chosen.error} /> : null}
    {app.value?.kind === 'standard' && <><label>Source Artifact<select aria-label="Source Artifact" value={operation.associationLabel ? `label:${operation.associationLabel}` : operation.associationId ? `id:${operation.associationId}` : ''} onChange={event => {
      const value = event.target.value
      change({ ...operation, associationId: value.startsWith('id:') ? value.slice(3) : undefined,
        associationLabel: value.startsWith('label:') ? value.slice(6) : undefined, source: '', executable: false })
    }}>
      <option value="">Use the only Artifact</option>
      {operation.associationId && !items.some(item => item.id === operation.associationId) && <option value={`id:${operation.associationId}`}>{bound ? kind(bound.artifactId) : 'Previously selected Artifact (unavailable)'}</option>}
      {items.map((item, index) => <option key={item.id} value={`id:${item.id}`}>{labelsByAssociation.get(item.id) || `${kind(item.artifactId)} ${index + 1} on this page`}</option>)}
      {operation.associationLabel && !labels.includes(operation.associationLabel) && <option value={`label:${operation.associationLabel}`}>Follow label “{operation.associationLabel}”</option>}
      {labels.map(label => <option key={label} value={`label:${label}`}>Follow label “{label}”</option>)}
    </select></label>{associations.controls}
      {labelMatches.length > 1 && <small>Several Artifacts share this label. Choose an exact Artifact or a unique label.</small>}
      {operation.associationId && selector.type !== 'release' && <small>This exact Artifact is tied to the current release. If the release moves, update this selection or use a release with one Artifact.</small>}
      {bound?.artifactId.startsWith('adtree:') && <TreePicker key={bound.artifactId} artifactId={bound.artifactId} choose={choose} />}
    </>}
    {app.value?.kind === 'derived' && releaseId && <VirtualPicker releaseId={releaseId} platform={platform} choose={choose} />}
    {(app.value?.kind === 'derived' || bound?.artifactId.startsWith('adtree:')) && <label>Source path (blank for entire tree)<input value={operation.source} onChange={event => change({ ...operation, source: event.target.value })} /></label>}
  </>
}


function VirtualPicker({ releaseId, platform, choose }: { releaseId: string; platform: string; choose: (path: string, file: boolean) => void }) {
  const [open, setOpen] = useState(false)
  const layout = useResource(`virtual:${releaseId}:${platform}:${open}`, signal => open
    ? read<Lock>(`/releases/${releaseId}/recipe/lock?platform=${enc(platform)}`, signal) : Promise.resolve(null))
  return <details onToggle={event => setOpen(event.currentTarget.open)}>
    <summary>Browse Derived output</summary>
    {layout.error ? <ProblemNotice error={layout.error} retry={layout.retry} /> : layout.loading ? <LoadingState /> : layout.value &&
      <label>Use source entry<select value="" onChange={event => {
        const index = event.target.selectedIndex
        if (index > 0) choose(event.target.value === '/' ? '' : event.target.value, index > layout.value!.directories.length + 1)
      }}>
        <option value="">Choose a file or directory</option>
        <option value="/">Entire Derived output</option>
        {layout.value.directories.map(path => <option key={path} value={path}>{path}/</option>)}
        {Object.keys(layout.value.files).map(path => <option key={path} value={path}>{path}</option>)}
      </select></label>}
  </details>
}

function ComponentSummary({ lock, root }: { lock: Lock; root: string }) {
  return <ul className="compact-list">{Object.entries(lock.releases).filter(([id]) => id !== root).map(([id, release]) =>
    <ComponentVersion key={id} appId={release.appId} version={release.version} />)}</ul>
}

function ComponentVersion({ appId, version }: { appId: string; version: string | null }) {
  const application = useResource(`recipe-component:${appId}`, signal => read<DepotApp>(`/apps/${appId}`, signal))
  return <li>{application.value && <span className="app-list-identity"><ArtworkThumbnail app={application.value} />{application.value.name}</span>}
    {application.error !== undefined && <ProblemNotice error={application.error} />}
    <span>{version || 'Unversioned release'}</span></li>
}

function PublishedRecipe({ recipe, base, release }: { recipe: Recipe; base: string; release: Release }) {
  const [platform, setPlatform] = useState(Object.keys(recipe.targets)[0])
  const result = useResource(`recipe-lock:${base}:${platform}`, signal => read<Lock>(`${base}/lock?platform=${enc(platform)}`, signal))
  return <>
    <label>Installation target<select value={platform} onChange={event => setPlatform(event.target.value)}>
      {Object.keys(recipe.targets).map(value => <option key={value} value={value}>{platformLabel(value)}</option>)}
    </select></label>
    <h3>Declared dependencies</h3>
    <ul className="compact-list">{Object.entries(recipe.dependencies).map(([alias, dependency]) => <DeclaredComponent key={alias} dependency={dependency} />)}</ul>
    <h3>Currently resolved components</h3>
    {result.error ? <ProblemNotice error={result.error} retry={result.retry} /> : result.loading ? <LoadingState /> : <>
      <ComponentSummary lock={result.value!} root={release.id} />
      <LayoutPreview preview={{ files: Object.keys(result.value!.files), directories: result.value!.directories }} />
      <dl className="facts">
        <div><dt>Launch executable</dt><dd><code>{recipe.targets[platform].launch.path}</code></dd></div>
        <div><dt>Working directory</dt><dd>{recipe.targets[platform].launch.workingDirectory || 'Installation root'}</dd></div>
        {recipe.targets[platform].launch.arguments.length > 0 && <div><dt>Arguments</dt><dd><code>{JSON.stringify(recipe.targets[platform].launch.arguments)}</code></dd></div>}
      </dl>
    </>}
  </>
}

function DeclaredComponent({ dependency }: { dependency: Dependency }) {
  const app = useResource(`declared:${dependency.appId}`, signal => read<DepotApp>(`/apps/${dependency.appId}`, signal))
  const selector = dependency.selector
  return <li><span className="app-list-identity">{app.value && <ArtworkThumbnail app={app.value} />}{app.value?.name || (app.error ? 'Unavailable application' : 'Loading application…')}</span>
    <span>{selector.type === 'latest' ? 'Latest published release' : selector.type === 'channel' ? `Channel: ${selector.name}` : 'Pinned release'}</span></li>
}

function LayoutPreview({ preview }: { preview: Layout }) {
  return <details open><summary>Installation layout ({preview.files.length} files)</summary>
    <ul className="recipe-layout">{[...preview.directories.map(path => `${path}/`), ...preview.files].sort().map(path => <li key={path}><code>{path}</code></li>)}</ul>
  </details>
}

export function RecipeSection({ appId, release, editable, canPublish, onChange, onSaved }: {
  appId: string; release: Release; editable: boolean; canPublish: boolean; onChange: () => void; onSaved: (etag: string) => void
}) {
  const base = `/releases/${release.id}/recipe`
  const stored = useResource(base, signal => resource<Recipe | null>(base, signal))
  return <section className="panel recipe-panel"><h2>Installation recipe</h2>
    {stored.loading ? <LoadingState /> : stored.error ? <ProblemNotice error={stored.error} retry={stored.retry} /> :
      editable ? <RecipeEditor appId={appId} base={base} release={release} canPublish={canPublish}
        saved={stored.value!} onChange={onChange} onSaved={onSaved} reload={stored.retry} /> :
      stored.value?.value && <PublishedRecipe recipe={stored.value.value} base={base} release={release} />}
  </section>
}

function RecipeEditor({ appId, base, release, saved, canPublish, onChange, onSaved, reload }: {
  appId: string; base: string; release: Release; saved: { value: Recipe | null; etag: string }; canPublish: boolean;
  onChange: () => void; onSaved: (etag: string) => void; reload: () => void
}) {
  const { state } = useAuth()
  const csrf = state.session!.csrfToken
  const [recipe, setRecipe] = useState(saved.value || initialRecipe)
  const [etag, setEtag] = useState(saved.etag)
  const [platform, setPlatform] = useState(Object.keys(recipe.targets)[0])
  const [preview, setPreview] = useState<Preview>()
  const [lastLayout, setLastLayout] = useState('')
  const [validated, setValidated] = useState<Lock>()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const [notice, setNotice] = useState('')
  const publishAttempt = useRef<{ etag: string; key: string } | undefined>(undefined)
  const directoryList = useId()
  const target = recipe.targets[platform]
  const layoutKey = JSON.stringify([platform, recipe.dependencies, target.operations])
  const currentPreview = lastLayout === layoutKey ? preview : undefined

  function update(value: Recipe) {
    setRecipe(value); setValidated(undefined); setNotice(''); setError(undefined)
  }
  function editTarget(changed: Target) {
    update({ ...recipe, targets: { ...recipe.targets, [platform]: changed } })
  }
  function editOperation(index: number, op: Copy) {
    editTarget({ ...target, operations: target.operations.map((item, i) => i === index ? op : item) })
  }
  function move(index: number, delta: number) {
    const operations = [...target.operations]
    const [op] = operations.splice(index, 1)
    operations.splice(index + delta, 0, op)
    editTarget({ ...target, operations })
  }
  function add(app: DepotApp) {
    const alias = Object.keys(recipe.dependencies).find(key => recipe.dependencies[key].appId === app.id) || `app-${app.id}`
    update({ ...recipe, dependencies: { ...recipe.dependencies, [alias]: recipe.dependencies[alias] || { appId: app.id, selector: { type: 'latest' } } },
      targets: { ...recipe.targets, [platform]: { ...target, operations: [...target.operations, newOperation(alias)] } } })
  }
  function changeSelector(alias: string, selector: Selector) {
    const targets = Object.fromEntries(Object.entries(recipe.targets).map(([key, value]) => [key, { ...value,
      operations: value.operations.map(op => op.dependency === alias ? { ...op, associationId: undefined } : op) }]))
    update({ ...recipe, dependencies: { ...recipe.dependencies, [alias]: { ...recipe.dependencies[alias], selector } }, targets })
  }
  function removeDependency(alias: string) {
    const dependencies = { ...recipe.dependencies }
    delete dependencies[alias]
    const targets = Object.fromEntries(Object.entries(recipe.targets).map(([key, value]) => [key, { ...value, operations: value.operations.filter(op => op.dependency !== alias) }]))
    update({ ...recipe, dependencies, targets })
  }
  function changePlatform(next: string) {
    setPlatform(next); setValidated(undefined); setNotice(''); setError(undefined)
  }
  function changePlatformSet(next: string, add: boolean) {
    const targets = { ...recipe.targets }
    if (add) targets[next] = target
    else delete targets[next]
    if (add || next === platform) changePlatform(add ? next : Object.keys(targets)[0])
    update({ ...recipe, targets })
  }
  function payload(previewing: boolean): Recipe {
    const used = new Set(Object.values(recipe.targets).flatMap(value => value.operations.map(op => op.dependency)))
    const dependencies = Object.fromEntries(Object.entries(recipe.dependencies).filter(([alias]) => used.has(alias)))
    if (!previewing) return { ...recipe, dependencies }
    const targets = Object.fromEntries(Object.entries(recipe.targets).map(([key, value]) => [key, {
      ...value, launch: { path: value.launch.path || 'unselected', workingDirectory: '', arguments: [] },
    }]))
    return { ...recipe, dependencies, targets }
  }

  async function run(action: () => Promise<void>) {
    setBusy(true); setError(undefined); setNotice(''); setValidated(undefined)
    try { await action() } catch (failure) { setError(failure) }
    finally { setBusy(false) }
  }
  async function buildPreview() {
    await run(async () => {
      const result = await apiRequest<Preview>(`${base}/preview?platform=${enc(platform)}`, { method: 'POST', authenticated: true, csrf, ...jsonBody(payload(true)) })
      setPreview(result.value); setLastLayout(layoutKey)
    })
  }
  async function save() {
    await run(async () => {
      const result = await apiRequest<Recipe>(base, { method: 'PUT', authenticated: true, csrf, ifMatch: etag, ...jsonBody(payload(false)) })
      const nextEtag = result.response.headers.get('ETag')!
      setEtag(nextEtag); onSaved(nextEtag)
      setNotice('Draft saved. Checking dependencies, layout and launch for each target…')
      let active: Lock | undefined
      for (const target of Object.keys(recipe.targets)) {
        const lock = await read<Lock>(`${base}/lock?platform=${enc(target)}`)
        if (target === platform) active = lock
      }
      setValidated(active)
      setNotice('Draft saved. All targets validated.')
    })
  }
  async function publish() {
    const attempt = publishAttempt.current?.etag === etag ? publishAttempt.current : { etag, key: idempotencyKey() }
    publishAttempt.current = attempt
    setBusy(true); setError(undefined)
    try {
      await apiRequest(`/releases/${release.id}/publish`, { method: 'POST', authenticated: true, csrf, ifMatch: etag, idempotencyKey: attempt.key })
      onChange()
    } catch (failure) { setError(failure) }
    finally { setBusy(false) }
  }

  return <div className="form-stack">
    {error !== undefined && <ProblemNotice error={error} />}
    {notice && <p role="status">{notice}</p>}
    <fieldset disabled={busy} className="form-stack">
      <legend>Target platforms</legend>
      <div className="recipe-targets">{platforms.map(value => <label key={value}><input type="checkbox" checked={value in recipe.targets}
        disabled={value in recipe.targets && Object.keys(recipe.targets).length === 1}
        onChange={event => changePlatformSet(value, event.target.checked)} />{platformLabel(value)}</label>)}</div>
      <label>Editing target<select value={platform} onChange={event => changePlatform(event.target.value)}>
        {Object.keys(recipe.targets).map(value => <option key={value} value={value}>{platformLabel(value)}</option>)}
      </select></label>
    </fieldset>
    <fieldset disabled={busy} className="form-stack">
      <legend>Dependencies</legend>
      {Object.entries(recipe.dependencies).map(([alias, dependency]) => <DependencyPolicy key={alias} dependency={dependency}
        change={selector => changeSelector(alias, selector)} remove={() => removeDependency(alias)} />)}
      <FindDependency exclude={appId} add={add} />
    </fieldset>
    <fieldset disabled={busy} className="form-stack">
      <legend>Assembly operations</legend>
      {target.operations.map((operation, index) => <fieldset key={index} className="form-stack">
        <legend>Copy {index + 1}</legend>
        <OperationApp appId={recipe.dependencies[operation.dependency].appId} />
        <SourceControls dependency={recipe.dependencies[operation.dependency]} operation={operation} change={op => editOperation(index, op)} platform={platform} />
        <label>Destination path<input list={directoryList} value={operation.destination}
          placeholder="Installation root for a tree, file name for a file"
          onChange={event => editOperation(index, { ...operation, destination: event.target.value })} /></label>
        <label>On file collision<select value={operation.collision} onChange={event => editOperation(index, { ...operation, collision: event.target.value as Copy['collision'] })}>
          <option value="reject">Report conflict</option><option value="replaceFile">Replace the earlier file</option>
        </select></label>
        <label><input type="checkbox" checked={operation.executable} onChange={event => editOperation(index, { ...operation, executable: event.target.checked })} /> Mark selected file executable</label>
        <div className="form-actions">
          <button type="button" className="text-button" disabled={index === 0} onClick={() => move(index, -1)}>Move up</button>
          <button type="button" className="text-button" disabled={index === target.operations.length - 1} onClick={() => move(index, 1)}>Move down</button>
          <button type="button" className="text-button" onClick={() => editTarget({ ...target, operations: target.operations.filter((_, i) => i !== index) })}>Remove operation</button>
        </div>
      </fieldset>)}
      <label>Add copy from dependency<select value="" onChange={event => {
        if (event.target.value) editTarget({ ...target, operations: [...target.operations, newOperation(event.target.value)] })
      }}>
        <option value="">Choose a dependency</option>{Object.entries(recipe.dependencies).map(([alias, dependency]) =>
          <OperationApp key={alias} appId={dependency.appId} option={alias} />)}
      </select></label>
      <button type="button" className="button button-secondary" disabled={!target.operations.length} onClick={() => void buildPreview()}>Preview installation layout</button>
    </fieldset>
    {currentPreview && <LayoutPreview preview={currentPreview} />}
    <datalist id={directoryList}>{currentPreview?.directories.map(path => <option key={path} value={path} />)}</datalist>
    <fieldset disabled={busy} className="form-stack">
      <legend>Launch entry point</legend>
      <label>Launch executable<select value={target.launch.path} onChange={event => editTarget({ ...target, launch: { ...target.launch, path: event.target.value } })}>
        <option value="">{currentPreview ? 'Choose an executable file' : 'Preview the layout to choose a file'}</option>
        {target.launch.path && !currentPreview?.launchCandidates.includes(target.launch.path) && <option value={target.launch.path}>{target.launch.path}</option>}
        {currentPreview?.launchCandidates.map(path => <option key={path} value={path}>{path}</option>)}
      </select></label>
      {currentPreview?.launchCandidates.length === 0 && <p className="subtle">Mark the selected file executable to use it on this target.</p>}
      <label>Working directory<input list={directoryList} placeholder="Installation root" value={target.launch.workingDirectory}
        onChange={event => editTarget({ ...target, launch: { ...target.launch, workingDirectory: event.target.value } })} /></label>
      <label>Arguments (one literal argument per line)<textarea rows={3} value={target.launch.arguments.join('\n')}
        onChange={event => editTarget({ ...target, launch: { ...target.launch, arguments: event.target.value ? event.target.value.split('\n') : [] } })} /></label>
    </fieldset>
    {validated && <><h3>Validated components for {platformLabel(platform)}</h3><ComponentSummary lock={validated} root={release.id} /></>}
    <div className="form-actions">
      <button type="button" className="button button-secondary" disabled={busy || !Object.values(recipe.targets).every(value => value.launch.path)} onClick={() => void save()}>Save and validate recipe</button>
      {canPublish && <button type="button" className="button button-primary" disabled={busy || !validated} onClick={() => void publish()}>Publish release</button>}
      <button type="button" className="text-button" disabled={busy} onClick={reload}>Discard edits and reload</button>
    </div>
  </div>
}

function OperationApp({ appId, option }: { appId: string; option?: string }) {
  const app = useResource(`copy-app:${appId}`, signal => read<DepotApp>(`/apps/${appId}`, signal))
  const label = app.value?.name || (app.error ? 'Unavailable application' : 'Loading application…')
  return option ? <option value={option}>{label}</option> : <div className="app-list-identity">{app.value && <ArtworkThumbnail app={app.value} />}{label}</div>
}
