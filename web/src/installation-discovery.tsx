export type DiscoveryReport = {
  suggestedLaunch: string | null
  packages: { path: string; name: string; architecture: string; status: string; evidence: string; supportingFiles: string[] }[]
  unresolved: string[]
  sourceObservations?: {
    reportedApplicationLanguages: string[]
    setupActions: { action: string; target: string | null; originalArgumentText: string | null; requiresElevation: boolean; conditions: string[] }[]
  }
}

const statuses: Record<string, string> = {
  alternative: 'Possible alternative', duplicate: 'Duplicate', requiresMsi: 'MSI execution is outside V1', unresolved: 'Needs review',
}

export function DiscoveryReview({ report, disabled, change }: {
  report: DiscoveryReport; disabled: string[]; change: (paths: string[]) => void
}) {
  return <section>
    <h4>Detected prerequisites</h4>
    {report.packages.length ? <p>Enabled installers run in the order shown. This is App Depot’s default order. Original installer arguments and order are unknown.</p> : <p>No recognized packages.</p>}
    <ol>{report.packages.map(item => <li key={item.path}>
      {item.status === 'ready' ? <label className="checkbox-row">
        <input type="checkbox" checked={!disabled.includes(item.path)}
          onChange={event => change(event.target.checked ? disabled.filter(path => path !== item.path) : [...disabled, item.path])} />{item.name}
      </label> : <span>{item.name} · {statuses[item.status] || item.status}</span>}
      <details><summary>Package details</summary><p><code>{item.path}</code> · {item.architecture}</p><p>{item.evidence}</p>
        {!!item.supportingFiles.length && <ul>{item.supportingFiles.map(path => <li key={path}><code>{path}</code></li>)}</ul>}
      </details>
    </li>)}</ol>
    {!!report.sourceObservations?.reportedApplicationLanguages.length && <p>
      Application languages reported by GOG file metadata: {report.sourceObservations.reportedApplicationLanguages.join(', ')}. Packaging is unconfirmed.
    </p>}
    {!!report.sourceObservations?.setupActions.length && <details><summary>Original setup actions for review</summary>
      <p>These source observations do not become executable installation steps.</p>
      <ol>{report.sourceObservations.setupActions.map((item, index) => <li key={index}>
        {item.action}{item.target && <> · <code>{item.target}</code></>}
        {item.originalArgumentText && <> · original arguments <code>{item.originalArgumentText}</code></>}
        {item.requiresElevation && ' · requests administrator privileges'}
        {!!item.conditions.length && <> · {item.conditions.join(', ')}</>}
      </li>)}</ol>
    </details>}
    {!!report.unresolved.length && <ul>{report.unresolved.map(note => <li key={note}>{note}</li>)}</ul>}
  </section>
}
