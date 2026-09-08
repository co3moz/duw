import { useEffect, useRef, useState } from 'react'
import { api, type FullState, type Progress } from './api'

/**
 * Subscribes to the scanner's SSE stream. `version` changes on every batch of
 * newly scanned entries; views use it as a cue to refetch.
 */
export function useLive() {
  const [state, setState] = useState<FullState | null>(null)
  const [progress, setProgress] = useState<Progress | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const ac = new AbortController()
    api
      .state(ac.signal)
      .then((s) => {
        setState(s)
        setProgress(s)
      })
      .catch((e) => {
        if (!ac.signal.aborted) setError(String(e))
      })

    const es = new EventSource('/api/events')
    const onTick = (ev: MessageEvent) => {
      try {
        setProgress(JSON.parse(ev.data) as Progress)
      } catch {
        /* ignore malformed frames */
      }
    }
    es.addEventListener('progress', onTick)
    es.addEventListener('done', onTick)
    es.onerror = () => setError('lost connection to duw')

    return () => {
      ac.abort()
      es.close()
    }
  }, [])

  return { state, progress, error }
}

/**
 * Follows `version` but changes at most once every `ms`, so a fast scan does
 * not turn into a fetch storm. The final value always lands.
 */
export function useThrottled(version: number, ms: number): number {
  const [value, setValue] = useState(version)
  const last = useRef(0)
  const timer = useRef<number | undefined>(undefined)

  useEffect(() => {
    const since = Date.now() - last.current
    if (since >= ms) {
      last.current = Date.now()
      setValue(version)
      return
    }
    window.clearTimeout(timer.current)
    timer.current = window.setTimeout(() => {
      last.current = Date.now()
      setValue(version)
    }, ms - since)
    return () => window.clearTimeout(timer.current)
  }, [version, ms])

  return value
}

/** Re-runs `load` whenever its inputs change, dropping out-of-order responses. */
export function useResource<T>(load: (signal: AbortSignal) => Promise<T>, deps: unknown[]) {
  const [data, setData] = useState<T | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const ac = new AbortController()
    load(ac.signal)
      .then((d) => {
        if (!ac.signal.aborted) {
          setData(d)
          setError(null)
        }
      })
      .catch((e) => {
        if (!ac.signal.aborted) setError(String(e))
      })
    return () => ac.abort()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps)

  return { data, error }
}
