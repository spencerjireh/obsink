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

// DESIGN.md §5: the notice after `Send sign-in code` on a server that cannot
// mail one (spec §4.1: `/auth/email/start` answers 503); the code field
// opens for the code the operator minted with `obsink-server code`.
export const NO_EMAIL_DELIVERY =
  'This server does not send email; ask the operator for a sign-in code.'

export function isNoEmailDelivery(error: CommandError): boolean {
  return error.kind === 'server' && error.status === 503
}
