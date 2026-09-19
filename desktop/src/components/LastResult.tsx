import type { SyncAction, SyncResult } from '../types'
import { EmptyState } from './EmptyState'

export function LastResult({ result }: { result: SyncResult | null }) {
  return (
    <section className="section" aria-labelledby="last-result-heading">
      <div className="section__heading">
        <h2 id="last-result-heading">Last result</h2>
      </div>
      {result ? (
        <div className="result-columns">
          <ResultColumn title="Uploaded" items={result.upload} />
          <ResultColumn title="Downloaded" items={result.download} />
        </div>
      ) : (
        <EmptyState>Run a sync to see uploads and downloads.</EmptyState>
      )}
    </section>
  )
}

function ResultColumn({ title, items }: { title: string; items: SyncAction[] }) {
  return (
    <div className="result-column">
      <h3>{title}</h3>
      {items.length === 0 ? <EmptyState>No entries.</EmptyState> : null}
      {items.map((item) => (
        <div key={`${title}-${item.path}-${item.kind}`} className="result-row">
          <code className="result-row__path">{item.path}</code>
          <span className="tag">{item.kind}</span>
        </div>
      ))}
    </div>
  )
}
