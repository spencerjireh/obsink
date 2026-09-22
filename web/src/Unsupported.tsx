// Folder sync needs the File System Access API, which browsers expose only
// on Chromium and only over HTTPS; elsewhere the page says which of the two
// is missing and points at the other ways to run ObSink.
export function Unsupported({ reason }: { reason: 'browser' | 'insecure' }) {
  const why =
    reason === 'insecure'
      ? 'This page is not served over HTTPS, so the browser withholds the File System Access API that reads the vault folder. Open it over HTTPS.'
      : 'Syncing a folder from the browser needs the File System Access API, which Chrome, Edge and other Chromium browsers provide. In other browsers, use the desktop app, the command line or the iOS app.'
  return (
    <main className="main">
      <div className="vault-page">
        <header className="pane-header">
          <div className="pane-header__title">
            <h1>ObSink in the browser</h1>
            <p className="pane-header__meta">{why}</p>
          </div>
        </header>
        <div className="choice-row">
          <a className="button button--primary" href="/#mac">
            See the downloads
          </a>
        </div>
      </div>
    </main>
  )
}
