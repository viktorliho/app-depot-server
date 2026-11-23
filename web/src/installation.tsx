import { InstallationStages } from './installation-stages'
import { DiscoveryReview, type DiscoveryReport } from './installation-discovery'
import { useEffect, useMemo, useRef, useState } from 'react'
import { isRelativeInstallPath, platformName, planTotals, prerequisiteIssue, publisherEvidence } from './installation-review'
import { ApiError, apiRequest, idempotencyKey, jsonBody, resource, type Artifact, type Association, type Declaration, type DeclarationSnapshot, type Page } from './api'
import { LoadingState, ProblemNotice } from './components'
import { useAuth } from './portal'
import { ArtifactPicker, ResolvedTreePicker } from './artifact-browser'
import { depot, fileSize, manifest as gogManifest, record, strings, text } from './release-information'
import { useResource } from './use-resource'

const platforms = ['windows/x86_64', 'windows/aarch64', 'linux/x86_64', 'linux/aarch64', 'macos/x86_64', 'macos/aarch64']
type Copy = { associationId: string; source?: string; destination: string; collision?: 'reject' | 'replaceFile'; platforms?: string[] }
type Choice = { id: string; name: string; payload: Copy[] }
type Group = { id: string; name: string; language?: boolean; multiple?: boolean; required?: boolean; defaults?: string[]; choices: Choice[] }
type Prerequisite = { id: string; name: string; associationId: string; path?: string; platforms: string[]; arguments?: string[]; optional?: boolean; evidence: string; successExitCodes?: number[]; when?: { optionId: string; choiceId: string } | null }
type Launch = { associationId: string; revisionId: string; entryPointId: string; destination: string; workingDirectory?: string | null }
type Localization = { mode: 'unknown' | 'selectable' } | { mode: 'fixed'; language: string } | { mode: 'bundled'; languages: string[] }
type Manifest = { localization?: Localization; version: 1; recipePayload?: boolean; payload?: Copy[]; options?: Group[]; prerequisites?: Prerequisite[]; unresolved?: string[]; launch?: Launch | null }
type Preview = { selections?: Record<string, string[]>; files?: Record<string, { size?: number }>; directories?: string[]; launch?: { path: string }; prerequisites?: { id: string; name: string; optional: boolean }[]; recipe?: { files: Record<string, { size?: number }>; launch: { path: string }; directories: string[] }; unresolved: string[] }
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
    <input type="checkbox" data-installation-field="platforms" checked={values.includes(platform)} onChange={event => update(toggle(values, platform, event.target.checked))} />{platformName(platform)}
  </label>)}</fieldset>
}
function SourcePicker({ sources, associationId, path, filesOnly, change }: { sources: Source[]; associationId: string; path: string; filesOnly?: boolean; change: (associationId: string, path: string, file: boolean) => void }) {
  const chosen = sources.find(source => source.id === associationId)
  const metadata = useResource(`installation-source:${chosen?.artifactId || ''}`, async signal => chosen
    ? (await apiRequest<Artifact>(`/artifacts/${encodeURIComponent(chosen.artifactId)}`, { authenticated: true, signal })).value : undefined)
  return <>
    <label>Artifact association<select data-installation-field="source" value={associationId} onChange={event => change(event.target.value, '', sources.some(source => source.id === event.target.value && source.artifactId.startsWith('adfile:')))}>
      <option value="">Choose artifact</option>
      {associationId && !chosen && <option value={associationId}>Unavailable association</option>}
      {sources.map(source => <option key={source.id} value={source.id}>{source.label}</option>)}
    </select></label>
    {chosen && <p>Source selection · {path || (chosen.artifactId.startsWith('adfile:') ? 'Entire file' : filesOnly ? 'Choose a file' : 'Entire tree')}</p>}
    {metadata.value && <p className="subtle">{metadata.value.kind} Artifact · {fileSize(metadata.value.size)} {path ? 'for the complete Artifact' : ''}</p>}
    {chosen?.artifactId.startsWith('adtree:') && <ArtifactPicker key={chosen.artifactId} artifactId={chosen.artifactId} mode={filesOnly ? 'file' : 'any'} choose={(path, file) => change(associationId, path, file)} />}
  </>
}

function Copies({ copies, sources, update }: { copies: Copy[]; sources: Source[]; update: (value: Copy[]) => void }) {
  return <>{copies.map((item, index) => {
    const change = (partial: Partial<Copy>) => update(replace(copies, index, { ...item, ...partial }))
    return <fieldset key={index} className="form-stack"><legend>Copy operation {index + 1}</legend>
      <SourcePicker sources={sources} associationId={item.associationId} path={item.source ?? ''} change={(associationId, source, file) =>
        change({ associationId, source, destination: item.destination || (file ? source.split('/').at(-1) || 'payload' : '') })} />
      <label>Installation destination<input aria-invalid={!isRelativeInstallPath(item.destination)} value={item.destination} onChange={event => change({ destination: event.target.value })} placeholder="Installation root for directories" /></label>
      {!isRelativeInstallPath(item.destination) && <p className="field-error">Enter a path inside the installation folder. Use forward slashes between folders.</p>}
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

function Options({ groups, sources, update, allowLanguage, languageOnly }: { languageOnly?: boolean; allowLanguage: boolean; groups: Group[]; sources: Source[]; update: (groups: Group[]) => void }) {
  const add = (language: boolean) => update([...groups, { id: language ? `language-${idempotencyKey()}` : `component-${idempotencyKey()}`, name: language ? 'Game language' : 'Optional components', language, multiple: !language, required: language, choices: [] }])
  return <>{groups.map((group, index) => {
    const change = (patch: Partial<Group>) => update(replace(groups, index, { ...group, ...patch }))
    const defaults = group.defaults || []
    return <fieldset key={index} className="form-stack"><legend>{group.language ? 'Game language choices' : 'Component choices'} {index + 1}</legend>
      <details><summary>Advanced option details</summary><label>Option identifier<input value={group.id} onChange={event => change({ id: event.target.value })} /></label></details>
      <label>Option name<input value={group.name} onChange={event => change({ name: event.target.value })} /></label>
      <label className="checkbox-row"><input type="checkbox" checked={!!group.required} onChange={event => change({ required: event.target.checked })} />Require a selection</label>
      {!group.language && <label className="checkbox-row"><input type="checkbox" checked={!!group.multiple} onChange={event => change({ multiple: event.target.checked, defaults: defaults.slice(0, 1) })} />Allow multiple choices</label>}
      {group.choices.map((choice, position) => <fieldset key={position} className="form-stack"><legend>Choice {position + 1}</legend>
        <div hidden={!group.language}><label>Language identifier<input value={choice.id} placeholder="en-US" onChange={event => {
          const id = event.target.value
          change({ choices: replace(group.choices, position, { ...choice, id }), defaults: defaults.map(value => value === choice.id ? id : value) })
        }} /></label></div>
        <label>Choice name<input value={choice.name} onChange={event => change({ choices: replace(group.choices, position, { ...choice, name: event.target.value }) })} /></label>
        <label className="checkbox-row"><input type="checkbox" checked={defaults.includes(choice.id)} onChange={event => change({ defaults: event.target.checked && !group.multiple ? [choice.id] : toggle(defaults, choice.id, event.target.checked) })} />Default choice</label>
        <Copies copies={choice.payload} sources={sources} update={payload => change({ choices: replace(group.choices, position, { ...choice, payload }) })} />
        <RowActions index={position} length={group.choices.length} move={to => change({ choices: reorder(group.choices, position, to) })} remove={() => change({
          choices: group.choices.filter((_, current) => current !== position), defaults: defaults.filter(id => id !== choice.id),
        })} />
      </fieldset>)}
      <button type="button" className="button button-secondary" onClick={() => change({ choices: [...group.choices, { id: group.language ? '' : idempotencyKey(), name: '', payload: [] }] })}>Add choice</button>
      <RowActions index={index} length={groups.length} move={to => update(reorder(groups, index, to))} remove={() => update(groups.filter((_, position) => position !== index))} />
    </fieldset>
  })}
  <div className="form-actions">
    {allowLanguage && !groups.some(item => item.language) && <button type="button" className="button button-secondary" onClick={() => add(true)}>Add language choices</button>}
    {!languageOnly && <button type="button" className="button button-secondary" onClick={() => add(false)}>Add component choices</button>}
  </div></>
}

function Prerequisites({ items, groups, sources, update, initialPlatforms, attempted, editId, select }: {
  items: Prerequisite[]; groups: Group[]; sources: Source[]; update: (items: Prerequisite[]) => void;
  initialPlatforms: string[]; attempted: boolean; editId: string; select: (id: string) => void
}) {
  const conditions = groups.flatMap(group => group.choices.map(choice => ({ group, choice })))
  return <>{items.map((item, index) => {
    const change = (patch: Partial<Prerequisite>) => update(replace(items, index, { ...item, ...patch }))
    const condition = item.when ? `${item.when.optionId}/${item.when.choiceId}` : ''
    const source = sources.find(source => item.associationId === source.id)
    const issue = prerequisiteIssue(item, !!source?.artifactId.startsWith('adtree:'))
    return <section className="installer-card" key={item.id} data-installer={item.id}>
      <div className="installer-heading"><div><h4>{index + 1}. {item.name || 'New installer'}</h4>
        <p>{item.path || source?.label || 'Choose an installer'} · {item.optional ? 'Optional' : 'Required'} · {item.platforms.map(platformName).join(', ') || 'Choose a platform'}</p>
        <p className="subtle">{item.id.startsWith('discovered-') ? 'Detected package' : 'Publisher configured'} · {issue ? 'Needs attention' : 'Ready to validate'}</p></div>
        <button type="button" className="button button-secondary" aria-expanded={editId === item.id} onClick={() => select(editId === item.id ? '' : item.id)}>
          {editId === item.id ? 'Close installer editor' : 'Edit installer'}</button>
      </div>
      {attempted && issue && <p className="field-error" role="alert" tabIndex={-1}>{issue.message}</p>}
      <RowActions index={index} length={items.length} move={to => update(reorder(items, index, to))} remove={() => {
        if (window.confirm('Remove this installer from the plan? Its file will still be available in the Artifact.')) update(items.filter((_, position) => position !== index))
      }} />
      {editId === item.id && <fieldset className="form-stack"><legend>Installer details</legend>

      <label>Prerequisite name<input data-installation-field="name" aria-invalid={attempted && issue?.field === 'name'} value={item.name} onChange={event => change({ name: event.target.value })} /></label>
      <SourcePicker filesOnly sources={sources} associationId={item.associationId} path={item.path || ''} change={(associationId, path) => change({ associationId, path })} />
      <details open={!item.platforms.length}><summary>Target platforms</summary><Platforms values={item.platforms} update={platforms => change({ platforms })} /></details>
      <label className="checkbox-row"><input type="checkbox" checked={!!item.optional} onChange={event => change({ optional: event.target.checked })} />Optional prerequisite</label>
      <details open={attempted && issue?.field === 'evidence'}><summary>Advanced installer settings</summary>
      <label>Evidence for the installer, order and arguments<input data-installation-field="evidence" aria-invalid={attempted && issue?.field === 'evidence'} value={item.evidence} onChange={event => change({ evidence: event.target.value })} /></label>
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
      </details>
      </fieldset>}
    </section>
  })}
  <button type="button" className="button button-secondary" onClick={() => {
    const id = idempotencyKey()
    update([...items, { id, name: '', associationId: '', platforms: initialPlatforms, evidence: publisherEvidence }])
    select(id)
  }}>Add prerequisite installer</button></>
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

type Detected = { report: DiscoveryReport; prerequisites: Prerequisite[]; unresolved: string[] }

function DetectComponents({ sources, apply }: { sources: Source[]; apply: (id: string, items: Prerequisite[], notes: string[]) => void }) {
  const { state } = useAuth()
  const trees = sources.filter(source => source.artifactId.startsWith('adtree:'))
  const [id, setId] = useState(trees[0]?.id || '')
  const [detected, setDetected] = useState<Detected>()
  const [disabled, setDisabled] = useState<string[]>([])
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)
  if (!trees.length) return null
  async function detect() {
    setDetected(undefined); setDisabled([]); setError(undefined); setBusy(true)
    try {
      const result = await apiRequest<Detected>(`/associations/${encodeURIComponent(id)}/installation/discover`, {
        method: 'POST', authenticated: true, csrf: state.session!.csrfToken, signal: AbortSignal.timeout(30000),
      })
      setDetected(result.value)
    } catch (error) {
      setError(error)
    } finally {
      setBusy(false)
    }
  }
  return <details><summary>Detect installation components</summary>
    <label>Tree Artifact<select disabled={busy} value={id} onChange={event => {
      setId(event.target.value); setDetected(undefined); setError(undefined)
    }}>{trees.map(tree => <option value={tree.id} key={tree.id}>{tree.label}</option>)}</select></label>
    <button type="button" className="button button-secondary" disabled={busy} onClick={() => void detect()}>{busy ? 'Detecting…' : 'Detect components'}</button>
    {error !== undefined && <ProblemNotice error={error} />}
    {detected && <>
      <DiscoveryReview report={detected.report} disabled={disabled} change={setDisabled} />
      <button type="button" className="button button-secondary" onClick={() => {
        apply(id, detected.prerequisites.filter(item => !disabled.includes(item.path || '')), detected.unresolved)
        setDetected(undefined)
      }}>Apply to installation editor</button>
    </>}
  </details>
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

const stages = ['Payload', 'Localization', 'Optional components', 'Prerequisites', 'Launch', 'Review installation plan'] as const
type Stage = typeof stages[number]

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
  const [pending, setPending] = useState<'preview' | 'save'>()
  const busy = !!pending
  const [stage, setStage] = useState<Stage>('Payload')
  const [editId, setEditId] = useState('')
  const [attempted, setAttempted] = useState(false)
  const [problem, setProblem] = useState('')
  const [browse, setBrowse] = useState(false)
  const message = useRef<HTMLParagraphElement>(null)
  const editor = useRef<HTMLDivElement>(null)
  const dirty = JSON.stringify(form) !== JSON.stringify(record(raw)?.version === 1 ? raw : null)
  useEffect(() => {
    if (!dirty) return
    const warn = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = '' }
    window.addEventListener('beforeunload', warn)
    return () => window.removeEventListener('beforeunload', warn)
  }, [dirty])
  const update = (patch: Partial<Manifest>) => { setForm(form && { ...form, ...patch }); setPreview(undefined); setBrowse(false); setProblem('') }
  const discovery = !derived && <DetectComponents key="detect-components" sources={sources} apply={(id, items, notes) => {
    setForm(current => {
      const existing: Manifest = current || { version: 1, payload: [{ associationId: id, destination: '' }], localization: { mode: 'unknown' } }
      const kept = (existing.prerequisites || []).filter(item => !item.id.startsWith(`discovered-${id}-`))
      return { ...existing, prerequisites: [...kept, ...items.filter(item => !kept.some(previous =>
        previous.associationId === id && previous.path === item.path))],
        unresolved: [...new Set([...(existing.unresolved || []), ...notes])] }
    })
    setPreview(undefined)
  }} />
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
  function showIssue(section: Stage, detail: string) {
    setStage(section); setProblem(detail)
    requestAnimationFrame(() => message.current?.focus())
  }
  async function action(kind: 'preview' | 'save') {
    const badInstaller = form?.prerequisites?.find(item => prerequisiteIssue(item,
      !!sources.find(source => source.id === item.associationId)?.artifactId.startsWith('adtree:')))
    if (badInstaller) {
      setEditId(badInstaller.id); setAttempted(true)
      showIssue('Prerequisites', `Complete the installer details for ${badInstaller.name || 'the new installer'}.`)
      const issue = prerequisiteIssue(badInstaller, !!sources.find(source => source.id === badInstaller.associationId)?.artifactId.startsWith('adtree:'))
      requestAnimationFrame(() => {
        const invalid = editor.current?.querySelector<HTMLElement>(`[data-installer="${CSS.escape(badInstaller.id)}"] [data-installation-field="${issue?.field}"]`)
        invalid?.focus()
      })
      return
    }
    if (form?.payload?.some(item => !item.associationId)) {
      showIssue('Payload', 'Choose a source Artifact for each payload copy.')
      return
    }
    if (form?.payload?.some(copy => !isRelativeInstallPath(copy.destination))) {
      showIssue('Payload', 'Use a path inside the installation folder for each copy destination.')
      return
    }
    const tags = form?.localization?.mode === 'fixed' ? [form.localization.language] : form?.localization?.mode === 'bundled' ? form.localization.languages : []
    if ((form?.localization?.mode === 'bundled' && tags.length < 2) ||
        tags.some(tag => !/^[A-Za-z][A-Za-z0-9]{0,7}(-[A-Za-z0-9]{1,8})*$/.test(tag)) ||
        new Set(tags.map(tag => tag.toLowerCase())).size !== tags.length) {
      showIssue('Localization', 'Enter distinct language tags such as ru-RU. Bundled localization needs at least two languages.')
      return
    }
    if (form?.localization?.mode === 'selectable' && !form.options?.some(group => group.language && group.choices.length >= 2)) {
      showIssue('Localization', 'Add at least two languages and map their installation files.')
      return
    }
    const incompleteGroup = form?.options?.find(group => !group.name.trim() || !group.choices.length ||
      group.choices.some(choice => !choice.name.trim() || !choice.id.trim() || choice.payload.some(copy => !copy.associationId || !isRelativeInstallPath(copy.destination))))
    if (incompleteGroup) {
      showIssue(incompleteGroup.language ? 'Localization' : 'Optional components', 'Name the choices and verify each file mapping’s Artifact and destination.')
      return
    }
    setPending(kind); setProblem(''); setError(undefined)
    try {
      const csrf = state.session!.csrfToken
      const authenticated = true
      const signal = AbortSignal.timeout(30000)
      if (kind === 'preview') {
        const reply = await apiRequest<Preview>(`/releases/${releaseId}/installation/resolve`, {
          method: 'POST', authenticated, csrf, signal, ...jsonBody({ platform, selections, manifest: form }),
        })
        setPreview(reply.value)
      } else {
        const appDepot = { ...block }
        if (form) appDepot.installation = form
        else delete appDepot.installation
        await apiRequest(`/releases/${releaseId}/declarations`, {
          method: 'PUT', authenticated, csrf, signal, ifMatch: etag, ...jsonBody({ data: { ...data, appDepot } }),
        })
        saved()
      }
    } catch (failure) {
      setError(failure)
      if (failure instanceof ApiError && failure.code === 'installation_collision') showIssue('Payload', 'Two copy operations write the same path. Review their destinations or allow the later copy to replace that file.')
      else if (failure instanceof ApiError && failure.code === 'installation_launch') showIssue('Launch', 'Choose an entry point whose executable is included at the installed path.')
      else if (failure instanceof ApiError && failure.code === 'installation_selection') showIssue('Review installation plan', 'Choose values for the required installation options.')
    } finally { setPending(undefined) }
  }
  const layout = preview?.recipe || preview
  const plan = useMemo(() => layout ? planTotals(layout.files || {}, layout.directories || []) : undefined, [layout])
  const entries = useMemo(() => browse && layout ? [
    ...Object.entries(layout.files || {}).map(([path, entry]) => ({ type: 'File' as const, path, size: entry.size })),
    ...(layout.directories || []).map(path => ({ type: 'Directory' as const, path })),
  ] : [], [layout, browse])
  if (!data || (data.appDepot !== undefined && !block) || (raw !== undefined && !form && record(raw)?.version !== 1)) {
    return <p>Open the raw Release declaration in Advanced release details to inspect this document.</p>
  }
  if (!form) return <>
    <p>Installation configuration is optional for downloadable releases.</p>
    {discovery}
    <button type="button" className="button button-secondary" onClick={() => setForm(emptyManifest(derived))}>Configure installation</button>
    {legacy && <button type="button" className="button button-secondary" onClick={() => setForm({
      version: 1, payload: [{ associationId: legacy.associationId, destination: '', platforms: ['windows/x86_64', 'windows/aarch64'] }],
      unresolved: ['Review the recovered payload layout and prerequisites before installing.', ...strings(record(legacySource?.analysis)?.unknownActions),
        ...strings(record(legacySource?.detected)?.redistributableRoots).slice(0, 64).map(path => `Review detected redistributable: ${Array.from(path).slice(0, 200).join('')}`)],
    })}>Use recovered GOG payload</button>}
    {raw !== undefined && <div className="form-actions">
      <button type="button" className="button button-secondary" disabled={busy} onClick={() => void action('save')}>Save removal</button>
      <button type="button" className="button button-secondary" disabled={busy} onClick={() => setForm(raw as Manifest)}>Cancel removal</button>
    </div>}
    {error !== undefined && <ProblemNotice error={error} />}
  </>
  const selected = form.launch ? `${form.launch.associationId}/${form.launch.entryPointId}/${form.launch.revisionId}` : ''
  const groups = form.options || []
  function editGroups(language: boolean, edited: Group[]) {
    const remaining = [...edited]
    const options = groups.flatMap(group => Boolean(group.language) === language ? remaining.splice(0, 1) : [group])
    update({ options: [...options, ...remaining] })
    setSelections({})
  }
  const releasePlatforms = strings(block?.platforms).filter(value => platforms.includes(value))
  const availableStages = derived ? stages.filter(item => item === 'Review installation plan') : stages
  const activeStage = derived ? 'Review installation plan' : stage
  const selectedChoices = groups.flatMap(group => group.choices
    .filter(choice => (preview?.selections?.[group.id] || selections[group.id] || group.defaults || []).includes(choice.id))
    .map(choice => ({ ...choice, language: group.language })))
  const copyCount = [...(form.payload || []), ...selectedChoices.flatMap(choice => choice.payload)]
    .filter(copy => !copy.platforms?.length || copy.platforms.includes(platform)).length
  const counts = [form.recipePayload ? 'Uses the release recipe' : `${form.payload?.length || 0} copy operations`,
    form.localization?.mode === 'fixed' ? form.localization.language || 'Choose a language' :
      form.localization?.mode === 'bundled' ? form.localization.languages.join(', ') : form.localization?.mode === 'selectable' ? 'Choose files at install time' : 'Unconfirmed',
    `${groups.filter(group => !group.language).length} component groups`, `${form.prerequisites?.length || 0} installers`,
    form.launch?.destination || 'No launch selected', plan ? (preview?.unresolved.length ? 'Valid with warnings' : 'Valid preview') : 'Resolve a preview']
  return <div className="form-stack" ref={editor}>
    <InstallationStages stages={availableStages.map(title => ({ title, summary: counts[stages.indexOf(title)] }))}
      active={activeStage} onSelect={setStage} />
    {problem && <p ref={message} className="field-error" role="alert" tabIndex={-1}>{problem}</p>}
    <fieldset disabled={busy} className="form-stack">
      {derived ? <p>Installation uses the existing RecipeV1 lock and its declared launch target for the chosen platform.</p> : <>
        {activeStage === 'Payload' && <section className="form-stack"><h3>Base payload</h3><Copies copies={form.payload || []} sources={sources} update={payload => update({ payload })} /></section>}
        {activeStage === 'Localization' && <section className="form-stack"><h3>Application languages</h3>
          <label>Localization packaging<select value={form.localization?.mode || (groups.some(group => group.language) ? 'legacy' : 'unknown')}
            onChange={event => {
              const mode = event.target.value
              if (mode === 'legacy') update({ localization: undefined })
              else if (mode === 'selectable') update({ localization: { mode } })
              else {
                const options = groups.filter(group => !group.language)
                if (options.length < groups.length && !window.confirm('Remove the current installation-time language choices and their file mappings?')) return
                if (mode === 'fixed') update({ localization: { mode, language: '' }, options })
                else if (mode === 'bundled') update({ localization: { mode, languages: ['', ''] }, options })
                else update({ localization: { mode: 'unknown' }, options })
              }
              setSelections({})
            }}>
            {!form.localization && groups.some(group => group.language) && <option value="legacy">Existing language choices</option>}
            <option value="unknown">Unknown</option><option value="fixed">Fixed language</option>
            <option value="bundled">Bundled languages (selected in the application)</option>
            <option value="selectable">Selectable languages (different installation files)</option>
          </select></label>
          {form.localization?.mode === 'fixed' && <label>Language identifier<input value={form.localization.language} placeholder="ru-RU"
            onChange={event => update({ localization: { mode: 'fixed', language: event.target.value } })} /></label>}
          {form.localization?.mode === 'bundled' && <label>Language identifiers (comma-separated)<input value={form.localization.languages.join(',')} placeholder="en-US,ru-RU"
            onChange={event => update({ localization: { mode: 'bundled', languages: event.target.value.split(',').map(tag => tag.trim()) } })} /></label>}
          <Options groups={groups.filter(group => group.language)} sources={sources} allowLanguage={form.localization?.mode === 'selectable'}
            update={options => editGroups(true, options)} languageOnly />
        </section>}
        {activeStage === 'Optional components' && <section className="form-stack"><h3>Optional components</h3>
          <Options groups={groups.filter(group => !group.language)} sources={sources} allowLanguage={false}
            update={options => editGroups(false, options)} />
        </section>}
        {activeStage === 'Prerequisites' && <section className="form-stack"><h3>Prerequisite installers</h3>
          <p>The client would request authorization and run these installers in order after preparing the files.</p>
          {discovery}
          <Prerequisites items={form.prerequisites || []} groups={groups} sources={sources} initialPlatforms={releasePlatforms} attempted={attempted}
            editId={editId} select={setEditId} update={prerequisites => update({ prerequisites })} />
        </section>}
        {activeStage === 'Launch' && <section className="form-stack"><h3>Launch integration</h3>
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
        </section>}
      </>}
      {activeStage === 'Review installation plan' && <details><summary>Unresolved installation actions</summary>
        <label>Unresolved notes (one per line)<textarea rows={4} value={form.unresolved?.join('\n') || ''} onChange={event => update({ unresolved: event.target.value === '' ? [] : event.target.value.split('\n') })} /></label>
      </details>}
    </fieldset>
    {activeStage === 'Review installation plan' && <section className="form-stack"><h3>Installation preview</h3>
      <label>Preview platform<select value={platform} onChange={event => { setPlatform(event.target.value); setPreview(undefined) }}>{platforms.map(value => <option key={value} value={value}>{platformName(value)}</option>)}</select></label>
      <OptionsPreview groups={groups} values={selections} update={values => { setSelections(values); setPreview(undefined) }} />
      <button type="button" className="button button-secondary" disabled={busy} onClick={() => void action('preview')}>Validate and preview</button>
      <ol className="installation-plan">
        <li><strong>Download and verify content</strong><span>{plan ? fileSize(plan.bytes) + ' logical installed bytes' : 'Resolve the plan to see output size'}</span></li>
        <li><strong>Prepare payload</strong><span>{plan ? `${plan.files} files · ${plan.directories} directories` : 'Assemble base files'} · {form.recipePayload ? 'Recipe layout' : `${copyCount} selected copy operations`}</span></li>
        <li><strong>Apply localization</strong><span>{form.localization?.mode === 'fixed' ? form.localization.language : form.localization?.mode === 'bundled' ? form.localization.languages.join(', ') : groups.some(group => group.language) ? (selectedChoices.filter(choice => choice.language).map(choice => choice.name).join(', ') || 'Choose a language') : 'Unconfirmed'}</span></li>
        <li><strong>Apply components</strong><span>{selectedChoices.filter(choice => !choice.language).map(choice => choice.name).join(', ') || 'No selections'}</span></li>
        <li><strong>Execute prerequisites with authorization</strong><span>{(preview?.prerequisites || form.prerequisites || []).map(item => `${item.name || 'Unnamed installer'} (${item.optional ? 'optional' : 'required'})`).join(' → ') || 'No installers'}</span></li>
        <li><strong>Activate installation</strong><span>Client activates the prepared files after required steps succeed.</span></li>
        <li><strong>Make the launch entry point available</strong><span>{layout?.launch?.path || form.launch?.destination || 'No launch selected'}</span></li>
      </ol>
      <p className="subtle">This is a configuration preview. Execution requires a compatible client.</p>
      {plan && <div aria-live="polite" className="form-stack"><strong>Valid for {platformName(platform)}</strong>
        {(preview?.unresolved.length ?? 0) > 0 && <p>{preview?.unresolved.length} unresolved actions · review the notes above.</p>}
        <button type="button" className="button button-secondary" aria-expanded={browse} onClick={() => setBrowse(!browse)}>{browse ? 'Hide resulting files' : 'Browse resulting files'}</button>
        {browse && <ResolvedTreePicker entries={entries} identity={`preview:${platform}:${JSON.stringify(selections)}`} />}
      </div>}
    </section>}
    <div className="installation-actions"><p aria-live="polite">{pending === 'preview' ? 'Validating…' : pending === 'save' ? 'Saving…' : error instanceof ApiError && error.status === 412 ? 'Conflict · a newer declaration exists. Your edits are still here. Open Advanced in another tab to compare, or discard changes to reload.' : error ? 'Request failed' : problem ? 'Invalid configuration' : preview ? 'Valid preview' : dirty ? 'Unsaved changes' : 'Saved'}</p>
    <div className="form-actions">
      <button type="button" className="button button-primary" disabled={busy} onClick={() => {
        if (form.unresolved?.some(item => !item.trim())) {
          showIssue('Review installation plan', 'Remove blank lines from unresolved notes before saving.')
        } else void action('save')
      }}>Save installation configuration</button>
      <button type="button" className="button button-secondary" disabled={busy || !dirty} onClick={() => {
        if (!window.confirm('Discard unsaved installation changes?')) return
        if (error instanceof ApiError && error.status === 412) refresh()
        else { setForm(record(raw)?.version === 1 ? raw as Manifest : null); setPreview(undefined); setBrowse(false); setError(undefined); setProblem('') }
      }}>Discard changes</button>
      <button type="button" className="button button-secondary" disabled={busy} onClick={() => {
        if (window.confirm('Remove the installation configuration? You can review and save the removal next.')) { setForm(null); setPreview(undefined) }
      }}>Clear configuration</button>
    </div>
    </div>
    {error !== undefined && <ProblemNotice error={error} />}
  </div>
}

export function InstallationConfiguration({ editable, ...props }: EditorProps & { editable: boolean }) {
  const configuration = record(depot(props.snapshot.release.data)?.installation)
  const localization = record(configuration?.localization)
  const languageSummary = localization?.mode === 'fixed' ? `Fixed application language · ${text(localization.language)}`
    : localization?.mode === 'bundled' ? `Bundled application languages · ${strings(localization.languages).join(', ')}`
    : localization?.mode === 'selectable' ? 'Application language affects installed files'
    : localization?.mode === 'unknown' ? 'Application localization is unconfirmed' : ''
  if (!editable && !configuration) return null
  let summary = 'Optional configuration'
  if (configuration) summary = configuration.version === 1 ? 'Configured' : 'Unsupported installation manifest'
  return <section className="panel installation-workspace"><h2>Installation Configuration</h2>
    <p>{summary}</p>
    {languageSummary && <p>{languageSummary}</p>}
    {editable && <><p className="subtle">Saving creates a declaration revision. Artifact files are immutable.</p><Editor {...props} /></>}
  </section>
}
