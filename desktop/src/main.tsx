import React from 'react'
import ReactDOM from 'react-dom/client'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { PopoverApp } from './popover/PopoverApp'
import { SettingsApp } from './settings/SettingsApp'
import './styles.css'

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
  <React.StrictMode>{label === 'popover' ? <PopoverApp /> : <SettingsApp />}</React.StrictMode>,
)
