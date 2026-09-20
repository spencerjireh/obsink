import { useCallback, useEffect, useRef, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import type { VaultStateInfo } from '../types'
import { call } from '../lib/tauri'

// Every configured vault with its state. Re-read on mount, whenever Rust
// says something changed, and on any extra event names given (the popover
// passes `popover://opened`).
export function useVaultStates(onError: (error: unknown) => void, extraEvents: string[] = []) {
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
        setStates(await call<VaultStateInfo[]>('get_vault_states'))
        setLoaded(true)
      } while (again.current)
    } catch (error) {
      onErrorRef.current(error)
    } finally {
      inFlight.current = false
    }
  }, [])

  useEffect(() => {
    void refresh()
    const names = ['state://changed', ...extraEvents]
    const unlisteners = names.map((name) => listen(name, () => void refresh()))
    return () => {
      for (const unlisten of unlisteners) {
        void unlisten.then((dispose) => dispose())
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refresh])

  return { states, loaded, refresh }
}
