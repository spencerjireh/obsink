import { useCallback, useEffect, useRef, useState } from 'react'
import type {
  Conflict,
  ConflictPreview,
  Progress,
  ResolutionChoice,
  SyncResponse,
  SyncResult,
} from '../types'
import { useBackend } from '../backend'
import { plural } from '../lib/format'

type Notify = (message: string) => void

// One vault's sync cycle: run it, hold its conflicts until they are resolved,
// show progress and the last result. Mounted per vault (`key={vaultId}`) so
// switching vaults drops everything.
export function useSyncRunner(vaultId: string, onError: (error: unknown) => void, notify: Notify) {
  const backend = useBackend()
  const [busy, setBusy] = useState(false)
  const [progress, setProgress] = useState<Progress | null>(null)
  const [syncResult, setSyncResult] = useState<SyncResult | null>(null)
  const [conflicts, setConflicts] = useState<Conflict[]>([])
  const [choices, setChoices] = useState<Record<string, ResolutionChoice>>({})
  const [selectedPath, setSelectedPath] = useState<string | null>(null)
  const [preview, setPreview] = useState<ConflictPreview | null>(null)
  const [previewBusy, setPreviewBusy] = useState(false)
  // Mirrors `busy` for callers that only hold a ref (a second "Sync now"
  // while a cycle runs must not start another one).
  const busyRef = useRef(false)
  const onErrorRef = useRef(onError)
  onErrorRef.current = onError
  const notifyRef = useRef(notify)
  notifyRef.current = notify

  // Apply a sync/resolve response: late 409s arrive as `pending_conflicts`
  // next to a `completed_result`, so both paths share this.
  function applyResponse(response: SyncResponse, doneMessage: string) {
    setSyncResult(response.completed_result)
    setConflicts(response.pending_conflicts)
    setSelectedPath(response.pending_conflicts[0]?.path ?? null)
    setPreview(null)
    setChoices(
      Object.fromEntries(
        response.pending_conflicts.map((conflict) => [conflict.path, 'KeepLocal']),
      ),
    )
    if (response.pending_conflicts.length > 0) {
      const count = response.pending_conflicts.length
      notifyRef.current(
        count === 1 ? '1 conflict needs attention.' : `${count} conflicts need attention.`,
      )
      return
    }
    const failures = response.completed_result?.failures.length ?? 0
    notifyRef.current(
      failures === 0 ? doneMessage : `Sync finished with ${plural(failures, 'failure')}.`,
    )
  }

  async function run(action: () => Promise<SyncResponse>, doneMessage: string) {
    if (busyRef.current) return
    busyRef.current = true
    setBusy(true)
    setProgress(null)
    try {
      applyResponse(await action(), doneMessage)
    } catch (error) {
      onErrorRef.current(error)
    } finally {
      busyRef.current = false
      setBusy(false)
      setProgress(null)
    }
  }

  const sync = useCallback(
    () => run(() => backend.syncVault(vaultId), 'Sync complete.'),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [backend, vaultId],
  )

  const resolve = useCallback(
    () =>
      run(
        () =>
          backend.resolveConflict(
            vaultId,
            conflicts.map((conflict) => ({
              path: conflict.path,
              choice: choices[conflict.path] ?? 'KeepLocal',
            })),
          ),
        'Conflict resolutions applied.',
      ),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [backend, vaultId, conflicts, choices],
  )

  const choose = useCallback((path: string, choice: ResolutionChoice) => {
    setChoices((current) => ({ ...current, [path]: choice }))
  }, [])

  // Progress lines for this vault only.
  useEffect(() => {
    return backend.on('sync://progress', (envelope) => {
      if (envelope.vault_id !== vaultId) return
      const ev = envelope.event
      if ('Phase' in ev) {
        setProgress({ phase: ev.Phase, current: 0, total: 0, path: null })
      } else if ('FileStarted' in ev) {
        setProgress((prev) => ({
          phase: prev?.phase ?? 'Uploading',
          current: ev.FileStarted.index + 1,
          total: ev.FileStarted.total,
          path: ev.FileStarted.path,
        }))
      } else if ('Done' in ev) {
        setProgress(null)
      }
    })
  }, [backend, vaultId])

  // Keep a conflict selected while there are any.
  useEffect(() => {
    if (conflicts.length === 0) {
      setSelectedPath(null)
      setPreview(null)
      return
    }
    if (!selectedPath || !conflicts.some((conflict) => conflict.path === selectedPath)) {
      setSelectedPath(conflicts[0].path)
    }
  }, [conflicts, selectedPath])

  // Fetch the preview for the selected conflict.
  useEffect(() => {
    if (!selectedPath) {
      setPreview(null)
      return
    }
    let cancelled = false
    setPreviewBusy(true)
    backend
      .getConflictPreview(vaultId, selectedPath)
      .then((next) => {
        if (!cancelled) setPreview(next)
      })
      .catch((error) => {
        if (!cancelled) {
          onErrorRef.current(error)
          setPreview(null)
        }
      })
      .finally(() => {
        if (!cancelled) setPreviewBusy(false)
      })
    return () => {
      cancelled = true
    }
  }, [backend, vaultId, selectedPath])

  return {
    busy,
    progress,
    syncResult,
    conflicts,
    choices,
    selectedPath,
    preview,
    previewBusy,
    sync,
    resolve,
    choose,
    select: setSelectedPath,
  }
}
