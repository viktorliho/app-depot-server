import { useState } from 'react'
import { useOutletContext, useParams, useSearchParams } from 'react-router'
import type { DepotApp } from './api'
import { PageHeader } from './components'
import { ServerImportPage } from './server-import'
import { UploadPage } from './uploads'

export type IngestionTarget = 'new-app' | 'app' | 'release'
export type IngestionMode = 'BareFile' | 'BareTree' | 'ZipImport'
type Source = 'device' | 'server'

export function ArtifactIngestionPage({ target, initialSource = 'device' }: { target: IngestionTarget; initialSource?: Source }) {
  const app = useOutletContext<DepotApp | undefined>()
  const { releaseId } = useParams()
  const [params, setParams] = useSearchParams()
  const [busy, setBusy] = useState(false)
  const source: Source = params.get('source') === 'server' ? 'server' : params.get('source') === 'device' ? 'device' : initialSource
  const requestedMode = params.get('mode')
  const mode: IngestionMode = requestedMode === 'BareTree' || requestedMode === 'ZipImport' ? requestedMode : 'BareFile'
  function choose(nextSource: Source, nextMode: IngestionMode) { setParams({ source: nextSource, mode: nextMode }) }
  const description = target === 'new-app' ? 'Create an App and Draft Release while registering original content.'
    : target === 'app' ? `Create a Draft Release in ${app?.name || 'this App'} while registering original content.`
      : `Register original content in the selected Draft Release of ${app?.name || 'this App'}.`
  return <><PageHeader title="Add Artifact" description={description} />
    <section className="panel form-panel" aria-label="Artifact selection"><div className="form-stack">
      <label>Source<select value={source} disabled={busy} onChange={event => choose(event.target.value as Source, mode)}>
        <option value="device">Upload from this device</option><option value="server">Import from server storage</option>
      </select></label>
      <label>Artifact type<select value={mode} disabled={busy} onChange={event => choose(source, event.target.value as IngestionMode)}>
        <option value="BareFile">Single file</option><option value="BareTree">Directory tree</option><option value="ZipImport">ZIP import</option>
      </select></label>
      <p className="subtle">Single file keeps a ZIP as an opaque File Artifact. ZIP import extracts its contents as a Tree Artifact. Registration saves a Draft; publishing always requires an explicit action.</p>
    </div></section>
    {source === 'device'
      ? <UploadPage key={`${target}:${app?.id || ''}:${releaseId || ''}:${mode}`} target={target} mode={mode} onBusyChange={setBusy} />
      : <ServerImportPage key={`${target}:${app?.id || ''}:${releaseId || ''}:${mode}`} target={target} mode={mode} onBusyChange={setBusy} />}
  </>
}
