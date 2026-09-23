import type { DevicePlatform } from '@obsink/ui'
import { bearerKey, del, get, KV_DEVICE_ID, put } from '../shared/db'
import { Api, type WireDeviceBody } from './api'
import { fromStatus } from './errors'

// The one server this page talks to: its own origin (the web container
// proxies the API paths).
export const serverUrl: string = self.location.origin

export const api = new Api(serverUrl)

export async function loadBearer(): Promise<string | null> {
  return (await get<string>('kv', bearerKey(serverUrl))) ?? null
}

export async function requireBearer(): Promise<string> {
  const bearer = await loadBearer()
  // No bearer reads as an expired session, as desktop's 401 does.
  if (!bearer) throw fromStatus(401, 'not signed in')
  return bearer
}

export function saveBearer(token: string): Promise<void> {
  return put('kv', bearerKey(serverUrl), token)
}

export function forgetBearer(): Promise<void> {
  return del('kv', bearerKey(serverUrl))
}

// A 401 means the session is gone (revoked, expired, account deleted): the
// stored bearer is useless, so forget it and let the UI show signed-out.
export async function bearerCall<T>(action: (bearer: string) => Promise<T>): Promise<T> {
  const bearer = await requireBearer()
  try {
    return await action(bearer)
  } catch (error) {
    if ((error as { kind?: string })?.kind === 'unauthorized') await forgetBearer()
    throw error
  }
}

export function deviceName(): string {
  const nav = self.navigator as { userAgentData?: { platform?: string }; platform?: string }
  const platform = nav.userAgentData?.platform || nav.platform || 'Browser'
  return `${platform} (ObSink Web)`
}

// The device id this profile keeps for good (spec §4.1): read from the `kv`
// store, minted on first use. `dev_<32 hex>` fits the server's
// `[A-Za-z0-9_-]{1,64}` and matches what the CLI mints.
export async function loadOrCreateDeviceId(): Promise<string> {
  const stored = await get<string>('kv', KV_DEVICE_ID)
  if (stored) return stored
  const bytes = new Uint8Array(16)
  crypto.getRandomValues(bytes)
  const id = `dev_${Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')}`
  await put('kv', KV_DEVICE_ID, id)
  return id
}

// This browser as the server knows it at sign-in.
export async function thisDevice(): Promise<WireDeviceBody> {
  const platform: DevicePlatform = 'browser'
  return { id: await loadOrCreateDeviceId(), name: deviceName(), platform }
}
