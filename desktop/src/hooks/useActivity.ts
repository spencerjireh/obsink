import { useCallback, useEffect, useRef, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import type { ActivityEvent } from '../types'
import { call } from '../lib/tauri'

// The newest activity events, for one vault or all, kept fresh on
// `state://changed`.
export function useActivity(
  vaultId: string | null,
  limit: number,
  onError: (error: unknown) => void,
) {
  const [events, setEvents] = useState<ActivityEvent[]>([])
  const onErrorRef = useRef(onError)
  onErrorRef.current = onError

  const refresh = useCallback(async () => {
    try {
      setEvents(await call<ActivityEvent[]>('list_activity', { vaultId, limit }))
    } catch (error) {
      onErrorRef.current(error)
    }
  }, [vaultId, limit])

  useEffect(() => {
    void refresh()
    const unlisten = listen('state://changed', () => void refresh())
    return () => {
      void unlisten.then((dispose) => dispose())
    }
  }, [refresh])

  return { events, refresh }
}
