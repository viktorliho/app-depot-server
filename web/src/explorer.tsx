import { useEffect, useState } from 'react'
import { Link, useLocation, useOutletContext, useParams, useSearchParams } from 'react-router'
import { apiRequest, type Artifact, type Association, type DepotApp, type Release } from './api'
import { ArtifactBrowser } from './artifact-browser'
import { LoadingState, PageHeader, ProblemNotice } from './components'
import { EntryPointDialog, type EntryTarget } from './entry-point-dialog'
import { useAuth } from './portal'
import { useResource } from './use-resource'

export function ArtifactExplorer() {
  const app = useOutletContext<DepotApp>()
  const { releaseId, associationId } = useParams()
  const { hash } = useLocation()
  const { state } = useAuth()
  const [params, setParams] = useSearchParams()
  const [editor, setEditor] = useState<EntryTarget | 'manage' | null>(null)
  const [notice, setNotice] = useState('')
  const release = useResource(`explorer-release:${releaseId}`, signal => apiRequest<Release>(`/releases/${releaseId}`, { authenticated: true, signal }).then(result => result.value))
  const association = useResource(`explorer-association:${associationId}`, signal => apiRequest<Association>(`/associations/${associationId}`, { authenticated: true, signal }).then(result => result.value))
  const validContext = release.value?.appId === app.id && association.value?.releaseId === releaseId
  const artifactId = validContext ? association.value?.artifactId : undefined
  const artifact = useResource(`explorer-artifact:${artifactId || ''}`, signal => artifactId
    ? apiRequest<Artifact>(`/artifacts/${encodeURIComponent(artifactId)}`, { authenticated: true, signal }).then(result => result.value)
    : Promise.resolve(null))
  const allowed = state.session!.user.serverAdmin || state.session!.user.id === app.ownerId
  useEffect(() => {
    setEditor(hash === '#entry-points-heading' ? 'manage' : null)
    setNotice('')
  }, [associationId, hash])
  if (release.loading || association.loading || (artifactId && artifact.loading)) return <LoadingState label="Loading Artifact…" />
  if (release.error || association.error || artifact.error) return <ProblemNotice error={release.error || association.error || artifact.error}
    retry={() => { release.retry(); association.retry(); artifact.retry() }} />
  if (!validContext || !artifact.value) return <ProblemNotice error={new Error('This Artifact association does not belong to the selected Release and application.')} />
  const item = artifact.value
  return <><PageHeader title={item.kind === 'Tree' ? 'Artifact Explorer' : 'File Artifact'}
    description={`Original content in ${release.value!.version || 'Unversioned release'}`}
    actions={<button type="button" className="button button-secondary" onClick={() => { setNotice(''); setEditor('manage') }}>Entry points</button>} />
    <p><Link to={`/apps/${app.id}/releases/${releaseId}`}>← Back to Release</Link></p>
    {notice && <p role="status" className="success-notice">{notice}</p>}
    <section className="panel" aria-label="Original content">
      <ArtifactBrowser key={associationId} artifactId={item.id} size={item.size}
        position={{ directory: params.get('dir') || '', search: params.get('q') || '' }}
        onNavigate={({ directory, search }) => setParams({ ...(directory ? { dir: directory } : {}), ...(search ? { q: search } : {}) })}
        actions={entry => allowed && entry.type === 'File' ? <button type="button" className="button button-secondary" onClick={() => {
          setNotice('')
          setEditor(entry.path ? { type: 'TreeFile', path: entry.path } : { type: 'ArtifactFile' })
        }}>Add entry point</button> : null} />
    </section>
    {editor && <EntryPointDialog item={item} releaseId={releaseId!} associationId={associationId!} allowed={allowed}
      target={editor === 'manage' ? undefined : editor} close={() => setEditor(null)}
      saved={() => { setEditor(null); setNotice('Entry points saved as a new declaration revision.') }} />}
  </>
}
