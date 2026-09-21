import React from 'react'
import ReactDOM from 'react-dom/client'
import { BackendProvider, SettingsApp } from '@obsink/ui'
import '@obsink/ui/styles.css'
import { WebBackend } from './backend'
import { Unsupported } from './Unsupported'

// The browser client is the settings window; there is no popover.
const supported = 'showDirectoryPicker' in window && typeof Worker !== 'undefined'
document.documentElement.classList.add('window-settings')

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    {supported ? (
      <BackendProvider backend={new WebBackend()}>
        <SettingsApp />
      </BackendProvider>
    ) : (
      <Unsupported />
    )}
  </React.StrictMode>,
)
