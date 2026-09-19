import type { AddVaultForm, Conflict, ResolutionChoice } from '../types'

// "Keep both" needs two live versions; when one side is a deletion the only
// question is which side wins, and the labels say what that means.
export function availableChoices(
  conflict: Conflict,
): { choice: ResolutionChoice; label: string }[] {
  const options: { choice: ResolutionChoice; label: string }[] = [
    { choice: 'KeepLocal', label: conflict.local.deleted ? 'Delete on server' : 'Keep local' },
    { choice: 'KeepRemote', label: conflict.remote.deleted ? 'Delete here' : 'Keep remote' },
  ]
  if (!conflict.local.deleted && !conflict.remote.deleted) {
    options.push({ choice: 'KeepBoth', label: 'Keep both' })
  }
  return options
}

export const emptyForm: AddVaultForm = {
  mode: 'connect',
  local_path: '',
  vault_name: '',
  vault_id: '',
  passphrase: '',
}
