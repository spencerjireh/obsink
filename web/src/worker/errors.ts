import type { CommandError } from '@obsink/ui'

// The worker throws plain `CommandError` objects (they cross postMessage as
// data); these mirror desktop's `CommandError` constructors.

export function other(message: string): CommandError {
  return { kind: 'other', message }
}

export function network(message: string): CommandError {
  return { kind: 'network', message }
}

export function fromStatus(status: number, message: string): CommandError {
  if (status === 401) {
    return { kind: 'unauthorized', message: 'Session expired. Sign in again.', status }
  }
  return { kind: 'server', message, status }
}

// The one message for a passphrase that does not decrypt (DESIGN.md §5).
export function wrongPassphrase(): CommandError {
  return other('Passphrase does not match this account.')
}

export function isCommandError(error: unknown): error is CommandError {
  return (
    typeof error === 'object' &&
    error !== null &&
    typeof (error as { kind?: unknown }).kind === 'string' &&
    typeof (error as { message?: unknown }).message === 'string'
  )
}

export function asCommandError(error: unknown): CommandError {
  if (isCommandError(error)) return error
  if (error instanceof Error) return other(error.message)
  return other(String(error))
}
