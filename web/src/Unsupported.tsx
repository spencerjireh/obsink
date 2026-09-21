// Folder sync needs the File System Access API (Chrome, Edge); elsewhere
// the page says so and points at the downloads.
export function Unsupported() {
  return (
    <main className="main">
      <div className="vault-page">
        <header className="pane-header">
          <div className="pane-header__title">
            <h1>ObSink in the browser</h1>
            <p className="pane-header__meta">
              Syncing a folder from the browser needs the File System Access API, which Chrome and
              Edge provide. In other browsers, use the desktop app or the command line.
            </p>
          </div>
        </header>
        <div className="choice-row">
          <a className="button button--primary" href="/">
            Downloads
          </a>
        </div>
      </div>
    </main>
  )
}
