import { Component, useEffect, useState, type ReactNode } from 'react'
import { BackendProvider, SettingsApp } from '@obsink/ui'
import type { WebBackend } from './backend'

// The browser client is the settings window; there is no popover. Around it:
// a boundary so a render error is a sentence and a Reload button rather than
// a blank page, a banner for errors the worker reports on its own, and the
// visibility wiring that pauses polling while the tab is hidden or offline.

export class Boundary extends Component<{ children: ReactNode }, { error: string | null }> {
  state = { error: null as string | null }

  static getDerivedStateFromError(error: unknown) {
    return { error: error instanceof Error ? error.message : String(error) }
  }

  render() {
    if (this.state.error) return <Stopped message={this.state.error} />
    return this.props.children
  }
}

export function Stopped({ message }: { message: string }) {
  return (
    <main className="main">
      <div className="vault-page">
        <header className="pane-header">
          <div className="pane-header__title">
            <h1>ObSink stopped</h1>
            <p className="pane-header__meta">{message}</p>
          </div>
        </header>
        <div className="choice-row">
          <button
            className="button button--primary"
            onClick={() => location.reload()}
            type="button"
          >
            Reload
          </button>
        </div>
      </div>
    </main>
  )
}

export function App({ backend }: { backend: WebBackend }) {
  const [problem, setProblem] = useState<string | null>(null)

  useEffect(() => backend.on('client://error', ({ message }) => setProblem(message)), [backend])

  useEffect(() => {
    const report = () => {
      const hidden = document.visibilityState === 'hidden' || !navigator.onLine
      void backend.setVisibility(hidden).catch(() => undefined)
    }
    document.addEventListener('visibilitychange', report)
    window.addEventListener('online', report)
    window.addEventListener('offline', report)
    report()
    return () => {
      document.removeEventListener('visibilitychange', report)
      window.removeEventListener('online', report)
      window.removeEventListener('offline', report)
    }
  }, [backend])

  return (
    <BackendProvider backend={backend}>
      {problem ? (
        <div className="notice notice--danger notice--actionable" role="alert">
          <span>{problem}</span>
          <button className="button button--ghost" onClick={() => location.reload()} type="button">
            Reload
          </button>
        </div>
      ) : null}
      <SettingsApp />
    </BackendProvider>
  )
}
