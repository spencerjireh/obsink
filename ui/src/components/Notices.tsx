import type { ReactNode } from 'react'
import type { SyncFailure } from '../types'
import { plural } from '../lib/format'

type NoticeKind = 'info' | 'warning' | 'danger'

export function Notice({
  kind = 'info',
  action,
  testId = 'statusText',
  children,
}: {
  kind?: NoticeKind
  // The data-testid the harnesses read this notice by (DESIGN.md lists them).
  testId?: string
  // One button the notice offers next to its text (for example `Sign in`).
  action?: ReactNode
  children: ReactNode
}) {
  return (
    <div
      className={`notice notice--${kind}${action ? ' notice--actionable' : ''}`}
      role={kind === 'info' ? 'status' : 'alert'}
      data-testid={testId}
      data-kind={kind}
    >
      {action ? <span>{children}</span> : children}
      {action ? <span className="notice__action">{action}</span> : null}
    </div>
  )
}

export function FailureNotice({ failures }: { failures: SyncFailure[] }) {
  const fatal = failures.some((failure) => failure.fatal)
  return (
    <Notice kind={fatal ? 'danger' : 'warning'}>
      <strong>Failed this sync</strong> · {plural(failures.length, 'file')}
      <ul className="failure-list">
        {failures.map((failure) => (
          <li key={failure.path}>
            <span className={`tag tag--${failure.fatal ? 'fatal' : 'skipped'}`}>
              {failure.fatal ? 'FATAL' : 'skipped'}
            </span>{' '}
            <code>{failure.path}</code>: {failure.error}
          </li>
        ))}
      </ul>
    </Notice>
  )
}
