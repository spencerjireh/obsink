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
2. **Sync is the single primary action** (hard rule 2: manual sync only).
   Every screen has at most one primary button, and on the vault screen it is
   `Sync now`. Nothing looks like a toggle for automatic or background sync.
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
| `--color-surface` | `#1E1D26` | `#FFFFFF` | cards, sidebar, inputs |
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
| Sync button | idle `Sync now`; busy `Working…` with a spinner; disabled | The only primary button on the vault screen. Disabled when no vault is active, a sync is running, or no key is available. |
| Status counts | uploads, downloads, conflicts as three counts | Always rendered; zero is shown as `0`. Count in `--font-mono` at 20px, label in muted text. |
| Notice | info, warning, danger | One line of text; a `FATAL` or `skipped` tag leads a failure line. Warning is used for the stale banner and pending local changes. |
| Vault row | active, inactive, disabled while busy | Name in the display face, local path in mono with ellipsis and a full-path tooltip. Active row has a 2px accent left border and `aria-current="page"`. |
| Conflict card | selected, unselected | Path in mono; two rows `This device` / `Other device` with size and modified time; a segmented choice of `Keep local`, `Keep remote`, `Keep both` (or `Delete on server` / `Delete here` when one side is deleted). |
| Preview | text, deleted, empty, loading | Two columns titled `This device` and `Other device`; body in mono, max height 280px, scrolls. |
| Result row | Upload, Download, DeleteLocal, DeleteRemote | Path in mono, kind as a tag. Lists are grouped `Uploaded` / `Downloaded`. |
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
| sections | `Vaults`, `Status`, `Account`, `Conflicts`, `Failed this sync`, `Last result`, `Devices`, `Invites`, `Manage vault` |
| primary actions | `Sync now`, `Apply resolutions` |
| setup | `Add vault`, `Create vault`, `Connect vault`, `Load vaults`, `Send sign-in code`, `Verify and sign in`, `Change email`, `Sign in` |
| account | `Invite someone`, `Copy`, `Sign out` (per device), `Delete account` |
| vault actions | `Remove from this device`, `Delete vault on server` (distinct from the conflict choice `Delete on server`) |
| device tag | `This device` (the same string as the conflict side, on purpose) |
| invite status | `Active`, `Used`, `Expired` |
| per-vault usage | `412 MiB of 1 GiB`; `412 MiB` when the server sets no cap |
| session | `Session expired. Sign in again.` with the action `Sign in` |
| server without email | `This server has no email sign-in.` |
| after actions | `Invite code created.`, `Invite code copied.`, `Device signed out.`, `Account deleted.`, `Removed <vault> from this device.`, `Deleted <vault> on the server.` |
| confirmation prompt | `Type <value> to confirm.` |
| confirmation copy | delete account: `This deletes your account, every vault it owns on <server>, and every signed-in device. Vault folders on this device stay.`; delete vault: `This deletes <vault> and all of its files on <server>. The folder on this device stays.`; remove: `The vault stays on the server. The key is removed from the keychain, so connecting again needs the passphrase.` |
| modes | `Create`, `Connect` |
| conflict sides | `This device`, `Other device` |
| conflict choices | `Keep local`, `Keep remote`, `Keep both`, `Delete on server`, `Delete here` |
| busy | `Working…` |
| status line | `Synced · ↑n ↓n`; `n conflicts need attention`; `Error: …` |
| stale banner | `n files changed on another device. Sync before editing.` |
| pending local | `n local changes not uploaded yet` |
| key state | `Key saved on this device` |

Empty states:

- Vaults: `No vaults yet.`
- Last result: `Run a sync to see uploads and downloads.`
- Conflicts: `Conflicts appear here when a sync needs a decision.`
- Result column: `No entries.`
- Preview: `Deleted in this version.` / `Empty file.`
- Devices: `No other devices signed in.` (iOS, where the current device is
  the picker's context); desktop always lists the current device.
- Invites: `No invites yet.`

The invite code field appears only when the server reports
`invite_required` (`GET /`) or has just refused a sign-up without one; in the
second case it takes focus. Sign-in methods the server does not offer are
not shown.

## 6. Platform mapping

### Desktop (Tauri, React)

- Tokens live on `:root` in `desktop/src/styles.css`; light values are the
  default, dark values under `@media (prefers-color-scheme: dark)`.
  `color-scheme: light dark` so native controls follow.
- Layout: a 240px sidebar (vault list, `Add vault`, `Account`) and a main
  pane (active vault name with the per-vault usage, `Sync now`, status
  counts, notices, `Last result`, `Conflicts`, `Manage vault`). Setup
  (server URL, sign-in, invite, `Devices`, `Invites`, `Delete account`, add
  vault) is a dedicated view that replaces the main pane; the app opens on
  it when no vault is configured.
- Errors reach the UI typed (`CommandError { kind, message, status }`); a
  401 shows the session notice with `Sign in`, which opens Setup at the
  account section.
- Window: default 1000x680, minimum 760x520. Two-column groups stack below
  860px.
- Menu bar: the template silhouette; menu items `Sync now`, `Show ObSink`,
  `Quit ObSink`.
- Icons: `desktop/src-tauri/icons/` holds the generated macOS set
  (`32x32.png`, `128x128.png`, `128x128@2x.png`, `icon.icns`, `icon.png`)
  and `tray.png`; `tauri.conf.json` `bundle.icon` lists them.

### iOS (SwiftUI)

- Colours: system semantic colours (`.primary`, `.secondary`,
  `.systemGroupedBackground`) for surfaces and text; `.orange` for warning,
  `.red` for danger, `.green` for success. Asset catalog colorsets:
  `AccentColor` (`--color-accent`, light and dark variants) for tinted text
  and controls; `Amber` and `Ink` (brand constants) for the primary button.
- Structure: `Form` with sections named per section 5; each header is a
  `Label` with an SF Symbol (`folder`, `arrow.triangle.2.circlepath`,
  `person.crop.circle`, `arrow.triangle.branch`, `xmark.octagon`).
- `Sync now` is a full-width `.borderedProminent` button, amber with ink
  text. There is no `Last result` section on iOS; the status line carries
  the last result.
- `Devices`, `Invites`, and `Manage vault` are pushed screens
  (`NavigationLink`) from the root form; typed confirmations are a sheet
  with a medium detent.
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
  `addVaultServerURL`, `addVaultAccountText`, `listVaultsButton`,
  `vaultPicker`, `addVaultStatusText`, `addVaultPassphraseField`,
  `addVaultSubmitButton`, `staleBanner`, `conflictRowTitle`,
  `winnerPicker`, `applyResolutionsButton`, `vaultUsageText`,
  `manageVaultButton`, `removeVaultButton`, `deleteVaultButton`,
  `devicesLink`, `deviceRow`, `deviceSignOutButton`, `invitesLink`,
  `inviteRow`, `deleteAccountButton`, `signInButton`, `sessionExpiredText`,
  `confirmationField`, `confirmDestructiveButton`, `confirmCancelButton`,
  `addVaultDoneButton`; and by these labels: the `Connect` segment, the
  `Keep local` / `Keep remote` / `Keep both` segments, the
  `Remove from this device` confirmation button, the status prefixes
  `Synced ·`, `Added vault`, `1 conflict`, and the banner text
  `changed on another device`. Renaming any of them means updating the
  tests and the harness in the same change.
