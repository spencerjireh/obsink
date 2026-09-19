import { invoke } from '@tauri-apps/api/core'
import { toCommandError } from './errors'

export async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args)
  } catch (error) {
    throw toCommandError(error)
  }
}
