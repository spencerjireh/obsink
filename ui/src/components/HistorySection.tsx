import { useEffect, useState } from 'react'
import type { FilePreview, TrashEntry, VersionInfo } from '../types'
import { useBackend } from '../backend'
import { formatBytes, formatRelative } from '../lib/format'
import { EmptyState } from './EmptyState'

type Props = {
  vaultId: string
  busy: boolean
  notify: (message: string) => void
  onError: (error: unknown) => void
}

// Which row's preview is open: a version of the picked file, or a tombstone.
type PreviewTarget = { kind: 'version'; name: string } | { kind: 'trash'; path: string }

// Spec §15.2 `History`: `File history` (pick a file, its versions, preview,
// restore) and `Recently deleted` (tombstones with real paths, preview,
// restore). A restore writes into the folder; the next sync uploads it.
export function HistorySection({ vaultId, busy, notify, onError }: Props) {
  const backend = useBackend()
  const [files, setFiles] = useState<string[]>([])
  const [file, setFile] = useState('')
  const [versions, setVersions] = useState<VersionInfo[] | null>(null)
  const [trash, setTrash] = useState<TrashEntry[] | null>(null)
  const [preview, setPreview] = useState<{ target: PreviewTarget; content: FilePreview } | null>(
    null,
  )
  const [working, setWorking] = useState(false)

  useEffect(() => {
    let cancelled = false
    void backend
      .listFiles(vaultId)
      .then((next) => {
        if (!cancelled) setFiles(next)
      })
      .catch(onError)
    void backend
      .listTrash(vaultId)
      .then((next) => {
        if (!cancelled) setTrash(next)
      })
      .catch(onError)
    return () => {
      cancelled = true
    }
    // The vault is fixed for this mount (`key={info.id}` above).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [backend, vaultId])

  useEffect(() => {
    if (!file) {
      setVersions(null)
      return
    }
    let cancelled = false
    setVersions(null)
    void backend
      .listVersions(vaultId, file)
      .then((next) => {
        if (!cancelled) setVersions(next)
      })
      .catch(onError)
    return () => {
      cancelled = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [backend, vaultId, file])

  async function run(action: () => Promise<void>) {
    setWorking(true)
    try {
      await action()
    } catch (error) {
      onError(error)
    } finally {
      setWorking(false)
    }
  }

  const same = (a: PreviewTarget, b: PreviewTarget) =>
    a.kind === b.kind &&
    (a.kind === 'version' ? a.name === (b as typeof a).name : a.path === (b as typeof a).path)

  function togglePreview(target: PreviewTarget, load: () => Promise<FilePreview>) {
    if (preview && same(preview.target, target)) {
      setPreview(null)
      return
    }
    void run(async () => setPreview({ target, content: await load() }))
  }

  function restore(path: string, action: () => Promise<void>) {
    void run(async () => {
      await action()
      notify(`Restored ${path}. Sync to upload it.`)
    })
  }

  const disabled = busy || working

  function previewBlock(target: PreviewTarget) {
    if (!preview || !same(preview.target, target)) return null
    return (
      <div className="preview">
        <div className="preview__header">
          <span>Read-only preview</span>
          <span className="mono">{formatBytes(preview.content.size)}</span>
        </div>
        {preview.content.text === null ? (
          <p className="empty-state">No preview for this file.</p>
        ) : preview.content.text === '' ? (
          <p className="empty-state">Empty file.</p>
        ) : (
          <pre>{preview.content.text}</pre>
        )}
      </div>
    )
  }

  return (
    <section className="section" aria-labelledby="history-heading">
      <div className="section__heading">
        <h2 id="history-heading">History</h2>
      </div>

      <div className="subsection" aria-labelledby="file-history-heading">
        <h3 id="file-history-heading">File history</h3>
        <label className="inline-field">
          <span>File</span>
          <select
            data-testid="historyFilePicker"
            disabled={disabled}
            value={file}
            onChange={(event) => {
              setFile(event.target.value)
              setPreview(null)
            }}
          >
            <option value="">Pick a file</option>
            {files.map((path) => (
              <option key={path} value={path}>
                {path}
              </option>
            ))}
          </select>
        </label>
        {!file ? (
          <EmptyState>Pick a file to see its versions.</EmptyState>
        ) : versions === null ? null : versions.length === 0 ? (
          <EmptyState>No earlier versions kept.</EmptyState>
        ) : (
          <ul className="row-list">
            {versions.map((version) => {
              const target: PreviewTarget = { kind: 'version', name: version.name }
              return (
                <li className="row-item row-item--stacked" key={version.name}>
                  <div
                    className="row-item__line row-item__row"
                    data-testid="historyRow"
                    data-ts={version.ts}
                  >
                    <span className="row-item__main">
                      <span>{formatRelative(version.ts)}</span>
                      <span className="row-item__meta">· {formatBytes(version.size)}</span>
                    </span>
                    <span className="choice-row">
                      <button
                        className="button button--ghost button--small"
                        data-testid="previewButton"
                        disabled={disabled}
                        onClick={() =>
                          togglePreview(target, () =>
                            backend.previewVersion(vaultId, file, version.name),
                          )
                        }
                        type="button"
                      >
                        Preview
                      </button>
                      <button
                        className="button button--ghost button--small"
                        data-testid="restoreButton"
                        disabled={disabled}
                        onClick={() =>
                          restore(file, () => backend.restoreVersion(vaultId, file, version.name))
                        }
                        type="button"
                      >
                        Restore
                      </button>
                    </span>
                  </div>
                  {previewBlock(target)}
                </li>
              )
            })}
          </ul>
        )}
      </div>

      <div className="subsection" aria-labelledby="trash-heading">
        <h3 id="trash-heading">Recently deleted</h3>
        {trash === null ? null : trash.length === 0 ? (
          <EmptyState>Nothing deleted in the last 30 days.</EmptyState>
        ) : (
          <ul className="row-list">
            {trash.map((entry) => {
              const target: PreviewTarget = { kind: 'trash', path: entry.path }
              return (
                <li className="row-item row-item--stacked" key={entry.path}>
                  <div
                    className="row-item__line row-item__row"
                    data-testid="trashRow"
                    data-path={entry.path}
                  >
                    <span className="row-item__main">
                      <span className="mono">{entry.path}</span>
                      <span className="row-item__meta">
                        · deleted {formatRelative(entry.deleted_at).toLowerCase()}
                      </span>
                    </span>
                    <span className="choice-row">
                      <button
                        className="button button--ghost button--small"
                        data-testid="previewButton"
                        disabled={disabled}
                        onClick={() =>
                          togglePreview(target, () => backend.previewTrash(vaultId, entry.path))
                        }
                        type="button"
                      >
                        Preview
                      </button>
                      <button
                        className="button button--ghost button--small"
                        data-testid="restoreButton"
                        disabled={disabled}
                        onClick={() =>
                          restore(entry.path, () => backend.restoreTrash(vaultId, entry.path))
                        }
                        type="button"
                      >
                        Restore
                      </button>
                    </span>
                  </div>
                  {previewBlock(target)}
                </li>
              )
            })}
          </ul>
        )}
      </div>
    </section>
  )
}
