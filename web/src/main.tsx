import React, { type ReactNode } from 'react'
import ReactDOM from 'react-dom/client'
import '@obsink/ui/styles.css'
import { App, Boundary, Stopped } from './App'
import { WebBackend } from './backend'
import { Unsupported } from './Unsupported'

// Folder sync needs a secure context and the File System Access API; the
// page says which is missing rather than blaming the browser for both.
function start(): ReactNode {
  if (!window.isSecureContext) return <Unsupported reason="insecure" />
  if (!('showDirectoryPicker' in window) || typeof Worker === 'undefined') {
    return <Unsupported reason="browser" />
  }
  try {
    return <App backend={new WebBackend()} />
  } catch (error) {
    return <Stopped message={error instanceof Error ? error.message : String(error)} />
  }
}

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <Boundary>{start()}</Boundary>
  </React.StrictMode>,
)
