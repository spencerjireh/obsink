import { bearerKey, del, get, put } from '../shared/db'
import { Api } from './api'
import { fromStatus } from './errors'

// The one server this page talks to: its own origin (the web container
// proxies the API paths). Stored on vault entries like desktop stores the
// baked URL, so an entry from another origin shows as foreign.
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
