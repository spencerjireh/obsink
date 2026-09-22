import type { AccountState, AuthCapabilities, InviteInfo } from '@obsink/ui'
import type { WireInvite } from './api'
import { all, type StoredVault } from '../shared/db'
import { stopDriver } from './driver'
import { forgetKeys } from './keys'
import {
  api,
  bearerCall,
  deviceName,
  forgetBearer,
  loadBearer,
  saveBearer,
  serverUrl,
} from './session'
import { forgetVaultsForServer } from './vaults'

// The account commands, one per desktop `#[tauri::command]`.

export async function getAuthCapabilities(): Promise<AuthCapabilities> {
  const caps = await api.capabilities()
  return {
    email: caps.auth.email,
    apple: caps.auth.apple,
    invite_required: caps.invite_required ?? false,
  }
}

// Returns the code itself only against a dev server (`AUTH_DEV_RETURN_CODE=1`).
export async function authEmailStart(email: string): Promise<string | null> {
  const result = await api.emailStart(email.trim())
  return result.code ?? null
}

export async function authEmailVerify(
  email: string,
  code: string,
  inviteCode: string | null,
): Promise<AccountState> {
  const invite = inviteCode?.trim() || null
  const session = await api.emailVerify(email.trim(), code.trim(), deviceName(), invite)
  await saveBearer(session.token)
  return getAccount()
}

function inviteInfo(invite: WireInvite): InviteInfo {
  const status = invite.status === 'used' || invite.status === 'expired' ? invite.status : 'active'
  return {
    code: invite.code,
    created: invite.created ?? 0,
    expires: invite.expires,
    status,
    used_at: invite.used_at ?? null,
  }
}

export async function getAccount(): Promise<AccountState> {
  const bearer = await loadBearer()
  if (!bearer) return { kind: 'signed_out' }
  try {
    const me = await api.me(bearer)
    // The operator bearer has no account behind it; only accounts work here.
    if (!me.user) return { kind: 'signed_out' }
    return {
      kind: 'account',
      user_id: me.user.id,
      email: me.user.email,
      devices: (me.sessions ?? []).map((session) => ({
        session_id: session.id,
        device_name: session.deviceName,
        created: session.created,
        current: session.current,
      })),
      usage: me.usage
        ? {
            total_bytes: me.usage.total_bytes,
            max_vault_bytes: me.usage.max_vault_bytes,
            max_vaults: me.usage.max_vaults,
            vaults: (me.usage.vaults ?? []).map((vault) => ({ id: vault.id, bytes: vault.bytes })),
          }
        : null,
    }
  } catch (error) {
    if ((error as { kind?: string })?.kind === 'unauthorized') {
      // Session revoked/expired elsewhere: signed out is the state.
      await forgetBearer()
      return { kind: 'signed_out' }
    }
    throw error
  }
}

export function createInvite(): Promise<InviteInfo> {
  return bearerCall(async (bearer) => inviteInfo(await api.createInvite(bearer)))
}

export function listInvites(): Promise<InviteInfo[]> {
  return bearerCall(async (bearer) => (await api.listInvites(bearer)).map(inviteInfo))
}

export async function revokeSession(sessionId: string): Promise<AccountState> {
  await bearerCall((bearer) => api.revokeSession(bearer, sessionId))
  return getAccount()
}

export async function signOut(): Promise<void> {
  const bearer = await loadBearer()
  if (bearer) {
    try {
      await api.logout(bearer)
    } catch {
      // Best effort: the bearer is forgotten either way.
    }
  }
  await forgetBearer()
  // Nothing keeps polling, and no key stays in memory, for an account that
  // is signed out; the vault entries stay so signing in again resumes.
  for (const vault of await all<StoredVault>('vaults')) {
    stopDriver(vault.id)
    forgetKeys(vault.id)
  }
}

export async function deleteAccount(): Promise<void> {
  await bearerCall((bearer) => api.deleteAccount(bearer))
  await forgetBearer()
  await forgetVaultsForServer(serverUrl)
}
