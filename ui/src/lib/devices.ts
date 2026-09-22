import type { DevicePlatform } from '../types'

// The platform nouns of DESIGN.md §5.
export function platformLabel(platform: DevicePlatform): string {
  switch (platform) {
    case 'macos':
      return 'Mac'
    case 'ios':
      return 'iPhone / iPad'
    case 'browser':
      return 'Browser'
    case 'cli':
      return 'CLI'
    case 'unknown':
      return 'Device'
  }
}
