import { useCallback, useEffect, useRef, useState } from 'react'
import type { ActivityEvent } from '../types'
import { useBackend } from '../backend'

// The newest activity events, for one vault or all, kept fresh on
// `state://changed`.
export function useActivity(
  vaultId: string | null,
  limit: number,
  onError: (error: unknown) => void,
) {
  const backend = useBackend()
  const [events, setEvents] = useState<ActivityEvent[]>([])
  const onErrorRef = useRef(onError)
  onErrorRef.current = onError

  const refresh = useCallback(async () => {
    try {
      setEvents(await backend.listActivity(vaultId, limit))
    } catch (error) {
      onErrorRef.current(error)
    }
  }, [backend, vaultId, limit])

  useEffect(() => {
    void refresh()
    return backend.on('state://changed', () => void refresh())
  }, [backend, refresh])

  return { events, refresh }
}
