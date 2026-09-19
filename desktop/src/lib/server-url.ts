const SERVER_URL_KEY = 'obsink.serverUrl'

export const DEFAULT_SERVER_URL = 'https://'

export function rememberedServerUrl(): string {
  try {
    return window.localStorage.getItem(SERVER_URL_KEY) ?? DEFAULT_SERVER_URL
  } catch {
    return DEFAULT_SERVER_URL
  }
}

export function rememberServerUrl(url: string) {
  try {
    window.localStorage.setItem(SERVER_URL_KEY, url)
  } catch {
    // Per-machine convenience only.
  }
}

// Mirrors core's `normalize_server_url` so the setup URL can be compared
// with the URLs stored on vault entries (which are normalised on load).
export function normalizeServerUrl(url: string): string {
  const trimmed = url.trim().replace(/\/+$/, '')
  const at = trimmed.indexOf('://')
  if (at === -1) return trimmed
  const scheme = trimmed.slice(0, at).toLowerCase()
  const rest = trimmed.slice(at + 3)
  const slash = rest.indexOf('/')
  const host = (slash === -1 ? rest : rest.slice(0, slash)).toLowerCase()
  const path = slash === -1 ? '' : rest.slice(slash)
  return `${scheme}://${host}${path}`
}
