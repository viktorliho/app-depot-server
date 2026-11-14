import { useState, type FormEvent } from 'react'

export type MetadataScope = 'app' | 'release' | 'association'

type Draft = {
  summary: string; description: string; kind: string; developer: string; publisher: string; homepageUrl: string; license: string; tags: string; categories: string
  notes: string; platforms: string; runtimeName: string; runtimeVersion: string
  label: string; purpose: string
}

const empty: Draft = { summary: '', description: '', kind: '', developer: '', publisher: '', homepageUrl: '', license: '', tags: '', categories: '', notes: '', platforms: '',
  runtimeName: '', runtimeVersion: '', label: '', purpose: '' }
const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === 'object' && !Array.isArray(value)

function documentBlock(text: string): { data: Record<string, unknown>; block: Record<string, unknown> } {
  let parsed: unknown
  try { parsed = JSON.parse(text) } catch { throw new Error('Correct the JSON value before using standard fields.') }
  if (!object(parsed)) throw new Error('Standard fields require a JSON object. Use the JSON editor to preserve other values, including null.')
  const block = parsed.appDepot
  if (block !== undefined && !object(block)) throw new Error('appDepot must be an object to use standard fields. The JSON editor can still preserve this value.')
  return { data: parsed, block: block || {} }
}

function readString(block: Record<string, unknown>, name: string): string {
  const value = block[name]
  if (value === undefined) return ''
  if (typeof value !== 'string') throw new Error(`appDepot.${name} must be a string to use standard fields.`)
  return value
}

function readList(block: Record<string, unknown>, name: string): string {
  const value = block[name]
  if (value === undefined) return ''
  if (!Array.isArray(value) || value.some(item => typeof item !== 'string')) throw new Error(`appDepot.${name} must be a list of strings to use standard fields.`)
  return value.join('\n')
}

function readDraft(text: string, scope: MetadataScope): Draft {
  const { block } = documentBlock(text)
  const draft = { ...empty }
  if (scope === 'app') {
    draft.summary = readString(block, 'summary'); draft.description = readString(block, 'description')
    draft.kind = readString(block, 'kind'); draft.tags = readList(block, 'tags')
    draft.developer = readString(block, 'developer'); draft.publisher = readString(block, 'publisher')
    draft.homepageUrl = readString(block, 'homepageUrl'); draft.license = readString(block, 'license')
    draft.categories = readList(block, 'categories')
  } else if (scope === 'release') {
    draft.notes = readString(block, 'notes'); draft.platforms = readList(block, 'platforms')
    if (block.runtime !== undefined && !object(block.runtime)) throw new Error('appDepot.runtime must be an object to use standard fields.')
    const runtime = (block.runtime || {}) as Record<string, unknown>
    draft.runtimeName = readString(runtime, 'name'); draft.runtimeVersion = readString(runtime, 'version')
  } else {
    draft.label = readString(block, 'label'); draft.description = readString(block, 'description')
    draft.purpose = readString(block, 'purpose')
  }
  return draft
}

export function supportsStandardFields(text: string, scope: MetadataScope): boolean {
  try { readDraft(text, scope); return true } catch { return false }
}

function optionalString(value: string, label: string, max: number, multiline = false): string | undefined {
  const trimmed = value.trim()
  if (!trimmed) return undefined
  if (trimmed.length > max) throw new Error(`${label} must be at most ${max} characters.`)
  if (multiline ? /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(trimmed) : /[\u0000-\u001f\u007f]/.test(trimmed)) {
    throw new Error(`${label} contains a control character.`)
  }
  return trimmed
}

function optionalList(value: string, label: string): string[] | undefined {
  const items = value.split('\n').map(item => item.trim()).filter(Boolean)
  if (!items.length) return undefined
  if (items.length > 32) throw new Error(`${label} may contain at most 32 entries.`)
  const seen = new Set<string>()
  for (const item of items) {
    optionalString(item, label, 80)
    if (seen.has(item)) throw new Error(`${label} contains a duplicate entry.`)
    seen.add(item)
  }
  return items
}

function optionalHomepage(value: string): string | undefined {
  const url = optionalString(value, 'Homepage URL', 2048)
  if (!url) return undefined
  try {
    const parsed = new URL(url)
    if (parsed.protocol !== 'https:' && parsed.protocol !== 'http:') throw new Error()
    if (!parsed.hostname || parsed.username || parsed.password) throw new Error()
  } catch { throw new Error('Homepage URL must be an absolute HTTP or HTTPS URL without credentials.') }
  return url
}

function assign(block: Record<string, unknown>, name: string, value: unknown) {
  if (value === undefined) delete block[name]
  else block[name] = value
}

function build(text: string, scope: MetadataScope, draft: Draft): Record<string, unknown> {
  const { data, block: existing } = documentBlock(text)
  // Read first so malformed known fields cannot be silently replaced; unknown extension fields survive.
  readDraft(text, scope)
  const block = { ...existing }
  if (scope === 'app') {
    assign(block, 'summary', optionalString(draft.summary, 'Summary', 280))
    assign(block, 'description', optionalString(draft.description, 'Description', 10000, true))
    assign(block, 'kind', optionalString(draft.kind, 'Kind', 80))
    assign(block, 'developer', optionalString(draft.developer, 'Developer', 160))
    assign(block, 'publisher', optionalString(draft.publisher, 'Publisher', 160))
    assign(block, 'homepageUrl', optionalHomepage(draft.homepageUrl))
    assign(block, 'license', optionalString(draft.license, 'License', 160))
    assign(block, 'tags', optionalList(draft.tags, 'Tags'))
    assign(block, 'categories', optionalList(draft.categories, 'Categories'))
  } else if (scope === 'release') {
    assign(block, 'notes', optionalString(draft.notes, 'Release notes', 10000, true))
    assign(block, 'platforms', optionalList(draft.platforms, 'Platforms'))
    const runtime = { ...(object(block.runtime) ? block.runtime : {}) }
    assign(runtime, 'name', optionalString(draft.runtimeName, 'Runtime name', 160))
    assign(runtime, 'version', optionalString(draft.runtimeVersion, 'Runtime version', 160))
    assign(block, 'runtime', Object.keys(runtime).length ? runtime : undefined)
  } else {
    assign(block, 'label', optionalString(draft.label, 'Artifact label', 160))
    assign(block, 'description', optionalString(draft.description, 'Description', 2000, true))
    assign(block, 'purpose', optionalString(draft.purpose, 'Purpose', 160))
  }
  const result = { ...data }
  if (Object.keys(block).length) result.appDepot = block
  else delete result.appDepot
  return result
}

export function StandardMetadataForm({ scope, documentText, editable, busy, onSave }: {
  scope: MetadataScope; documentText: string; editable: boolean; busy: boolean; onSave: (data: unknown) => Promise<void>
}) {
  const [initial] = useState(() => {
    try { return { draft: readDraft(documentText, scope), error: '' } }
    catch (failure) { return { draft: { ...empty }, error: failure instanceof Error ? failure.message : 'Cannot read standard fields.' } }
  })
  const [draft, setDraft] = useState(initial.draft)
  const [error, setError] = useState(initial.error)
  function field(name: keyof Draft, label: string, multiline = false, type = 'text') {
    return <label>{label}{multiline
      ? <textarea rows={name === 'description' || name === 'notes' ? 5 : 3} value={draft[name]} readOnly={!editable} onChange={event => setDraft(current => ({ ...current, [name]: event.target.value }))} />
      : <input type={type} value={draft[name]} readOnly={!editable} onChange={event => setDraft(current => ({ ...current, [name]: event.target.value }))} />}</label>
  }
  function submit(event: FormEvent) {
    event.preventDefault(); setError('')
    try { void onSave(build(documentText, scope, draft)) }
    catch (failure) { setError(failure instanceof Error ? failure.message : 'Invalid standard metadata.') }
  }
  if (initial.error) return <p role="alert">{initial.error}</p>
  return <form className="form-stack form-panel" onSubmit={submit}>
    <p className="subtle">Every field is optional. Saving creates a declaration revision and preserves other JSON properties. This form does not select files or execute content.</p>
    {scope === 'app' && <>{field('summary', 'Summary')}{field('description', 'Description', true)}
      <label htmlFor="standard-kind">Kind</label><select id="standard-kind" value={['', 'Game', 'Application', 'Tool', 'Library', 'Content', 'Other'].includes(draft.kind) ? draft.kind : 'custom'} onChange={event => setDraft(current => ({ ...current, kind: event.target.value === 'custom' ? 'Custom' : event.target.value }))}><option value="">Unspecified</option>{['Game', 'Application', 'Tool', 'Library', 'Content', 'Other'].map(value => <option key={value}>{value}</option>)}<option value="custom">Custom…</option></select>
      {!['', 'Game', 'Application', 'Tool', 'Library', 'Content', 'Other'].includes(draft.kind) && field('kind', 'Custom kind')}
      {field('developer', 'Developer')}{field('publisher', 'Publisher')}{field('homepageUrl', 'Homepage URL', false, 'url')}{field('license', 'License')}{field('tags', 'Tags (one per line)', true)}{field('categories', 'Categories (one per line)', true)}</>}
    {scope === 'release' && <>{field('notes', 'Release notes', true)}<fieldset><legend>Target platforms (optional)</legend><p className="subtle">Select useful presets or add one platform per line. Leave empty for unspecified.</p><div className="form-actions">{['windows/x86_64', 'linux/x86_64', 'linux/aarch64', 'macos/aarch64', 'macos/x86_64'].map(value => <label key={value}><input type="checkbox" checked={draft.platforms.split('\n').includes(value)} disabled={!editable} onChange={event => setDraft(current => ({ ...current, platforms: (event.target.checked ? [...current.platforms.split('\n').filter(Boolean), value] : current.platforms.split('\n').filter(item => item !== value)).join('\n') }))} />{value}</label>)}</div>{field('platforms', 'Platforms (one per line; custom values allowed)', true)}</fieldset>{field('runtimeName', 'Runtime name')}{field('runtimeVersion', 'Runtime version')}</>}
    {scope === 'association' && <>{field('label', 'Artifact label')}{field('description', 'Description', true)}{field('purpose', 'Purpose')}</>}
    {error && <p role="alert">{error}</p>}
    {editable && <button className="button button-primary" disabled={busy}>{busy ? 'Saving…' : 'Save standard metadata'}</button>}
  </form>
}
