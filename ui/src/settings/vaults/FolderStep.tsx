import { useState } from 'react'
import { useBackend } from '../../backend'

type Props = {
  hint: string
  busy: boolean
  // The typed path (desktop) or the picked handle id (browser), and the
  // name to show for a pick.
  value: { local_path: string; folder_name: string }
  onChange: (next: { local_path: string; folder_name: string }) => void
  onError: (error: unknown) => void
}

// One folder question, shared by Create vault and Download: a picker where
// the platform picks folders, a path field where it types them.
export function FolderStep({ hint, busy, value, onChange, onError }: Props) {
  const backend = useBackend()
  const { folderPlaceholder } = backend.platform
  const [picking, setPicking] = useState(false)

  async function pickFolder() {
    if (!backend.pickFolder) return
    setPicking(true)
    try {
      const picked = await backend.pickFolder()
      onChange({ local_path: picked.id, folder_name: picked.name })
    } catch (error) {
      // Closing the picker is not an error.
      if ((error as DOMException)?.name !== 'AbortError') onError(error)
    } finally {
      setPicking(false)
    }
  }

  return (
    <>
      <div className="section__heading">
        <h2 id="step-heading">Folder</h2>
        <span className="section__hint">{hint}</span>
      </div>
      {backend.pickFolder ? (
        <div className="form-grid__wide">
          <span>Vault folder</span>
          <div className="choice-row">
            <button
              className="button button--ghost"
              autoFocus
              disabled={busy || picking}
              onClick={() => void pickFolder()}
              type="button"
            >
              {value.folder_name ? 'Choose another folder' : 'Choose folder'}
            </button>
            {value.folder_name ? <code>{value.folder_name}</code> : null}
          </div>
        </div>
      ) : (
        <label className="form-grid__wide">
          <span>Local vault path</span>
          <input
            data-testid="addVaultPathField"
            className="mono"
            autoCapitalize="off"
            autoCorrect="off"
            autoFocus
            spellCheck={false}
            placeholder={folderPlaceholder}
            value={value.local_path}
            onChange={(event) =>
              onChange({ local_path: event.target.value, folder_name: event.target.value })
            }
          />
        </label>
      )}
    </>
  )
}
