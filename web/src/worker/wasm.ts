import init, * as core from '@obsink/core-wasm'
import wasmUrl from '@obsink/core-wasm/obsink_core_wasm_bg.wasm?url'

// The pure half of obsink-core, loaded once per worker.
let ready: Promise<typeof core> | null = null

export function wasm(): Promise<typeof core> {
  if (!ready) {
    ready = init({ module_or_path: wasmUrl }).then(() => core)
  }
  return ready
}

export type Core = typeof core
export type VaultKeys = core.VaultKeys
