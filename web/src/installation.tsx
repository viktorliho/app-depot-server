import { useState, type ReactNode } from 'react'
import { ApiError, apiRequest, jsonBody, resource, type Association, type Declaration, type DeclarationSnapshot, type Page } from './api'
import { LoadingState, ProblemNotice } from './components'
import { useAuth } from './portal'
import { ArtifactPicker, ResolvedTreePicker } from './artifact-browser'
import { depot, manifest as gogManifest, record, strings, text } from './release-information'
import { useResource } from './use-resource'

const platforms = ['windows/x86_64', 'windows/aarch64', 'linux/x86_64', 'linux/aarch64', 'macos/x86_64', 'macos/aarch64']
type Copy = { associationId: string; source?: string; destination: string; collision?: 'reject' | 'replaceFile'; platforms?: string[] }
type Choice = { id: string; name: string; payload: Copy[] }
type Group = { id: string; name: string; language?: boolean; multiple?: boolean; required?: boolean; defaults?: string[]; choices: Choice[] }
type Prerequisite = { id: string; name: string; associationId: string; path?: string; platforms: string[]; arguments?: string[]; optional?: boolean; evidence: string; successExitCodes?: number[]; when?: { optionId: string; choiceId: string } | null }
type Launch = { associationId: string; revisionId: string; entryPointId: string; destination: string; workingDirectory?: string | null }
type Manifest = { version: 1; recipePayload?: boolean; payload?: Copy[]; options?: Group[]; prerequisites?: Prerequisite[]; unresolved?: string[]; launch?: Launch | null }
type Preview = { files?: Record<string, unknown>; directories?: string[]; launch?: { path: string }; prerequisites?: { id: string; name: string; optional: boolean }[]; recipe?: { files: Record<string, unknown>; launch: { path: string }; directories: string[] }; unresolved: string[] }
type Source = Association & { label: string }
const emptyCopy = (): Copy => ({ associationId: '', destination: '' })
const emptyManifest = (derived: boolean): Manifest => ({ version: 1, ...(derived ? { recipePayload: true } : { payload: [emptyCopy()] }) })

function replace<T>(items: T[], index: number, value: T) {
  return items.map((item, position) => position === index ? value : item)
}
function toggle<T>(items: T[], value: T, checked: boolean) {
  return checked ? [...items, value] : items.filter(item => item !== value)
}
function reorder<T>(items: T[], from: number, to: number) {
  return replace(replace(items, to, items[from]), from, items[to])
}

function RowActions({ index, length, move, remove }: { index: number; length: number; move: (to: number) => void; remove: () => void }) {
  return <div className="form-actions">
    <button type="button" className="button button-secondary" disabled={!index} onClick={() => move(index - 1)}>Move up</button>
    <button type="button" className="button button-secondary" disabled={index === length - 1} onClick={() => move(index + 1)}>Move down</button>
    <button type="button" className="button button-secondary" onClick={remove}>Remove</button>
  </div>
}
function Platforms({ values, update }: { values: string[]; update: (values: string[]) => void }) {
  return <fieldset><legend>Target platforms</legend>{platforms.map(platform => <label key={platform} className="checkbox-row">
    <input type="checkbox" checked={values.includes(platform)} onChange={event => update(toggle(values, platform, event.target.checked))} />{platform}
  </label>)}</fieldset>
}
function SourcePicker({ sources, associationId, path, filesOnly, change }: { sources: Source[]; associationId: string; path: string; filesOnly?: boolean; change: (associationId: string, path: string, file: boolean) => void }) {
  const chosen = sources.find(source => source.id === associationId)
  return <>
    <label>Artifact association<select value={associationId} onChange={event => change(event.target.value, '', sources.some(source => source.id === event.target.value && source.artifactId.startsWith('adfile:')))}>
      <option value="">Choose artifact</option>
      {associationId && !chosen && <option value={associationId}>Unavailable association</option>}
      {sources.map(source => <option key={source.id} value={source.id}>{source.label}</option>)}
    </select></label>
    {chosen && <p>Source selection · {path || (chosen.artifactId.startsWith('adfile:') ? 'Entire file' : filesOnly ? 'Choose a file' : 'Entire tree')}</p>}
    {chosen?.artifactId.startsWith('adtree:') && <ArtifactPicker key={chosen.artifactId} artifactId={chosen.artifactId} mode={filesOnly ? 'file' : 'any'} choose={(path, file) => change(associationId, path, file)} />}
  </>
}

function Copies({ copies, sources, update }: { copies: Copy[]; sources: Source[]; update: (value: Copy[]) => void }) {
  return <>{copies.map((item, index) => {
    const change = (partial: Partial<Copy>) => update(replace(copies, index, { ...item, ...partial }))
    return <fieldset key={index} className="form-stack"><legend>Copy operation {index + 1}</legend>
      <SourcePicker sources={sources} associationId={item.associationId} path={item.source ?? ''} change={(associationId, source, file) =>
        change({ associationId, source, destination: item.destination || (file ? source.split('/').at(-1) || 'payload' : '') })} />
      <label>Installation destination<input value={item.destination} onChange={event => change({ destination: event.target.value })} placeholder="Installation root for directories" /></label>
      <label>Collision policy<select value={item.collision || 'reject'} onChange={event => change({ collision: event.target.value as Copy['collision'] })}>
        <option value="reject">Reject duplicate files</option><option value="replaceFile">Replace earlier file at the same path</option>
      </select></label>
      <details><summary>Limit operation to selected platforms</summary><Platforms values={item.platforms || []} update={platforms => change({ platforms })} /></details>
      <RowActions index={index} length={copies.length} move={to => update(reorder(copies, index, to))} remove={() => update(copies.filter((_, position) => position !== index))} />
    </fieldset>
  })}
    <button type="button" className="button button-secondary" onClick={() => update([...copies, emptyCopy()])}>Add copy operation</button>
  </>
}

function Options({ groups, sources, update }: { groups: Group[]; sources: Source[]; update: (groups: Group[]) => void }) {
  const add = (language: boolean) => update([...groups, { id: language ? 'language' : `component-${groups.length + 1}`, name: language ? 'Game language' : 'Optional components', language, multiple: !language, required: language, choices: [] }])
  return <>{groups.map((group, index) => {
    const change = (patch: Partial<Group>) => update(replace(groups, index, { ...group, ...patch }))
    const defaults = group.defaults || []
    return <fieldset key={index} className="form-stack"><legend>{group.language ? 'Game language choices' : 'Component choices'} {index + 1}</legend>
      <label>Option identifier<input value={group.id} onChange={event => change({ id: event.target.value })} /></label>
      <label>Option name<input value={group.name} onChange={event => change({ name: event.target.value })} /></label>
      <label className="checkbox-row"><input type="checkbox" checked={!!group.required} onChange={event => change({ required: event.target.checked })} />Require a selection</label>
      {!group.language && <label className="checkbox-row"><input type="checkbox" checked={!!group.multiple} onChange={event => change({ multiple: event.target.checked, defaults: defaults.slice(0, 1) })} />Allow multiple choices</label>}
      {group.choices.map((choice, position) => <fieldset key={position} className="form-stack"><legend>Choice {position + 1}</legend>
        <label>{group.language ? 'Language identifier' : 'Choice identifier'}<input value={choice.id} placeholder={group.language ? 'en-US' : 'soundtrack'} onChange={event => {
          const id = event.target.value
          change({ choices: replace(group.choices, position, { ...choice, id }), defaults: defaults.map(value => value === choice.id ? id : value) })
        }} /></label>
        <label>Choice name<input value={choice.name} onChange={event => change({ choices: replace(group.choices, position, { ...choice, name: event.target.value }) })} /></label>
        <label className="checkbox-row"><input type="checkbox" checked={defaults.includes(choice.id)} onChange={event => change({ defaults: event.target.checked && !group.multiple ? [choice.id] : toggle(defaults, choice.id, event.target.checked) })} />Default choice</label>
        <Copies copies={choice.payload} sources={sources} update={payload => change({ choices: replace(group.choices, position, { ...choice, payload }) })} />
        <RowActions index={position} length={group.choices.length} move={to => change({ choices: reorder(group.choices, position, to) })} remove={() => change({
          choices: group.choices.filter((_, current) => current !== position), defaults: defaults.filter(id => id !== choice.id),
        })} />
      </fieldset>)}
      <button type="button" className="button button-secondary" onClick={() => change({ choices: [...group.choices, { id: '', name: '', payload: [] }] })}>Add choice</button>
      <RowActions index={index} length={groups.length} move={to => update(reorder(groups, index, to))} remove={() => update(groups.filter((_, position) => position !== index))} />
    </fieldset>
  })}
  <div className="form-actions">
    {!groups.some(item => item.language) && <button type="button" className="button button-secondary" onClick={() => add(true)}>Add language choices</button>}
    <button type="button" className="button button-secondary" onClick={() => add(false)}>Add component choices</button>
  </div></>
}

function Prerequisites({ items, groups, sources, update }: { items: Prerequisite[]; groups: Group[]; sources: Source[]; update: (items: Prerequisite[]) => void }) {
  const conditions = groups.flatMap(group => group.choices.map(choice => ({ group, choice })))
  return <>{items.map((item, index) => {
    const change = (patch: Partial<Prerequisite>) => update(replace(items, index, { ...item, ...patch }))
    const condition = item.when ? `${item.when.optionId}/${item.when.choiceId}` : ''
    return <fieldset key={index} className="form-stack"><legend>Installer {index + 1}</legend>
      <label>Prerequisite identifier<input value={item.id} onChange={event => change({ id: event.target.value })} /></label>
      <label>Prerequisite name<input value={item.name} onChange={event => change({ name: event.target.value })} /></label>
      <SourcePicker filesOnly sources={sources} associationId={item.associationId} path={item.path || ''} change={(associationId, path) => change({ associationId, path })} />
      <Platforms values={item.platforms} update={platforms => change({ platforms })} />
      <label>Evidence for the installer, order and arguments<input value={item.evidence} onChange={event => change({ evidence: event.target.value })} /></label>
      <label className="checkbox-row"><input type="checkbox" checked={!!item.optional} onChange={event => change({ optional: event.target.checked })} />Optional prerequisite</label>
      <label>Apply when<select value={condition} onChange={event => {
        const [optionId, choiceId] = event.target.value.split('/')
        change({ when: optionId ? { optionId, choiceId } : null })
      }}><option value="">Always on selected platforms</option>{conditions.map(({ group, choice }, index) =>
        <option key={index} value={`${group.id}/${choice.id}`}>{group.name} · {choice.name}</option>)}</select></label>
      <fieldset><legend>Literal installer arguments</legend>{(item.arguments || []).map((argument, index) => <div className="form-actions" key={index}>
        <label>Argument {index + 1}<input value={argument} onChange={event => change({ arguments: replace(item.arguments || [], index, event.target.value) })} /></label>
        <button type="button" className="button button-secondary" onClick={() => change({ arguments: item.arguments?.filter((_, position) => position !== index) })}>Remove argument</button>
      </div>)}
      <button type="button" className="button button-secondary" onClick={() => change({ arguments: [...(item.arguments || []), ''] })}>Add installer argument</button></fieldset>
      <details><summary>Successful exit codes</summary>
        {(item.successExitCodes || [0]).map((code, position) => <div className="form-actions" key={position}>
          <label>Successful exit code {position + 1}<input type="number" step="1" value={code} onChange={event => change({ successExitCodes: replace(item.successExitCodes || [0], position, Number(event.target.value)) })} /></label>
          <button type="button" className="button button-secondary" disabled={(item.successExitCodes || [0]).length <= 1}
            onClick={() => change({ successExitCodes: item.successExitCodes?.filter((_, current) => current !== position) })}>Remove exit code</button>
        </div>)}
        <button type="button" className="button button-secondary" onClick={() => change({ successExitCodes: [...(item.successExitCodes || [0]), 0] })}>Add exit code</button>
      </details>
      <RowActions index={index} length={items.length} move={to => update(reorder(items, index, to))} remove={() => update(items.filter((_, position) => position !== index))} />
    </fieldset>
  })}
  <button type="button" className="button button-secondary" onClick={() => update([...items, {
    id: `installer-${items.length + 1}`, name: '', associationId: '', platforms: [], evidence: '',
  }])}>Add prerequisite installer</button></>
}

function OptionsPreview({ groups, values, update }: { groups: Group[]; values: Record<string, string[]>; update: (groups: Record<string, string[]>) => void }) {
  return <>{groups.map(group => {
    const selected = values[group.id] || group.defaults || []
    const choice = (selected: string[]) => update({ ...values, [group.id]: selected })
    return group.multiple ? <fieldset key={group.id}><legend>{group.name} {group.required && '(required)'}</legend>
      {group.choices.map(item => <label key={item.id} className="checkbox-row"><input type="checkbox" checked={selected.includes(item.id)} onChange={event => choice(toggle(selected, item.id, event.target.checked))} />{item.name}</label>)}
    </fieldset> : <label key={group.id}>{group.name} {group.required && '(required)'}<select value={selected[0] || ''} onChange={event => choice(event.target.value ? [event.target.value] : [])}>
      <option value="">{group.required ? 'Choose a value' : 'None'}</option>{group.choices.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}
    </select></label>
  })}</>
}

type EditorProps = { releaseId: string; derived: boolean; snapshot: DeclarationSnapshot; onSaved: () => void }
function Editor({ releaseId, derived, snapshot, onSaved }: EditorProps) {
  const { state } = useAuth()
  const path = `/releases/${releaseId}/declarations`
  const current = useResource(path, signal => resource<Declaration>(path, signal))
  const available = useResource(`installation-sources:${releaseId}`, async signal => {
    const items: Association[] = []
    let cursor: string | null = null
    if (!derived) do {
      const page: Page<Association> = (await apiRequest<Page<Association>>(`/releases/${releaseId}/artifacts?pageSize=100${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`, { authenticated: true, signal })).value
      cursor = page.nextCursor
      items.push(...page.items)
    } while (cursor)
    return items
  })
  const [notice, setNotice] = useState('')
  if (current.error || available.error) return <ProblemNotice error={current.error || available.error} retry={() => { current.retry(); available.retry() }} />
  if (current.loading || available.loading) return <LoadingState />
  const doc = current.value!.value
  const sources = available.value!.map((item, index) => ({
    ...item, label: text(depot(snapshot.associations.find(entry => entry.associationId === item.id)?.declaration.data)?.label)
      || `${item.artifactId.startsWith('adfile:') ? 'File' : 'Tree'} artifact ${index + 1}`,
  }))
  return <>
    {notice && <p role="status">{notice}</p>}
    <ConfigurationForm key={doc.revisionId} {...{ releaseId, derived, snapshot, sources, doc, etag: current.value!.etag }}
      refresh={current.retry} saved={() => { setNotice('Installation configuration saved as a new declaration revision.'); current.retry(); onSaved() }} />
  </>
}

function ConfigurationForm({ releaseId, derived, snapshot, sources, doc, etag, refresh, saved }: Omit<EditorProps, 'onSaved'> & {
  sources: Source[]; doc: Declaration; etag?: string; refresh: () => void; saved: () => void
}) {
  const { state } = useAuth()
  const data = record(doc.data)
  const block = depot(doc.data)
  const raw = block?.installation
  const [form, setForm] = useState<Manifest | null>(() => record(raw)?.version === 1 ? raw as Manifest : null)
  const [platform, setPlatform] = useState(() => strings(block?.platforms).find(value => platforms.includes(value)) || platforms[0])
  const [selections, setSelections] = useState<Record<string, string[]>>({})
  const [preview, setPreview] = useState<Preview>()
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)
  const update = (patch: Partial<Manifest>) => { setForm(form && { ...form, ...patch }); setPreview(undefined) }
  const legacy = snapshot.associations.find(item => gogManifest(item.declaration.data) && sources.some(source => source.id === item.associationId))
  const legacySource = gogManifest(legacy?.declaration.data)
  const points = snapshot.associations.flatMap(item => {
    const array = depot(item.declaration.data)?.entryPoints
    if (!Array.isArray(array)) return []
    return array.flatMap(point => record(point)?.id && record(point.launch)?.method === 'NativePortableV1' ? [{
      associationId: item.associationId, revisionId: item.declaration.revisionId, id: text(point.id),
      label: text(point.label) || text(record(point.target)?.path) || text(point.id), path: text(record(point.target)?.path),
    }] : [])
  })
  async function action(kind: 'preview' | 'save') {
    setBusy(true); setError(undefined)
    try {
      const csrf = state.session!.csrfToken
      const authenticated = true
      if (kind === 'preview') {
        const reply = await apiRequest<Preview>(`/releases/${releaseId}/installation/resolve`, {
          method: 'POST', authenticated, csrf, ...jsonBody({ platform, selections, manifest: form }),
        })
        setPreview(reply.value)
      } else {
        const appDepot = { ...block }
        if (form) appDepot.installation = form
        else delete appDepot.installation
        await apiRequest(`/releases/${releaseId}/declarations`, {
          method: 'PUT', authenticated, csrf, ifMatch: etag, ...jsonBody({ data: { ...data, appDepot } }),
        })
        saved()
      }
    } catch (failure) { setError(failure) } finally { setBusy(false) }
  }
  if (!data || (data.appDepot !== undefined && !block) || (raw !== undefined && !form && record(raw)?.version !== 1)) {
    return <p>Open the raw Release declaration in Advanced release details to inspect this document.</p>
  }
  if (!form) return <>
    <p>Installation configuration is optional for downloadable releases.</p>
    <button type="button" className="button button-secondary" onClick={() => setForm(emptyManifest(derived))}>Configure installation</button>
    {legacy && <button type="button" className="button button-secondary" onClick={() => setForm({
      version: 1, payload: [{ associationId: legacy.associationId, destination: '', platforms: ['windows/x86_64', 'windows/aarch64'] }],
      unresolved: ['Review the recovered payload layout and prerequisites before installing.', ...strings(record(legacySource?.analysis)?.unknownActions),
        ...strings(record(legacySource?.detected)?.redistributableRoots).slice(0, 64).map(path => `Review detected redistributable: ${Array.from(path).slice(0, 200).join('')}`)],
    })}>Use recovered GOG payload</button>}
    {raw !== undefined && <button type="button" className="button button-secondary" disabled={busy} onClick={() => void action('save')}>Save removal</button>}
    {error !== undefined && <ProblemNotice error={error} />}
  </>
  const selected = form.launch ? `${form.launch.associationId}/${form.launch.entryPointId}/${form.launch.revisionId}` : ''
  const groups = form.options || []
  const layout = preview?.recipe || preview
  const output = Object.keys(layout?.files || {})
  return <div className="form-stack">
    <fieldset disabled={busy} className="form-stack">
      {derived ? <p>Installation uses the existing RecipeV1 lock and its declared launch target for the chosen platform.</p> : <>
        <details open><summary>Base payload</summary><Copies copies={form.payload || []} sources={sources} update={payload => update({ payload })} /></details>
        <details><summary>Language and component choices</summary><Options groups={groups} sources={sources} update={options => { update({ options }); setSelections({}) }} /></details>
        <details><summary>Prerequisite installers</summary><p>Executable prerequisites run only in a supporting client with user authorization. Order follows this list.</p>
          <Prerequisites items={form.prerequisites || []} groups={groups} sources={sources} update={prerequisites => update({ prerequisites })} />
        </details>
        <details><summary>Launch integration</summary>
          <label>Association entry point<select value={selected} onChange={event => {
            const point = points.find(item => `${item.associationId}/${item.id}/${item.revisionId}` === event.target.value)
            update({ launch: point ? { associationId: point.associationId, revisionId: point.revisionId, entryPointId: point.id, destination: point.path || 'payload' } : null })
          }}><option value="">Leave launch unselected</option>
            {selected && !points.some(item => `${item.associationId}/${item.id}/${item.revisionId}` === selected) && <option value={selected}>Previously pinned entry point revision</option>}
            {points.map(point => <option key={`${point.associationId}/${point.id}`} value={`${point.associationId}/${point.id}/${point.revisionId}`}>{point.label}</option>)}
          </select></label>
          {form.launch && <>
            <label>Installed executable path<input value={form.launch.destination} onChange={event => update({ launch: { ...form.launch!, destination: event.target.value } })} /></label>
            <label className="checkbox-row"><input type="checkbox" checked={form.launch.workingDirectory != null}
              onChange={event => update({ launch: { ...form.launch!, workingDirectory: event.target.checked ? '' : null } })} />Override installed working directory</label>
            {form.launch.workingDirectory != null && <label>Installed working directory<input value={form.launch.workingDirectory} placeholder="Installation root"
              onChange={event => update({ launch: { ...form.launch!, workingDirectory: event.target.value } })} /></label>}
            {form.launch.workingDirectory != null && layout?.directories && <details><summary>Browse installed directories from preview</summary>
              <ResolvedTreePicker identity={`installation:${platform}`} mode="directory" entries={layout.directories.map(path => ({ type: 'Directory', path }))}
                choose={path => update({ launch: { ...form.launch!, workingDirectory: path } })} />
            </details>}
          </>}
        </details>
      </>}
      <details><summary>Unresolved installation actions</summary>
        <label>Unresolved notes (one per line)<textarea rows={4} value={form.unresolved?.join('\n') || ''} onChange={event => update({ unresolved: event.target.value === '' ? [] : event.target.value.split('\n') })} /></label>
      </details>
    </fieldset>
    <details><summary>Installation preview</summary>
      <label>Preview platform<select value={platform} onChange={event => { setPlatform(event.target.value); setPreview(undefined) }}>{platforms.map(value => <option key={value}>{value}</option>)}</select></label>
      <OptionsPreview groups={groups} values={selections} update={values => { setSelections(values); setPreview(undefined) }} />
      <button type="button" className="button button-secondary" disabled={busy} onClick={() => void action('preview')}>Validate and preview</button>
      {layout && <div aria-live="polite"><p>{output.length} files · {layout.directories?.length || 0} directories</p>
        <ul>{output.slice(0, 50).map(path => <li key={path}><code>{path}</code></li>)}</ul>
        {output.length > 50 && <p>First 50 paths shown.</p>}
        {layout.launch && <p>Launch · {layout.launch.path}</p>}
        {preview?.prerequisites?.length ? <ol>{preview.prerequisites.map(item => <li key={item.id}>{item.name} ({item.optional ? 'optional' : 'required'})</li>)}</ol> : null}
        {preview?.unresolved.length ? <ul>{preview.unresolved.map((item, index) => <li key={index}>{item}</li>)}</ul> : null}
      </div>}
    </details>
    <div className="form-actions">
      <button type="button" className="button button-primary" disabled={busy} onClick={() => {
        if (form.unresolved?.some(item => !item.trim())) {
          setError(new Error('Remove blank lines from unresolved notes before saving.'))
        } else void action('save')
      }}>Save installation configuration</button>
      <button type="button" className="button button-secondary" disabled={busy} onClick={() => { setForm(null); setPreview(undefined) }}>Clear configuration</button>
    </div>
    {error !== undefined && <ProblemNotice error={error} retry={error instanceof ApiError && error.status === 412 ? refresh : undefined} />}
  </div>
}

export function InstallationConfiguration({ editable, ...props }: EditorProps & { editable: boolean }) {
  const [opened, setOpened] = useState(false)
  const configuration = record(depot(props.snapshot.release.data)?.installation)
  if (!editable && !configuration) return null
  let summary: ReactNode = 'Optional configuration'
  if (configuration) summary = configuration.version === 1 ? 'Installation Manifest V1' : 'Unsupported installation manifest'
  return <section className="panel"><h2>Installation Configuration</h2>
    <p>{summary}</p>
    {editable ? <details onToggle={event => { if (event.currentTarget.open) setOpened(true) }}>
      <summary>Configure payload, choices and prerequisites</summary>
      {opened && <Editor {...props} />}
    </details> : <p>Publishers define the installation layout and optional components.</p>}
  </section>
}
