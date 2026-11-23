import { platformName } from './installation-review'
import { Link } from 'react-router'
import type { DeclarationSnapshot } from './api'

export const record = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : undefined
export const depot = (data: unknown) => record(record(data)?.appDepot)
export const manifest = (data: unknown) => {
  const value = record(depot(data)?.gogOfflineInstaller)
  return value?.version === 1 ? value : undefined
}
export const text = (value: unknown) => typeof value === 'string' ? value : ''
export const strings = (value: unknown) => Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : []
export function fileSize(value: unknown) {
  if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) return 'Unknown'
  if (value < 1024) return `${value.toLocaleString()} bytes`
  const units = ['KiB', 'MiB', 'GiB', 'TiB']
  const index = Math.min(units.length - 1, Math.floor(Math.log(value) / Math.log(1024)) - 1)
  return `${(value / (1024 ** (index + 1))).toLocaleString(undefined, { maximumFractionDigits: 2 })} ${units[index]}`
}

export function ReleaseLaunches({ snapshot, path, editable }: { snapshot: DeclarationSnapshot; path: string; editable: boolean }) {
  const entries = snapshot.associations.flatMap(({ associationId, declaration }) => {
    const points = depot(declaration.data)?.entryPoints
    return Array.isArray(points) ? points.flatMap(point => record(point) ? [{ associationId, point }] : []) : []
  })
  return <section className="panel"><h2>Launch entry points</h2>
    {entries.length ? <ul>{entries.map(({ associationId, point }, index) => {
      const target = record(point.target)
      const targetName = text(target?.path) || (target?.type === 'ArtifactFile' ? 'Single file' : 'Unrecognized target')
      return <li key={index}><strong>{text(point.label) || targetName}</strong>{text(point.label) && text(point.label) !== targetName && <> · {targetName}</>}
        {strings(point.platforms).length > 0 && <> · {strings(point.platforms).map(platformName).join(', ')}</>}
        {' · '}<Link to={`${path}/artifacts/${associationId}#entry-points-heading`}>{editable ? 'Edit entry points' : 'View entry points'}</Link>
      </li>
    })}</ul> : <p>No entry points.{editable && <> <Link to={`${path}?view=artifacts`}>Browse Artifacts</Link> and choose a launch file.</>}</p>}
  </section>
}

export function GogInstallation({ snapshot, path }: { snapshot: DeclarationSnapshot; path: string }) {
  const imports = snapshot.associations.flatMap(({ associationId, declaration }) => {
    const value = manifest(declaration.data)
    return value ? [{ associationId, value, points: depot(declaration.data)?.entryPoints }] : []
  })
  if (!imports.length) return null
  return <section className="panel"><h2>Installation information</h2>{imports.map(({ associationId, value, points }) => {
    const analysis = record(value.analysis)
    const user = record(value.user)
    const payload = record(value.payload)
    const candidate = text(user?.candidateExecutable)
    const candidates = strings(record(value.detected)?.candidateExecutables)
    const declared = !!candidate && Array.isArray(points) && points.some(point => record(record(point)?.target)?.path === candidate)
    const groups = Object.entries(record(analysis?.languageTaggedFiles) || {}).flatMap(([language, count]) =>
      typeof count === 'number' ? [`${language} (${count.toLocaleString()})`] : [])
    const redistributables = strings(record(value.detected)?.redistributableRoots)
    const actions = strings(analysis?.unknownActions)
    return <div key={associationId}>
      <h3>GOG Offline Installer</h3>

      <dl className="facts">
        <div><dt>Installer product</dt><dd>{text(analysis?.productName) || 'Unknown'}</dd></div>
        <div><dt>Recovered game payload</dt><dd>{typeof payload?.fileCount === 'number' && `${payload.fileCount.toLocaleString()} files · `}{fileSize(payload?.size)}</dd></div>
        {text(user?.productName) && <div><dt>Product name (user provided)</dt><dd>{text(user?.productName)}</dd></div>}
        {text(user?.gameVersion) && <div><dt>Game version (user provided)</dt><dd>{text(user?.gameVersion)}</dd></div>}
        {!candidate && candidates.length > 0 && <div><dt>Executable candidates (detected by path)</dt><dd>
          {candidates[0]}{candidates.length > 1 && ` · ${candidates.length - 1} more in the manifest`}
        </dd></div>}
        {candidate && <div><dt>{declared ? 'Declared launch file' : 'Selected launch candidate (user provided)'}</dt><dd>{candidate} · <Link to={`${path}/artifacts/${associationId}#entry-points-heading`}>Open entry point editor</Link></dd></div>}
        {text(user?.notes) && <div><dt>Installation notes (user provided)</dt><dd>{text(user?.notes)}</dd></div>}
        <div><dt>Installation readiness</dt><dd>Unverified{actions.length > 0 && ` · ${actions.length} setup topics to review`}</dd></div>
      </dl>
      <details><summary>Source metadata and unresolved setup actions</summary>
        <dl className="facts">
        <div><dt>Installer format and version</dt><dd>{[text(analysis?.installerFormat), text(analysis?.installerVersion)].filter(Boolean).join(' ') || 'Unknown'}</dd></div>
        <div><dt>Extractor</dt><dd>{[text(analysis?.engine), text(analysis?.engineVersion)].filter(Boolean).join(' ') || 'Unknown'}</dd></div>
        <div><dt>Installer UI languages</dt><dd>{strings(analysis?.installerLanguages).join(', ') || 'Unknown'}</dd></div>
        <div><dt>Language-tagged file groups</dt><dd>{groups.join(', ') || 'None reported'}<br />Complete game language support is unknown.</dd></div>
        <div><dt>Redistributable directories (detected by path)</dt><dd>{redistributables.length ? <ul>{redistributables.map(item => <li key={item}><code>{item}</code></li>)}</ul> : 'None detected'}<br />Prerequisite requirements and order are unknown.</dd></div>
        </dl>
        {actions.length > 0 && <ul>{actions.map(action => <li key={action}>{action}</li>)}</ul>}
      </details>
    </div>
  })}</section>
}
