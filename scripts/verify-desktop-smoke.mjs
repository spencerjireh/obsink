#!/usr/bin/env node
// Full-flow smoke of the desktop app through its debug-only automation seam
// (desktop/src-tauri/src/automation.rs), against a running dev server:
//
//   OBSINK_PORT=18080 docker compose up -d --wait
//   node scripts/verify-desktop-smoke.mjs
//
// Env: OBSINK_SERVER_URL (default http://localhost:18080), OBSINK_API_KEY
// (operator bearer, default dev-operator-key; mints the invite a new account
// needs), OBSINK_SMOKE_KEEP=1 keeps the sandbox and the app running at the end,
// OBSINK_SMOKE_SHOTS=<dir> for the screenshots (default <sandbox>/shots). A
// server that is not localhost is refused unless OBSINK_SMOKE_ALLOW_REMOTE=1.
//
// It builds the debug binary with the frontend embedded, launches it with a
// sandboxed HOME and the file-backed keyring, signs in and creates a vault
// through the same commands the UI calls, then drives the real settings window
// and popover by data-testid: sync, activity log, a conflict against the CLI
// as the second device, Keep both, vault deletion. The tray menu is opened
// with a real right-click (uv + pyobjc) and read through System Events; that
// step needs Accessibility permission for the terminal and is reported, not
// fatal, when it is missing.

import { spawn, spawnSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import net from 'node:net'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const REPO = join(dirname(fileURLToPath(import.meta.url)), '..')
const SERVER = process.env.OBSINK_SERVER_URL ?? 'http://localhost:18080'
const API_KEY = process.env.OBSINK_API_KEY ?? 'dev-operator-key'
const KEEP = process.env.OBSINK_SMOKE_KEEP === '1'
const EMAIL = `desktop-smoke-${Date.now()}@example.test`
const PASSPHRASE = 'smoke-passphrase'
const VAULT = `smoke-${Date.now()}`
const NOTE = 'notes/a.md'

function fail(message) {
  throw new Error(`FAIL: ${message}`)
}

function expect(condition, message) {
  if (!condition) fail(message)
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { cwd: REPO, encoding: 'utf8', ...options })
  if (result.status !== 0) {
    fail(`${command} ${args.join(' ')} exited ${result.status}\n${result.stdout}${result.stderr}`)
  }
  return result.stdout
}

// ---- server ---------------------------------------------------------------

async function api(path, options = {}) {
  const response = await fetch(`${SERVER}${path}`, options)
  if (!response.ok) fail(`${path} -> ${response.status} ${await response.text()}`)
  return response.json()
}

async function mintInvite() {
  const { invite_required } = await api('/', { headers: { Accept: 'application/json' } })
  if (!invite_required) return null
  const { invite } = await api('/auth/invites', {
    method: 'POST',
    headers: { Authorization: `Bearer ${API_KEY}` },
  })
  return invite.code
}

// ---- seam -----------------------------------------------------------------

let port = 0

function seam(op, args = {}) {
  return new Promise((resolve, reject) => {
    let buffer = ''
    let settled = false
    const done = (error, value) => {
      if (settled) return
      settled = true
      error ? reject(error) : resolve(value)
    }
    const socket = net.connect(port, '127.0.0.1')
    socket.setTimeout(45_000)
    socket.on('connect', () => socket.write(`${JSON.stringify({ op, ...args })}\n`))
    socket.on('data', (chunk) => {
      buffer += chunk
      if (buffer.includes('\n')) socket.end()
    })
    socket.on('timeout', () => socket.destroy(new Error(`${op}: no reply in 45 s`)))
    socket.on('error', (error) => done(error))
    socket.on('close', () => {
      let reply
      try {
        reply = JSON.parse(buffer.trim())
      } catch {
        return done(new Error(`${op}: bad reply ${JSON.stringify(buffer)}`))
      }
      reply.ok ? done(null, reply.value) : done(new Error(`${op}: ${reply.error}`))
    })
  })
}

// `body` is the body of an async function run inside the window.
const evalIn = (window, body) => seam('eval', { window, js: body })

const invoke = (command, args = {}) =>
  evalIn(
    'settings',
    `return await window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)}, ${JSON.stringify(args)})`,
  )

const click = (window, selector) =>
  evalIn(
    window,
    `const el = document.querySelector(${JSON.stringify(selector)})
     if (!el) throw new Error('no element ' + ${JSON.stringify(selector)})
     if (el.disabled) throw new Error('disabled: ' + ${JSON.stringify(selector)})
     el.click()
     return true`,
  )

const type = (window, selector, value) =>
  evalIn(
    window,
    `const el = document.querySelector(${JSON.stringify(selector)})
     if (!el) throw new Error('no element ' + ${JSON.stringify(selector)})
     const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set
     set.call(el, ${JSON.stringify(value)})
     el.dispatchEvent(new Event('input', { bubbles: true }))
     return true`,
  )

const texts = (window, selector) =>
  evalIn(
    window,
    `return [...document.querySelectorAll(${JSON.stringify(selector)})].map((el) => el.textContent ?? '')`,
  )

// Polls `body` (an async function body returning a value) until it is truthy.
async function until(window, body, what, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs
  let last
  while (Date.now() < deadline) {
    last = await evalIn(window, body)
    if (last) return last
    await sleep(250)
  }
  fail(`timed out waiting for ${what} (last: ${JSON.stringify(last)})`)
}

const noticeIncludes = (window, text) =>
  until(
    window,
    `return [...document.querySelectorAll('[data-testid=statusText]')].some((el) => (el.textContent ?? '').includes(${JSON.stringify(text)}))`,
    `notice "${text}"`,
  )

async function screenshot(window, file) {
  const bounds = await seam('bounds', { window })
  const region = [bounds.x, bounds.y, bounds.width, bounds.height].map(Math.round).join(',')
  run('screencapture', ['-x', '-R', region, file])
  console.log(`shot: ${file}`)
}

// ---- tray (System Events for the menu; uv + pyobjc for a best-effort shot)

const TRAY_PY = `# /// script
# requires-python = ">=3.11"
# dependencies = ["pyobjc-framework-Quartz"]
# ///
import sys, time, Quartz

def find(pid):
    # The status item is a layer-25 window hosted by Control Center and named
    # after the app's pid, once per display; prefer the main display's copy.
    main = Quartz.CGDisplayBounds(Quartz.CGMainDisplayID())
    found = None
    for w in Quartz.CGWindowListCopyWindowInfo(Quartz.kCGWindowListOptionAll, Quartz.kCGNullWindowID):
        if w.get("kCGWindowLayer") != 25 or str(w.get("kCGWindowName") or "") != str(pid):
            continue
        b = w["kCGWindowBounds"]
        on_main = Quartz.CGRectContainsPoint(main, Quartz.CGPointMake(b["X"] + 1, b["Y"] + 1))
        if found is None or on_main:
            found = (int(b["X"]), int(b["Y"]), int(b["Width"]), int(b["Height"]))
    if found is None:
        sys.exit(2)
    print(*found)

def rclick(x, y):
    p = Quartz.CGPointMake(x, y)
    post = lambda kind, button: Quartz.CGEventPost(
        Quartz.kCGHIDEventTap, Quartz.CGEventCreateMouseEvent(None, kind, p, button))
    post(Quartz.kCGEventMouseMoved, 0)
    time.sleep(0.05)
    post(Quartz.kCGEventRightMouseDown, 1)
    time.sleep(0.05)
    post(Quartz.kCGEventRightMouseUp, 1)

def escape():
    for down in (True, False):
        Quartz.CGEventPost(Quartz.kCGHIDEventTap, Quartz.CGEventCreateKeyboardEvent(None, 53, down))

mode = sys.argv[1]
if mode == "find":
    find(int(sys.argv[2]))
elif mode == "rclick":
    rclick(float(sys.argv[2]), float(sys.argv[3]))
elif mode == "escape":
    escape()
`

// The menu's items are readable through the accessibility API whether or not
// the menu is open, so that is the assertion. The right-click screenshot is a
// bonus: the status item is listed once per display and the capture depends
// on which copy the window list returns, so it is reported, not asserted.
async function trayStep(app, sandbox, shots) {
  const names = spawnSync(
    'osascript',
    [
      '-e',
      'tell application "System Events" to tell process "obsink-desktop" to get name of menu items of menu 1 of menu bar item 1 of menu bar 2',
    ],
    { encoding: 'utf8' },
  )
  if (names.status !== 0) {
    return `System Events could not read the menu (${names.stderr.trim()}); grant Accessibility to the terminal in System Settings > Privacy & Security`
  }
  const items = names.stdout.trim().split(', ')
  for (const label of ['Sync now', 'Open settings', 'Check for updates', 'Quit ObSink']) {
    if (!items.includes(label)) return `tray menu lacks "${label}": ${items.join(' | ')}`
  }
  if (spawnSync('uv', ['--version']).status !== 0) {
    console.log('tray: no uv, skipping the menu screenshot')
    return null
  }
  const script = join(sandbox, 'tray.py')
  await writeFile(script, TRAY_PY)
  const py = (...args) => spawnSync('uv', ['run', '--quiet', script, ...args], { encoding: 'utf8' })
  const found = py('find', String(app.pid))
  if (found.status !== 0) {
    console.log('tray: status item not in the window list, skipping the menu screenshot')
    return null
  }
  const [x, y, w, h] = found.stdout.trim().split(' ').map(Number)
  py('rclick', String(x + w / 2), String(y + h / 2))
  await sleep(700)
  try {
    run('screencapture', ['-x', '-R', `${x - 200},${y},${w + 400},260`, join(shots, 'tray.png')])
    console.log(`shot: ${join(shots, 'tray.png')} (best effort)`)
  } finally {
    py('escape')
  }
  return null
}

// ---- main -----------------------------------------------------------------

if (
  !/^http:\/\/(localhost|127\.0\.0\.1)(:\d+)?$/.test(SERVER) &&
  process.env.OBSINK_SMOKE_ALLOW_REMOTE !== '1'
) {
  fail(
    `refusing ${SERVER} (creates accounts and vaults); set OBSINK_SMOKE_ALLOW_REMOTE=1 to override`,
  )
}
try {
  await api('/healthz')
} catch {
  fail(`no server at ${SERVER}; start it with: OBSINK_PORT=18080 docker compose up -d --wait`)
}

console.log('==> Building the desktop app (frontend embedded) and the CLI')
run('npm', ['run', 'build', '-w', 'desktop'], { stdio: ['ignore', 'ignore', 'inherit'] })
run('cargo', ['build', '-q', '-p', 'obsink-desktop', '--features', 'tauri/custom-protocol'], {
  stdio: ['ignore', 'ignore', 'inherit'],
})
run('cargo', ['build', '-q', '-p', 'obsink'], { stdio: ['ignore', 'ignore', 'inherit'] })
const APP = join(REPO, 'target/debug/obsink-desktop')
const CLI = join(REPO, 'target/debug/obsink')

const sandbox = await mkdtemp(join(tmpdir(), 'obsink-smoke-'))
const shots = process.env.OBSINK_SMOKE_SHOTS ?? join(sandbox, 'shots')
const vaultA = join(sandbox, 'vaultA')
const vaultB = join(sandbox, 'vaultB')
await mkdir(join(vaultA, 'notes'), { recursive: true })
await mkdir(vaultB, { recursive: true })
await mkdir(shots, { recursive: true })
const appLog = join(sandbox, 'app.log')

port = 40_000 + Math.floor(Math.random() * 20_000)
const app = spawn(APP, [], {
  cwd: sandbox,
  env: {
    ...process.env,
    HOME: sandbox,
    OBSINK_KEYRING_DIR: join(sandbox, 'keyring'),
    OBSINK_SERVER_URL: SERVER,
    OBSINK_AUTOMATION_PORT: String(port),
  },
  stdio: ['ignore', 'pipe', 'pipe'],
})
let log = ''
app.stdout.on('data', (chunk) => (log += chunk))
app.stderr.on('data', (chunk) => (log += chunk))
let exited = false
app.on('exit', () => (exited = true))

// The CLI plays device B on the same account: it gets the desktop session's
// bearer from the file keyring (the web harness hands its second device the
// first one's bearer the same way, which also dodges the email cooldown).
const cli = (home, bearer, ...args) => {
  const result = spawnSync(CLI, args, {
    cwd: sandbox,
    encoding: 'utf8',
    input: '',
    env: {
      ...process.env,
      OBSINK_HOME: home,
      OBSINK_KEYRING_DIR: join(home, 'keyring'),
      OBSINK_SERVER_URL: SERVER,
      OBSINK_API_KEY: bearer,
    },
  })
  if (result.status !== 0)
    fail(`obsink ${args.join(' ')} exited ${result.status}\n${result.stdout}${result.stderr}`)
  return result.stdout + result.stderr
}

let trayProblem = null
try {
  // The seam answers once setup ran; the settings web view loads right after.
  for (let attempt = 0; ; attempt++) {
    if (exited) fail(`the app exited during startup\n${log}`)
    try {
      await seam('ping')
      break
    } catch (error) {
      if (attempt > 60) fail(`no automation seam on port ${port} after 30 s: ${error.message}`)
      await sleep(500)
    }
  }
  await sleep(1000)
  await until(
    'settings',
    `return !!document.querySelector('[data-testid=settingsTab]')`,
    'the settings UI',
  )
  console.log('app: up, seam answering')

  // Sign in and create the vault through the commands the UI calls.
  const invite = await mintInvite()
  const code = await invoke('auth_email_start', { email: EMAIL })
  expect(
    typeof code === 'string' && code.length === 6,
    `dev server did not return the code inline: ${code}`,
  )
  const account = await invoke('auth_email_verify', { email: EMAIL, code, inviteCode: invite })
  expect(account?.email === EMAIL, `signed in as ${JSON.stringify(account)}`)
  await writeFile(join(vaultA, NOTE), '# from A\n')
  const added = await invoke('add_vault', {
    request: {
      mode: 'create',
      local_path: vaultA,
      vault_name: VAULT,
      vault_id: '',
      passphrase: PASSPHRASE,
    },
  })
  const vaultId = added.id
  expect(typeof vaultId === 'string' && vaultId, `add_vault returned ${JSON.stringify(added)}`)
  console.log(`setup: signed in as ${EMAIL}, vault ${vaultId}`)

  // Settings window: the vault page, Sync now, the activity log.
  await seam('show', { window: 'settings' })
  await invoke('open_settings', { target: { tab: 'vaults', vault_id: vaultId, add_vault: false } })
  await until(
    'settings',
    `return !!document.querySelector('[data-testid=syncButton]')`,
    'the vault page',
  )
  const cards = await texts('settings', `[data-testid=vaultCard][data-vault-id="${vaultId}"]`)
  expect(cards.length === 1 && cards[0].includes(VAULT), `vault card: ${JSON.stringify(cards)}`)
  await until(
    'settings',
    `return document.querySelector('[data-testid=syncButton]').textContent === 'Sync now'`,
    'Sync now',
  )
  await click('settings', '[data-testid=syncButton]')
  await noticeIncludes('settings', 'Sync complete.')
  await click('settings', '[data-testid=settingsTab][data-tab=activity]')
  await until(
    'settings',
    `return [...document.querySelectorAll('[data-testid=activityRow]')].some((el) => el.textContent.includes(${JSON.stringify(NOTE)}))`,
    'an activity row for the note',
  )
  await click('settings', '[data-testid=settingsTab][data-tab=vaults]')
  await invoke('open_settings', { target: { tab: 'vaults', vault_id: vaultId, add_vault: false } })
  console.log('settings: synced through the UI, activity row present')

  // Device B is the CLI; it gets the note, then both sides edit it. A's edit
  // lands first and its daemon waits out the 2 s batch window, so B's upload
  // wins the race and A's next cycle finds the conflict.
  const homeB = join(sandbox, 'cli')
  const bearer = (
    await readFile(join(sandbox, 'keyring', `bearer:${SERVER}`.replace(/[/:]/g, '_')), 'utf8')
  ).trim()
  cli(
    homeB,
    bearer,
    'connect',
    '--vault-id',
    vaultId,
    '--directory',
    vaultB,
    '--passphrase',
    PASSPHRASE,
  )
  expect(
    (await readFile(join(vaultB, NOTE), 'utf8')) === '# from A\n',
    'B did not receive the note',
  )
  await writeFile(join(vaultA, NOTE), '# from A, edited\n')
  await writeFile(join(vaultB, NOTE), '# from B, edited\n')
  const syncB = cli(homeB, bearer, 'sync')
  expect(
    !/conflict/i.test(syncB),
    `B hit the conflict instead of A (A's daemon uploaded first):\n${syncB}`,
  )
  await until(
    'settings',
    `return document.querySelector('[data-testid=syncButton]').textContent === 'Sync now'`,
    'Sync now',
  )
  await click('settings', '[data-testid=syncButton]')
  await noticeIncludes('settings', '1 conflict needs attention.')
  await until(
    'settings',
    `return !!document.querySelector('[data-testid=conflictRowTitle][data-path=${JSON.stringify(NOTE)}]')`,
    'the conflict card',
  )
  await screenshot('settings', join(shots, 'conflict.png'))
  await click('settings', '[data-testid=winnerPicker][data-choice=KeepBoth]')
  await click('settings', '[data-testid=applyResolutionsButton]')
  await noticeIncludes('settings', 'Conflict resolutions applied.')
  // Keep both on the resolving device: its own version stays a.md, the other
  // device's version becomes a.conflict.md.
  expect(
    (await readFile(join(vaultA, NOTE), 'utf8')) === '# from A, edited\n',
    'A did not keep its own a.md',
  )
  expect(
    (await readFile(join(vaultA, 'notes/a.conflict.md'), 'utf8')) === '# from B, edited\n',
    "A has no a.conflict.md with B's version",
  )
  cli(homeB, bearer, 'sync')
  expect(existsSync(join(vaultB, 'notes/a.conflict.md')), 'B did not receive a.conflict.md')
  console.log('conflict: Keep both applied through the UI, both devices have both versions')

  // Popover: the row, Sync now, a screenshot. Showing settings hides it and
  // it hides on focus loss, so hide settings first and capture right away.
  await seam('hide', { window: 'settings' })
  await seam('show', { window: 'popover' })
  await until(
    'popover',
    `const row = document.querySelector('[data-testid=popoverVaultRow][data-vault-id="${vaultId}"]')
     return !!row && row.textContent.includes(${JSON.stringify(VAULT)}) && row.textContent.includes('Up to date')`,
    'the popover row to read Up to date',
  )
  await click('popover', '[data-testid=popoverSyncButton]')
  await until(
    'popover',
    `return document.querySelector('[data-testid=popoverSyncButton]').textContent === 'Sync now'`,
    'the popover sync to finish',
  )
  await screenshot('popover', join(shots, 'popover.png'))
  await seam('hide', { window: 'popover' })
  console.log('popover: row and Sync now work')

  trayProblem = await trayStep(app, sandbox, shots)
  if (trayProblem) console.log(`WARN: tray step skipped: ${trayProblem}`)
  else console.log('tray: menu has the four items')

  // Delete the vault on the server through the UI. A daemon cycle may be
  // running at this moment, in which case the delete is refused; retry.
  await seam('show', { window: 'settings' })
  await invoke('open_settings', { target: { tab: 'vaults', vault_id: vaultId, add_vault: false } })
  await until(
    'settings',
    `return !!document.querySelector('[data-testid=deleteVaultButton]')`,
    'Manage vault',
  )
  await click('settings', '[data-testid=deleteVaultButton]')
  await until(
    'settings',
    `return !!document.querySelector('[data-testid=confirmationField]')`,
    'the confirmation form',
  )
  await type('settings', '[data-testid=confirmationField]', VAULT)
  for (let attempt = 0; ; attempt++) {
    await click('settings', '[data-testid=confirmDestructiveButton]')
    const outcome = await until(
      'settings',
      `const t = [...document.querySelectorAll('[data-testid=statusText]')].map((el) => el.textContent ?? '')
       return t.find((s) => s.startsWith('Deleted ') || s.includes('A sync is running')) ?? null`,
      'the delete outcome',
    )
    if (outcome.startsWith('Deleted')) break
    if (attempt >= 10) fail(`vault deletion kept being refused: ${outcome}`)
    await sleep(2000)
  }
  await noticeIncludes('settings', `Deleted ${VAULT} on the server.`)
  console.log('cleanup: vault deleted on the server')
  console.log(`PASS: desktop smoke (screenshots in ${shots})`)
} catch (error) {
  console.error(error.message)
  console.error(`--- app log (${appLog})\n${log.split('\n').slice(-40).join('\n')}`)
  process.exitCode = 1
} finally {
  await writeFile(appLog, log)
  if (KEEP) {
    console.log(`kept: app pid ${app.pid}, seam port ${port}, sandbox ${sandbox}`)
  } else {
    if (!exited) app.kill('SIGTERM')
    await rm(sandbox, { recursive: true, force: true })
  }
}
