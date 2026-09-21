import type { BackendEvent, BackendEvents } from '@obsink/ui'
import type { WorkerEvent } from '../shared/protocol'

// Events the worker pushes to the page (desktop's `app.emit`).
export function emit<E extends BackendEvent>(event: E, payload: BackendEvents[E]): void {
  const message: WorkerEvent<E> = { event, payload }
  self.postMessage(message)
}

export function stateChanged(vaultId: string | null): void {
  emit('state://changed', { vault_id: vaultId })
}
