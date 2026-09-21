# DESIGN.md — ObSink

Rules for how ObSink looks, reads, and behaves on every client. `AGENTS.md`
owns engineering rules; this file owns the user interface. When the two
conflict, `AGENTS.md` wins. The desktop app and the iOS app share one brand,
one token set, and one vocabulary; the CLI shares the vocabulary.

## 1. Principles

Each principle follows from a hard rule in `AGENTS.md`.

1. **A utility, not a product page.** No hero, no tagline, no gradients, no
   decorative texture. The window shows the state of the vault and the one
   thing the user can do about it.
2. **Sync is the single primary action** (hard rule 2: sync is driven, not
   ambient). Every screen has at most one primary button, and on a vault
   screen it is `Sync now`; the desktop popover's `Sync now` runs it for every
   vault in turn. Automatic sync is visible only through the state text
   (`Syncing…`, `Last synced`); there is no toggle for it.
3. **Conflicts and stale state are first-class** (hard rule 3: never silently
   overwrite). Conflicts get their own section that is never hidden behind a
   disclosure; "changed on another device" is a warning notice, not a badge.
4. **Privacy is shown, not marketed.** The UI states facts that follow from
   hard rules 1, 4, and 5 where they matter: "Key saved on this device",
   "Passphrase does not match this vault", no recovery link. It does not
   advertise encryption.
5. **Deterministic over decorative.** Counts show zero rather than disappear;
   file paths are always monospace; the same event has the same wording on
   every platform.

## 2. Brand

- **Name:** `ObSink`. Capital O, capital S. Never "Obsink" or "OBSink".
- **Mark:** the closed loop. An amber sync ring, two arcs with arrowheads,
  around a paper disc with an ink keyhole, on an ink field. Two arcs stand for
  two devices; the keyhole stands for one key per vault.
- **Files:** `design/icon.svg` (full-bleed 1024 viewBox, source of truth) and
  `design/tray.svg` (monochrome silhouette for the macOS menu bar: heavier
  ring, solid disc, no keyhole, because 18 pt cannot show it). Every raster
  comes from `scripts/gen-icons.sh`; do not edit a PNG or `.icns` by hand.
- **Clear space:** at least one ring stroke width (60/1024 of the icon size)
  around the mark when it sits next to text.
- **Do:** use the mark at any size on ink or paper; use the tray silhouette
  as a template image so macOS tints it.
- **Do not:** recolour the ring, add a gradient or shadow, place text inside
  the ring, rotate the mark, or use the colour mark in the menu bar.

## 3. Tokens

Token names are CSS custom properties on desktop and asset-catalog or
semantic-colour names on iOS (section 6). Values are sRGB hex.

### 3.1 Colour

Brand constants (mode independent, used by the mark and the primary button):

| token | value |
|---|---|
| `--brand-ink` | `#15141B` |
| `--brand-amber` | `#E8A84A` |
| `--brand-paper` | `#F2EFE6` |

Semantic colours (switch with the system appearance):

| token | dark | light | role |
|---|---|---|---|
| `--color-bg` | `#15141B` | `#F2EFE6` | window background |
| `--color-surface` | `#1E1D26` | `#FFFFFF` | cards, lists, tab bar, inputs |
| `--color-border` | `#2C2B35` | `#DDD9CE` | hairlines |
| `--color-text` | `#F2EFE6` | `#15141B` | body text |
| `--color-text-muted` | `#A9A59B` | `#5F5B66` | labels, captions |
| `--color-accent` | `#E8A84A` | `#8A5A12` | tinted text, active borders, focus ring |
| `--color-warning` | `#F3C77A` | `#7A4F00` | stale banner, pending local changes, skipped files |
| `--color-danger` | `#FFB4AB` | `#B3261E` | fatal failures, sign-in errors |
| `--color-success` | `#9ED38B` | `#2E6B2F` | key saved, sync complete |

Contrast, measured against `--color-bg` and `--color-surface`:
`--color-text` 15.9:1 and 14.5:1 dark, 15.9:1 and 18.3:1 light;
`--color-text-muted` 7.4:1 and 6.8:1 dark, 5.8:1 and 6.6:1 light;
`--color-accent` 8.8:1 and 8.1:1 dark, 5.1:1 and 5.9:1 light. Warning,
danger, and success are at or above 5.6:1 in both modes. All text meets
WCAG AA (4.5:1).

The primary button is `--brand-amber` fill with `--brand-ink` text in both
modes (8.8:1). Raw amber as text on paper is 1.8:1, so light mode uses
`--color-accent` (deep amber) for tinted text and never `--brand-amber`.

Notices use the semantic colour at 12% alpha as the fill and at 40% alpha as
the border, with `--color-text` for the body and the semantic colour for the
leading tag.

### 3.2 Type

| token | value | use |
|---|---|---|
| `--font-serif` | `'Iowan Old Style', 'Palatino Linotype', 'Book Antiqua', Georgia, serif` | headings, body copy, buttons, vault names (desktop) |
| `--font-mono` | `ui-monospace, 'SF Mono', Menlo, monospace` | paths, vault ids, server URLs, counts, invite codes, timestamps |

Sizes: 12 (captions, tags), 14 (body, controls), 16 (section headings), 20
(counts), 22 (the active vault name; the largest text in the app). Line
height 1.4 for body, 1.2 for headings. No letter-spacing and no uppercase
except the `FATAL` / `skipped` tags.

iOS uses the system font with Dynamic Type (`.body`, `.caption`,
`.headline`) so text scales; `.monospaced()` for the data roles above.

### 3.3 Spacing, radius, motion

- Spacing scale `--space-1` to `--space-8`: 4, 8, 12, 16, 24, 32, 48, 64 px.
  Controls pad 8x12; cards pad 16; sections gap 24.
- Radii: `--radius-sm: 6px` for controls and inputs, `--radius-md: 10px` for
  cards and notices. No pill buttons.
- Motion: `--duration: 150ms` ease-out for hover and focus transitions only.
  Under `prefers-reduced-motion: reduce` the duration is 0. No entrance
  animations.
- Focus: a 2px `--color-accent` outline with 2px offset on every focusable
  element; never removed.

## 4. Components

| component | states | notes |
|---|---|---|
| Sync button | idle `Sync now`; busy `Working…` with a spinner; disabled | The only primary button on the vault screen. Disabled when no vault is active, a sync is running, the session has expired, or no key is available. |
| State dot | ok, pending, conflict, error, muted | An 8px circle in the semantic colour next to a vault name; the state text next to it says what it means (section 5). Never the only indicator. |
| Vault row (popover) | idle, hover, syncing | Dot, name, state text, and an icon-only `Open folder` button with an `aria-label`. The row opens the settings window at that vault. |
| Activity row | uploaded, downloaded, deleted here, deleted on server, conflict, error, synced | Relative time in muted text, the vault name when several vaults are shown, then the line in mono; errors in `--color-danger`, conflicts in `--color-warning`, `Synced` summaries muted. |
| Tab | selected, idle, hover | `role="tablist"` / `role="tab"` with `aria-selected`; a 2px accent underline marks the selected tab. |
| Stepper | done, current, todo | Numbered step chips; the current step has the accent border and `aria-current="step"`. Steps already passed are plain text; steps to come are muted. |
| Icon button | idle, hover | 28px square, no border, muted glyph that turns to `--color-text` on hover; always carries an `aria-label`. |
| Status counts | uploads, downloads, conflicts as three counts | iOS only. Always rendered; zero is shown as `0`. Count in `--font-mono` at 20px, label in muted text. |
| Notice | info, warning, danger | One line of text; a `FATAL` or `skipped` tag leads a failure line. Warning is used for the stale banner and pending local changes. |
| Vault row (list) | active, inactive, disabled while busy | State dot and name in the display face, state text in mono below. Active row has a 2px accent left border and `aria-current="page"`. |
| Conflict card | selected, unselected | Path in mono; two rows `This device` / `Other device` with size and modified time; a segmented choice of `Keep local`, `Keep remote`, `Keep both` (or `Delete on server` / `Delete here` when one side is deleted). |
| Preview | text, deleted, empty, loading | Two columns titled `This device` and `Other device`; body in mono, max height 280px, scrolls. |
| Form field | default, focused, invalid | Label above the input in muted text; URLs, ids, and codes in mono; passphrase is a secure field; the error sits below the field in `--color-danger`. |
| Empty state | — | One sentence in muted text that says what will appear and when. Never an illustration. |
| Device row | current, other | Device name, a `This device` tag on the current session, sign-in date in muted text, and a `Sign out` button. The current row signs this device out; any other row revokes that session on the server. |
| Invite row | active, used, expired | Code in mono (letter-spaced), a status tag (`Active` success, `Used` muted, `Expired` warning), expiry or use date in muted text; `Copy` on active rows only. |
| Confirmation | pending, ready, busy | Inline (desktop) or a sheet (iOS), never a native dialog. Title, one sentence of consequence, then `Type <value> to confirm.` with the value in mono; the destructive button is disabled until the typed value matches (the account email, or `delete` for an account without one, or the vault name). `Remove from this device` needs no typed value. |
| Danger button | idle, disabled | Ghost shape with `--color-danger` text and a translucent danger border; used for `Delete account` and `Delete vault on server`. |

## 5. Copy

- **Sentence case** everywhere: section headers, buttons, titles, menu items.
- No exclamation marks, no "successfully", no emoji.
- Numbers carry their unit and pluralise: `1 conflict`, `3 files`.
- Errors say what happened and what to do, in that order:
  `Passphrase does not match this vault.`
- Timestamps use the platform's short format; sizes use binary units
  (`1.2 MiB`).

Shared labels. Every platform uses exactly these strings:

| role | label |
|---|---|
| sections | `Vaults`, `Status`, `Account`, `Conflicts`, `Failed this sync`, `Last result` (iOS status line only), `Devices`, `Invites`, `Manage vault`, `Recent` (popover), `Activity` |
| tabs (desktop settings) | `Vaults`, `Account`, `Activity` |
| primary actions | `Sync now`, `Apply resolutions` |
| setup | `Add vault`, `Create vault`, `Connect vault`, `Load vaults` (iOS; desktop lists on entry), `Send sign-in code`, `Verify and sign in`, `Change email`, `Sign in` |
| add-vault steps | `Sign in`, `Choose vault`, `Folder`, `Passphrase`; navigation `Back`, `Next`, `Cancel`, `Done` |
| windows | `Settings`, `Open settings`, `Open folder`, `Quit ObSink` |
| vault state | `Up to date`, `n to upload`, `n to download`, `n to upload · n to download`, `n conflicts` (`1 conflict`), `Syncing…`, `Offline`, `Session expired`, `Error: <message>`, `On another server`, `Needs passphrase` |
| last synced | `Never synced`, `Just now`, `1 minute ago`, `n minutes ago`, `n hours ago`, `Yesterday`, `n days ago`, then the short date; `Last synced <relative>` on a vault page |
| popover global line | the worst state across vaults: `Offline`, `n conflicts`, `Syncing…`, `Error`, `Changes pending · synced <relative>`, `Up to date · synced <relative>`; `No vaults yet.` |
| activity lines | `Uploaded <path>`, `Downloaded <path>`, `Deleted here <path>`, `Deleted on server <path>`, `Conflict <path>`, `Failed <path>: <error>`, `Error: <error>`, `Synced · ↑n ↓n` |
| account | `Invite someone`, `Copy`, `Sign out` (per device), `Delete account` |
| vault actions | `Remove from this device`, `Delete vault on server` (distinct from the conflict choice `Delete on server`) |
| device tag | `This device` (the same string as the conflict side, on purpose) |
| invite status | `Active`, `Used`, `Expired` |
| per-vault usage | `412 MiB of 1 GiB`; `412 MiB` when the server sets no cap |
| session | `Session expired. Sign in again.` with the action `Sign in` |
| server without email | `This server has no email sign-in.` |
| after actions | `Invite code created.`, `Invite code copied.`, `Device signed out.`, `Account deleted.`, `Added <vault>.`, `Removed <vault> from this device.`, `Deleted <vault> on the server.` |
| foreign vault | `Vault is on another server. Remove it from this device.` (error); the page explains it is configured for another server and offers only `Remove from this device` |
| confirmation prompt | `Type <value> to confirm.` |
| confirmation copy | delete account: `This deletes your account, every vault it owns on <server>, and every signed-in device.` then, desktop: `Vault folders on this device stay.`, iOS: `The copies on this device are removed too.`; delete vault: `This deletes <vault> and all of its files on <server>` then, desktop: `The folder on this device stays.`, iOS: `for every device.`; remove: `The vault stays on the server.` then, desktop: `The key is removed from the keychain, so connecting again needs the passphrase.`, iOS: `The copy on this device, its Files location, and the key are removed, so connecting again needs the passphrase.` |
| modes | `Create`, `Connect` |
| conflict sides | `This device`, `Other device` |
| conflict choices | `Keep local`, `Keep remote`, `Keep both`, `Delete on server`, `Delete here` |
| busy | `Working…` |
| status line | `Synced · ↑n ↓n`; `n conflicts need attention`; `Error: …` |
| stale banner | `n files changed on another device. Sync before editing.` |
| pending local | `n local changes not uploaded yet` |
| key state | `Key saved on this device` |

Empty states:

- Vaults: `No vaults yet.` (desktop); `No vault yet. Tap Add vault.` (iOS)
- Last result / Recent / Activity: `Run a sync to see uploads and downloads.`
- Vault page with nothing pending: `Nothing to sync.`
- Conflicts: `Conflicts appear here when a sync needs a decision.`
- Result column: `No entries.`
- Preview: `Deleted in this version.` / `Empty file.`
- Devices: `No other devices signed in.` (iOS, where the current device is
  the picker's context); desktop always lists the current device.
- Invites: `No invites yet.`

The invite code field appears only when the server reports
`invite_required` (`GET /`) or has just refused a sign-up without one; in the
second case it takes focus. Sign-in methods the server does not offer are
not shown. The server itself is never shown as a field: each build talks to
one server (`OBSINK_SERVER_URL` at build time), and the UI only prints it
next to the account heading in mono.

## 6. Platform mapping

### Desktop (Tauri, React)

- Tokens live on `:root` in `desktop/src/styles.css`; light values are the
  default, dark values under `@media (prefers-color-scheme: dark)`.
  `color-scheme: light dark` so native controls follow.
- Posture: a menu-bar accessory app (`ActivationPolicy::Accessory`, no
  Dock icon). The server URL is baked in from `OBSINK_SERVER_URL`; an
  environment variable of the same name at launch overrides it for tests.
- Popover: a 360x480 borderless window (`decorations: false`, always on
  top, no transparency) shown centred under the tray icon on a left click
  and hidden when it loses focus. Header: the app icon as a 20px rounded
  tile (`BrandMark`), `ObSink`, a gear icon button. Then the global line,
  one vault row per configured vault (or an `Add vault` row), `Recent`
  (the newest eight activity events), and a footer with `Sync now`
  (primary) and `Settings` (ghost). It never shows a form; anything that
  needs input opens the settings window.
- Settings window: 900x620, minimum 760x520, hidden by default, closing
  hides it. A tab bar (`Vaults`, `Account`, `Activity`) over a pane. The
  Vaults tab is a 240px list (state dot, name, state text; `Add vault`
  below) and a page: name, state line with `Last synced <relative>` and the
  per-vault usage, the path with `Open folder`, `Sync now`, `Status`
  notices, `Conflicts`, `Manage vault`. `Add vault` replaces the page with
  the stepper flow and ends on a card with the folder path, `Open folder`
  and `Done`. The Account tab holds sign-in or the account, `Devices`,
  `Invites`, `Delete account`; the Activity tab lists events with a vault
  filter. Two-column groups stack below 860px; the list stacks above the
  page below 760px.
- Errors reach the UI typed (`CommandError { kind, message, status }`); a
  401 shows `Session expired` as the vault state and the session notice
  with `Sign in`, which opens the Account tab.
- Menu bar: the template silhouette; left click toggles the popover; menu
  items `Sync now`, `Open settings`, `Quit ObSink`.
- Icons: `desktop/src-tauri/icons/` holds the generated macOS set
  (`32x32.png`, `128x128.png`, `128x128@2x.png`, `icon.icns`, `icon.png`)
  and `tray.png`; `tauri.conf.json` `bundle.icon` lists them.
  `desktop/src/assets/icon.svg` is a copy of `design/icon.svg` (made by
  `scripts/gen-icons.sh`) that the popover header shows as a rounded tile.

### iOS (SwiftUI)

- Colours: system semantic colours (`.primary`, `.secondary`,
  `.systemGroupedBackground`) for surfaces and text; `.orange` for warning,
  `.red` for danger, `.green` for success. Asset catalog colorsets:
  `AccentColor` (`--color-accent`, light and dark variants) for tinted text
  and controls; `Amber` and `Ink` (brand constants) for the primary button.
- Structure: a `TabView`. **Home** is a scroll of cards on the grouped
  background, one per vault: name and `Manage` (the vault screen), a state
  dot with the shared state text (section 5) and `Last synced`, the
  per-vault usage, the last result line on the active vault, the stale
  banner, `Resolve n conflicts` (pushes the Conflicts screen), a
  `Passphrase` field until the key is stored, and a full-width
  `.borderedProminent` `Sync now` (amber with ink text). One sync runs at a
  time. After the first vault a one-time `Open in Obsidian` card explains
  the Files path (`Got it` dismisses it for good). `+` in the toolbar adds a
  vault. **Settings** holds the account: signed in as, usage, `Devices`,
  `Invites`, `Sign out`, `Delete account`, or `Sign in` when signed out
  (the session notice when it expired), plus the version.
- The server is baked in (`OBSINK_SERVER_URL` at build time, read from
  `Info.plist` `ObSinkServerURL`; `OBSINK_UITEST_SERVER_URL` overrides it
  for tests) and only printed under the account section. A vault entry on
  another server shows `On another server` and offers only `Remove from
  this device`.
- Add vault is a sheet with steps: `Sign in` (only when signed out; Sign
  in with Apple or an email code, the invite field only when needed),
  `Choose vault` (`Create` / `Connect`, the account's vaults listed on
  entry, `Next`), `Passphrase` (`Create vault` / `Connect vault`).
- `Devices`, `Invites`, `Manage vault` and `Conflicts` are pushed screens;
  typed confirmations are a sheet with a medium detent
  (`TypedConfirmationSheet`); `Remove from this device` is a confirmation
  dialog. Errors reach Swift typed (`MobileError`) and
  `MobileErrorPresentation.swift` maps each variant to its copy.
- The vault cache on iOS lives inside the app group, so `Remove from this
  device` and `Delete account` remove it (the copy says so); on desktop the
  vault folder is the user's and stays.
- Icon: `ios/ObSink/Assets.xcassets/AppIcon.appiconset/AppIcon.png`, a
  1024x1024 opaque RGB PNG (App Store Connect rejects alpha), full bleed;
  the OS applies the mask.

### CLI

Uses the shared vocabulary in messages and prompts: `obsink sync` is the
one action; the interactive conflict prompt offers `keep local`, `keep
remote`, `keep both` in that order.

## 7. Accessibility

- Both appearances are supported on every client; every text/background
  pair meets 4.5:1 (section 3.1).
- `prefers-reduced-motion` disables transitions; there is nothing that
  moves on its own.
- Desktop: every control is a real `<button>`, `<input>`, `<select>`, or
  `<a>`; the focus outline is never removed; icon-only controls carry an
  `aria-label`.
- iOS: touch targets are at least 44pt; Dynamic Type is respected.
- **XCUITest identifiers and matched labels are API.** The simulator
  harness (`ios/UITests/SyncE2ETests.swift`,
  `scripts/verify-ios-sim-e2e.sh`) finds elements by these identifiers:
  `syncButton`, `passphraseField`, `statusText`, `addVaultButton`,
  `addVaultAccountText`, `addVaultNextButton`, `listVaultsButton`,
  `vaultPicker`, `addVaultStatusText`, `addVaultPassphraseField`,
  `addVaultSubmitButton`, `staleBanner`, `conflictRowTitle`,
  `winnerPicker`, `applyResolutionsButton`, `vaultUsageText`,
  `manageVaultButton`, `removeVaultButton`, `deleteVaultButton`,
  `devicesLink`, `deviceRow`, `deviceSignOutButton`, `invitesLink`,
  `inviteRow`, `deleteAccountButton`, `signInButton`, `sessionExpiredText`,
  `confirmationField`, `confirmDestructiveButton`, `confirmCancelButton`,
  `addVaultDoneButton`, `vaultCard`, `vaultStateText`, `lastSyncedText`,
  `resolveConflictsLink`, `guidanceCard`, `guidanceDismissButton`,
  `accountNoticeText`, `appVersionText`, `backgroundRefreshText`; and by these labels: the `Connect` segment, the
  `Keep local` / `Keep remote` / `Keep both` segments, the
  `Remove from this device` confirmation button, the tab bar buttons
  `Home` / `Settings`, `Next`, `Got it`, the status prefixes `Synced ·`,
  `Added vault`, `1 conflict`, the empty state `No vault yet. Tap Add
  vault.`, and the banner text `changed on another device`. Renaming any of them means updating the
  tests and the harness in the same change.
