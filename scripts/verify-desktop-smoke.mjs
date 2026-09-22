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
// OBSINK_SMOKE_SHOTS=<dir> shows each window for a moment to capture
// conflict.png and popover.png (off by default). A server that is not
// localhost is refused unless OBSINK_SMOKE_ALLOW_REMOTE=1.
//
// It builds the debug binary with the frontend embedded, launches it with a
// sandboxed HOME and the file-backed keyring, signs in and creates a vault
// through the same commands the UI calls, then drives the real settings window
// and popover by data-testid: sync, activity log, a conflict against the CLI
// as the second device, Keep both, vault deletion. Every window stays hidden
// (the web views run either way) and no mouse or keyboard input is ever
// posted, so the run does not interfere with whatever else is on the screen;
// with OBSINK_SMOKE_SHOTS the windows are shown without taking focus. The tray
// menu is read through System Events, which needs Accessibility permission
// for the terminal; that step is reported, not fatal, when it is missing.

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
const SHOTS = process.env.OBSINK_SMOKE_SHOTS
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

// Only with OBSINK_SMOKE_SHOTS: show the window (the seam's `show` does not
// activate the app or focus the window), capture its bounds, hide it again.
async function screenshot(window, name) {
  if (!SHOTS) return
  const file = join(SHOTS, name)
  await seam('show', { window })
  try {
    await sleep(500)
    const bounds = await seam('bounds', { window })
    const region = [bounds.x, bounds.y, bounds.width, bounds.height].map(Math.round).join(',')
    run('screencapture', ['-x', '-R', region, file])
    console.log(`shot: ${file}`)
  } finally {
    await seam('hide', { window })
  }
}

// Selects a vault in the settings window through the event the tray and the
// popover use; `open_settings` would also show and focus the window.
const navigate = (vaultId) =>
  evalIn(
    'settings',
    `await window.__TAURI_INTERNALS__.invoke('plugin:event|emit', {
       event: 'settings://navigate',
       payload: { tab: 'vaults', vault_id: ${JSON.stringify(vaultId)}, add_vault: false },
     })`,
  )

// ---- tray (System Events reads the menu; nothing is clicked) ------------

// The menu's items are readable through the accessibility API without opening
// the menu, so the tray needs no synthetic input at all.
async function trayStep() {
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
const vaultA = join(sandbox, 'vaultA')
const vaultB = join(sandbox, 'vaultB')
await mkdir(join(vaultA, 'notes'), { recursive: true })
await mkdir(vaultB, { recursive: true })
if (SHOTS) await mkdir(SHOTS, { recursive: true })
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
  await navigate(vaultId)
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
  // The vault page carries its own activity log (spec §15.2).
  await until(
    'settings',
    `return [...document.querySelectorAll('[data-testid=activityRow]')].some((el) => el.textContent.includes(${JSON.stringify(NOTE)}))`,
    'an activity row for the note',
  )
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
  await screenshot('settings', 'conflict.png')
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

  // Popover: the row and Sync now, driven hidden like the settings window.
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
  await screenshot('popover', 'popover.png')
  console.log('popover: row and Sync now work')

  trayProblem = await trayStep()
  if (trayProblem) console.log(`WARN: tray step skipped: ${trayProblem}`)
  else console.log('tray: menu has the four items')

  // Delete the vault on the server through the UI. A daemon cycle may be
  // running at this moment, in which case the delete is refused; retry.
  await navigate(vaultId)
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
  console.log(SHOTS ? `PASS: desktop smoke (screenshots in ${SHOTS})` : 'PASS: desktop smoke')
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
