// Declaration data is arbitrary JSON. Keep imprecise numeric lexemes as JSON raw values so
// editing another field cannot round an unrelated number. Unsupported browsers fail closed.
export function parseExactJson(text: string): unknown {
  const raw = (JSON as typeof JSON & { rawJSON?: (source: string) => unknown }).rawJSON
  return JSON.parse(text, (_key: string, value: unknown, context?: { source?: string }) => {
    const source = context?.source
    if (typeof value === 'number' && !Number.isSafeInteger(value) && !source) {
      throw new Error('This browser cannot preserve exact JSON numbers in declarations.')
    }
    if (typeof value === 'number' && source && JSON.stringify(value) !== source) {
      if (!raw) throw new Error('This browser cannot preserve exact JSON numbers. Use a browser with JSON.rawJSON support.')
      return raw(source)
    }
    return value
  })
}
