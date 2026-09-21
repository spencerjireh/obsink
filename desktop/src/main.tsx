import React from 'react'
import ReactDOM from 'react-dom/client'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { BackendProvider, PopoverApp, SettingsApp } from '@obsink/ui'
import '@obsink/ui/styles.css'
import { tauriBackend } from './backend'

// One bundle, two windows: the label decides what renders. `?view=` is for
// a plain browser during development, where there is no Tauri window.
function windowLabel(): string {
  const fromQuery = new URLSearchParams(window.location.search).get('view')
  if (fromQuery) return fromQuery
  try {
    return getCurrentWindow().label
  } catch {
    return 'settings'
  }
}

const label = windowLabel()
document.documentElement.classList.add(`window-${label}`)

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <BackendProvider backend={tauriBackend}>
      {label === 'popover' ? <PopoverApp /> : <SettingsApp />}
    </BackendProvider>
  </React.StrictMode>,
)
