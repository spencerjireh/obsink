import { useCallback, useEffect, useRef, useState } from 'react'
import type { VaultStateInfo } from '../types'
import { useBackend, type BackendEvent } from '../backend'

// Every configured vault with its state. Re-read on mount, whenever the
// backend says something changed, and on any extra event names given (the
// popover passes `popover://opened`).
export function useVaultStates(
  onError: (error: unknown) => void,
  extraEvents: BackendEvent[] = [],
) {
  const backend = useBackend()
  const [states, setStates] = useState<VaultStateInfo[]>([])
  const [loaded, setLoaded] = useState(false)
  const onErrorRef = useRef(onError)
  onErrorRef.current = onError
  // One read at a time; a change that lands mid-read queues one more.
  const inFlight = useRef(false)
  const again = useRef(false)

  const refresh = useCallback(async () => {
    if (inFlight.current) {
      again.current = true
      return
    }
    inFlight.current = true
    try {
      do {
        again.current = false
        setStates(await backend.getVaultStates())
        setLoaded(true)
      } while (again.current)
    } catch (error) {
      onErrorRef.current(error)
    } finally {
      inFlight.current = false
    }
  }, [backend])

  useEffect(() => {
    void refresh()
    const names: BackendEvent[] = ['state://changed', ...extraEvents]
    const unsubscribes = names.map((name) => backend.on(name, () => void refresh()))
    return () => {
      for (const unsubscribe of unsubscribes) unsubscribe()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [backend, refresh])

  return { states, loaded, refresh }
}
