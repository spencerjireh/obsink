import init, * as core from '@obsink/core-wasm'
import wasmUrl from '@obsink/core-wasm/obsink_core_wasm_bg.wasm?url'
import { other } from './errors'

// The pure half of obsink-core, loaded once per worker. A failed load is not
// remembered: the next call tries again, and the user reads one sentence
// rather than the fetch or compile error.
let ready: Promise<typeof core> | null = null

export function wasm(): Promise<typeof core> {
  if (!ready) {
    ready = init({ module_or_path: wasmUrl })
      .then(() => core)
      .catch((error: unknown) => {
        ready = null
        const detail = error instanceof Error ? error.message : String(error)
        throw other(`Could not load the sync engine (${detail}). Reload the page.`)
      })
  }
  return ready
}

export type Core = typeof core
export type VaultKeys = core.VaultKeys
export type AccountKey = core.AccountKey
