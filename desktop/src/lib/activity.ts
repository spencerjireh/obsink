import type { ActivityEvent } from '../types'

// The activity vocabulary (DESIGN.md §5): what happened, then the path.
export function activityLine(event: ActivityEvent): string {
  const path = event.path ?? ''
  switch (event.kind) {
    case 'uploaded':
      return `Uploaded ${path}`
    case 'downloaded':
      return `Downloaded ${path}`
    case 'deleted_here':
      return `Deleted here ${path}`
    case 'deleted_on_server':
      return `Deleted on server ${path}`
    case 'conflict':
      return `Conflict ${path}`
    case 'error':
      return path ? `Failed ${path}: ${event.detail ?? ''}` : `Error: ${event.detail ?? ''}`
    case 'synced':
      return `Synced · ${event.detail ?? ''}`
  }
}
