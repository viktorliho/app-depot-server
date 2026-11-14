const textEncoder = new TextEncoder()

export function pathOrder(left: string, right: string) {
  const a = textEncoder.encode(left)
  const b = textEncoder.encode(right)
  for (let index = 0; index < Math.min(a.length, b.length); index++) if (a[index] !== b[index]) return a[index] - b[index]
  return a.length - b.length
}

export function isOsMetadataPath(path: string) {
  return path.split('/').some(part =>
    part === '.DS_Store' || part.startsWith('._') ||
    part === '.Spotlight-V100' || part === '.Trashes' || part === '.fseventsd' ||
    /^(thumbs\.db|ehthumbs\.db|desktop\.ini)$/i.test(part))
}

export function selectTreeFiles<T extends Pick<File, 'name' | 'webkitRelativePath'>>(files: readonly T[], excludeOsMetadata: boolean) {
  const selected = files.map(file => ({ file, path: file.webkitRelativePath || file.name }))
  const included = excludeOsMetadata ? selected.filter(item => !isOsMetadataPath(item.path)) : selected
  return { files: included.sort((a, b) => pathOrder(a.path, b.path)), excludedCount: selected.length - included.length }
}
