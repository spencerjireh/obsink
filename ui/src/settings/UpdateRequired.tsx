import type { ProtocolInfo } from '../types'

// Spec §15.5: a client speaks exactly one wire format. When the server's
// differs, this page is all the app shows.
export function UpdateRequired({ protocol, siteUrl }: { protocol: ProtocolInfo; siteUrl: string }) {
  return (
    <div className="vault-page">
      <header className="pane-header">
        <div className="pane-header__title">
          <h1>Update ObSink</h1>
          <p className="pane-header__meta" data-testid="updateRequiredText">
            This app is too old for the server. Download the current version.
          </p>
        </div>
      </header>
      <div className="card">
        <p>
          The server speaks protocol <code>{protocol.server ?? '?'}</code>; this build speaks{' '}
          <code>{protocol.client}</code>.
        </p>
        <p>
          <a href={siteUrl} target="_blank" rel="noreferrer">
            {siteUrl}
          </a>
        </p>
      </div>
    </div>
  )
}
