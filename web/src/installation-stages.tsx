export function InstallationStages<T extends string>({ stages, active, onSelect }: {
  stages: { title: T; summary: string }[]; active: T; onSelect: (title: T) => void
}) {
  return <nav className="installation-stages" aria-label="Installation editor sections">
    {stages.map(({ title, summary }) => <button type="button" key={title} className="button button-secondary"
      aria-pressed={active === title} onClick={() => onSelect(title)}>
      <strong>{title}</strong><span>{summary}</span>
    </button>)}
  </nav>
}
