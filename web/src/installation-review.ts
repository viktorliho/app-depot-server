export const publisherEvidence = 'Added manually by publisher. Execution order follows the configured App Depot plan.'

export function platformName(value: string) {
  const [system, arch] = value.split('/')
  const names: Record<string, string> = { windows: 'Windows', linux: 'Linux', macos: 'macOS' }
  return system in names && arch ? `${names[system]} ${arch}` : value
}

export function prerequisiteIssue(item: { name: string; associationId: string; platforms: string[]; evidence: string; path?: string }, tree: boolean) {
  if (!item.name.trim()) return { field: 'name', message: 'Enter a package name.' }
  if (!item.associationId || (tree && !item.path)) return { field: 'source', message: 'Choose the installer file from an Artifact.' }
  if (!item.platforms.length) return { field: 'platforms', message: 'Choose at least one target platform.' }
  if (!item.evidence.trim()) return { field: 'evidence', message: 'Describe where this installer and its settings came from.' }
}

export function planTotals(files: Record<string, { size?: number }>, directories: string[]) {
  return { files: Object.keys(files).length, directories: directories.length,
    bytes: Object.values(files).reduce((sum, entry) => sum + (entry.size ?? 0), 0) }
}

export function isRelativeInstallPath(value: string) {
  return value === '' || (!/[\\:\u0000-\u001f\u007f]/.test(value) &&
    value.split('/').every(segment => segment !== '' && segment !== '.' && segment !== '..'))
}
