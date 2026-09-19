import type { CommandError } from '../types'

// Shown wherever a bearer was rejected; the action next to it opens sign-in.
export const SESSION_EXPIRED = 'Session expired. Sign in again.'

// Tauri rejects with the serialised `CommandError` for command failures and
// with a plain string for its own plumbing errors; callers see one shape.
export function toCommandError(error: unknown): CommandError {
  if (typeof error === 'object' && error !== null && 'kind' in error && 'message' in error) {
    const candidate = error as { kind: unknown; message: unknown; status?: unknown }
    if (typeof candidate.kind === 'string' && typeof candidate.message === 'string') {
      return {
        kind: candidate.kind as CommandError['kind'],
        message: candidate.message,
        status: typeof candidate.status === 'number' ? candidate.status : undefined,
      }
    }
  }
  return { kind: 'other', message: String(error) }
}

export function isUnauthorized(error: CommandError): boolean {
  return error.kind === 'unauthorized'
}

// The server has no machine-readable code for this 403; its two invite
// messages both contain the word.
export function isInviteRequired(error: CommandError): boolean {
  return error.kind === 'server' && error.status === 403 && /invite/i.test(error.message)
}
