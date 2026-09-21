import type { BackendEvent, BackendEvents, CommandError } from '@obsink/ui'

// Main thread <-> worker. Every Backend call is one request with the method
// name and its arguments; the worker answers with the value or a
// `CommandError`, and pushes events on its own.

export type WorkerRequest = { id: number; method: string; args: unknown[] }

export type WorkerResponse =
  { id: number; ok: true; value: unknown } | { id: number; ok: false; error: CommandError }

export type WorkerEvent<E extends BackendEvent = BackendEvent> = {
  event: E
  payload: BackendEvents[E]
}

export type WorkerMessage = WorkerResponse | WorkerEvent

export function isResponse(message: WorkerMessage): message is WorkerResponse {
  return 'id' in message
}
