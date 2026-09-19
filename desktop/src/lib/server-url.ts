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
