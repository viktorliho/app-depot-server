export type DiscoveryReport = {
  suggestedLaunch: string | null
  packages: { path: string; name: string; family?: string; architecture: string; status: string; evidence: string; supportingFiles: string[] }[]
  unresolved: string[]
  sourceObservations?: {
    reportedApplicationLanguages: string[]
    setupActions: { action: string; target: string | null; originalArgumentText: string | null; requiresElevation: boolean; conditions: string[] }[]
  }
}

const statuses: Record<string, string> = {
  alternative: 'Possible alternative', duplicate: 'Duplicate', requiresMsi: 'MSI execution is outside V1', unresolved: 'Needs review',
}

type DetectedPackage = DiscoveryReport['packages'][number]

function PackageDetails({ item }: { item: DetectedPackage }) {
  return <details><summary>Package details</summary>
    <p><code>{item.path}</code> · {item.architecture}</p><p>{item.evidence}</p>
    {!!item.supportingFiles.length && <ul>{item.supportingFiles.map(path => <li key={path}><code>{path}</code></li>)}</ul>}
  </details>
}

export function DiscoveryReview({ report, disabled, change }: {
  report: DiscoveryReport; disabled: string[]; change: (paths: string[]) => void
}) {
  const proposed = report.packages.filter(item => item.status === 'ready')
  const retained = new Map<string, DetectedPackage[]>()
  for (const item of report.packages) {
    if (item.status === 'ready') continue
    const family = item.family || item.name
    retained.set(family, [...(retained.get(family) || []), item])
  }
  return <section className="form-stack">
    <h4>Detected prerequisites</h4>
    {proposed.length ? <>
      <p>Selected installers enter the proposed plan in App Depot’s default order.</p>
      <ol className="installation-plan">{proposed.map(item => <li key={item.path}>
        <label className="checkbox-row"><input type="checkbox" checked={!disabled.includes(item.path)}
          onChange={event => change(event.target.checked ? disabled.filter(path => path !== item.path) : [...disabled, item.path])} />{item.name}</label>
        <PackageDetails item={item} />
      </li>)}</ol>
    </> : <p>{report.packages.length ? 'No installer can be selected automatically.' : 'No recognized packages.'}</p>}
    {retained.size > 0 && <div className="form-stack">
      <h4>Files retained for review</h4>
      <p>These files stay in the Artifact. They are outside the proposed execution order.</p>
      {[...retained.entries()].map(([family, items]) => <section key={family} className="installer-card">
        <strong>{items[0].name}</strong>
        {items.some(item => item.status === 'alternative') && <p>Multiple packages match this family. Review the alternatives before choosing an installer.</p>}
        <ul>{items.map(item => <li key={item.path}>
          {statuses[item.status] || item.status} · <code>{item.path}</code>
          <PackageDetails item={item} />
        </li>)}</ul>
      </section>)}
    </div>}
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
