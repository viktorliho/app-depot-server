import { useEffect, useState } from 'react'

export function useResource<T>(key: string, load: (signal: AbortSignal) => Promise<T>) {
  const [revision, setRevision] = useState(0)
  const [state, setState] = useState<{ key: string; value?: T; error?: unknown }>({ key })
  useEffect(() => {
    const controller = new AbortController()
    load(controller.signal).then(value => setState({ key, value })).catch(error => {
      if (!controller.signal.aborted) setState({ key, error })
    })
    return () => controller.abort()
    // The caller's key defines the request; a new inline load function must not restart it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, revision])
  return { value: state.key === key ? state.value : undefined, error: state.key === key ? state.error : undefined,
    loading: state.key !== key || (!state.value && !state.error), retry: () => { setState({ key }); setRevision(value => value + 1) } }
}
