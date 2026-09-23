#!/usr/bin/env node
// End-to-end check of the browser client with two headless Chromium contexts
// as two devices, against a running dev server and local stack:
//
//   docker compose up -d                       (AUTH_DEV_RETURN_CODE=1 returns the sign-in code)
//   wasm-pack build core-wasm --target web && npm run dev -w web
//   npx playwright install chromium            (once)
//   OBSINK_INVITE_CODE=... node scripts/verify-web-e2e.mjs
//
// Env: OBSINK_WEB_URL (default http://localhost:5173/app/), OBSINK_SERVER_URL
// (default http://localhost:18080), OBSINK_INVITE_CODE (an invite from a signed-in
// account, when the stack already has accounts). An OPFS directory stands in for the picked
// folder: `showDirectoryPicker` is shimmed, everything else is the real client.
//
// Flow: A signs in and creates a vault with a.md; B connects with the same
// passphrase and gets a.md; both edit a.md; B resolves the conflict with Keep
// both; A ends up with a.md and a.conflict.md.

import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { chromium } from 'playwright'

const WEB = process.env.OBSINK_WEB_URL ?? 'http://localhost:5173/app/'
const SERVER = process.env.OBSINK_SERVER_URL ?? 'http://localhost:18080'
const EMAIL = `web-e2e-${Date.now()}@example.test`
const PASSPHRASE = 'e2e-passphrase'
const VAULT = `e2e-${Date.now()}`

// A thrown failure unwinds through the `finally` below, so the devices'
// profiles are removed and the contexts closed even on a failed run.
function fail(message) {
  throw new Error(`FAIL: ${message}`)
}

async function api(path, options = {}) {
  const response = await fetch(`${SERVER}${path}`, options)
  if (!response.ok) fail(`${path} -> ${response.status} ${await response.text()}`)
  return response.json()
}

// A fresh CI database needs no invite (the first account is free). Against a
// stack that already has accounts, pass one minted by a signed-in account
// (`obsink invite`) as OBSINK_INVITE_CODE.
async function mintInvite() {
  const { invite_required } = await api('/', { headers: { Accept: 'application/json' } })
  if (!invite_required) return null
  const code = process.env.OBSINK_INVITE_CODE
  if (!code)
    fail('the server needs an invite for a new account; set OBSINK_INVITE_CODE (obsink invite)')
  return code
}

// The dev server returns the code from /auth/email/start; the UI fills it in.
const shim = `
  window.showDirectoryPicker = async () => {
    const root = await navigator.storage.getDirectory()
    return root.getDirectoryHandle('e2e-vault', { create: true })
  }
`

// The client's IndexedDB (`web/src/shared/db.ts`), from the page.
const idbGet = ([store, key]) =>
  new Promise((resolve, reject) => {
    const open = indexedDB.open('obsink')
    open.onerror = () => reject(open.error)
    open.onsuccess = () => {
      const request = open.result.transaction(store, 'readonly').objectStore(store).get(key)
      request.onsuccess = () => resolve(request.result ?? null)
      request.onerror = () => reject(request.error)
    }
  })
// The page has opened (and versioned) the database by the time this runs,
// so no version is passed: the client owns the schema.
const idbPut = ([store, key, value]) =>
  new Promise((resolve, reject) => {
    const open = indexedDB.open('obsink')
    open.onerror = () => reject(open.error)
    open.onsuccess = () => {
      const request = open.result.transaction(store, 'readwrite').objectStore(store).put(value, key)
      request.onsuccess = () => resolve(undefined)
      request.onerror = () => reject(request.error)
    }
  })

async function writeFile(page, name, text) {
  await page.evaluate(
    async ([name, text]) => {
      const root = await navigator.storage.getDirectory()
      const dir = await root.getDirectoryHandle('e2e-vault', { create: true })
      const handle = await dir.getFileHandle(name, { create: true })
      const writable = await handle.createWritable()
      await writable.write(text)
      await writable.close()
    },
    [name, text],
  )
}

async function readFiles(page) {
  return page.evaluate(async () => {
    const root = await navigator.storage.getDirectory()
    const dir = await root.getDirectoryHandle('e2e-vault', { create: true })
    const out = {}
    for await (const [name, handle] of dir.entries()) {
      if (handle.kind === 'file') out[name] = await (await handle.getFile()).text()
    }
    return out
  })
}

// Spec §12.1: sign in, then set the account passphrase (a new account) or
// enter it (a device that came later).
async function signIn(page, invite) {
  await page.getByRole('textbox', { name: 'Email' }).fill(EMAIL)
  await page.getByRole('button', { name: 'Send sign-in code' }).click()
  await page.getByRole('button', { name: 'Verify and sign in' }).waitFor()
  if (invite) await page.getByRole('textbox', { name: 'Invite code' }).fill(invite)
  await page.getByRole('button', { name: 'Verify and sign in' }).click()
  await page.getByRole('heading', { name: 'Set passphrase' }).waitFor()
  await page.getByRole('textbox', { name: 'Passphrase' }).fill(PASSPHRASE)
  await page.getByRole('textbox', { name: 'Again' }).fill(PASSPHRASE)
  await page.getByRole('button', { name: 'Set passphrase' }).click()
  await page.getByText('Passphrase set.').waitFor({ timeout: 60_000 })
}

async function unlock(page) {
  await page.getByRole('heading', { name: 'Unlock' }).waitFor()
  await page.getByRole('textbox', { name: 'Passphrase' }).fill(PASSPHRASE)
  await page.getByRole('button', { name: 'Unlock' }).click()
  await page.getByText('Unlocked.').waitFor({ timeout: 60_000 })
}

// The folder step of Create vault / Download, then the done card.
async function folderStep(page, submitTestId, doneHeading) {
  await page.getByRole('button', { name: 'Choose folder' }).click()
  await page.getByText('e2e-vault').waitFor()
  await page.getByTestId(submitTestId).click()
  await page.getByRole('heading', { name: doneHeading }).waitFor({ timeout: 60_000 })
  await page.getByRole('button', { name: 'Done' }).click()
}

// The notice from an earlier sync may still be on the page, so the wait
// follows the cycle itself: the button reads Working… while it runs.
async function syncNow(page) {
  const button = page.getByRole('button', { name: 'Sync now' })
  await button.click()
  await page
    .getByRole('button', { name: 'Working…' })
    .waitFor({ timeout: 5_000 })
    .catch(() => undefined)
  await button.waitFor({ timeout: 60_000 })
  await page.getByText(/Sync complete|needs attention|Sync finished/).waitFor({ timeout: 60_000 })
}

// One persistent profile per device: an ephemeral context keeps OPFS in
// memory and Chrome cannot hand such a directory handle to a worker through
// IndexedDB (the browser process exits), while a profile on disk can.
async function device() {
  let closing = false
  const profile = await mkdtemp(join(tmpdir(), 'obsink-e2e-'))
  const context = await chromium.launchPersistentContext(profile, {
    channel: 'chromium',
    headless: process.env.OBSINK_E2E_HEADED !== '1',
  })
  context.on('close', () => {
    if (!closing) {
      console.error(
        'the browser closed on its own; with an ephemeral context Chrome exits when a worker ' +
          'receives an OPFS handle through IndexedDB, so keep launchPersistentContext',
      )
    }
  })
  await context.addInitScript(shim)
  const page = context.pages()[0] ?? (await context.newPage())
  page.on('pageerror', (error) => console.error(`page error: ${error.message}`))
  page.on('console', (message) => {
    if (message.type() === 'error') console.error(`console: ${message.text()}`)
  })
  await page.goto(WEB)
  await page.getByRole('tab', { name: 'Vaults' }).waitFor()
  return {
    page,
    close: async () => {
      closing = true
      await context.close()
      await rm(profile, { recursive: true, force: true })
    },
  }
}

const invite = await mintInvite()
const devices = []
try {
  // Device A: create the vault with one note.
  const { page: a, close: closeA } = await device()
  devices.push(closeA)
  await writeFile(a, 'a.md', '# from A\n')
  await signIn(a, invite)
  await a.getByRole('main').getByRole('button', { name: 'Create vault' }).click()
  await a.getByRole('textbox', { name: 'Vault name' }).fill(VAULT)
  await a.getByRole('button', { name: 'Next' }).click()
  await folderStep(a, 'createVaultSubmitButton', `Created ${VAULT}`)
  await syncNow(a)
  console.log('A: vault created and a.md uploaded')

  // Device B: the same account (a second sign-in for one address within a
  // minute hits the server's email cooldown, so B carries A's session),
  // unlock with the passphrase, download the vault, get the note.
  const { page: b, close: closeB } = await device()
  devices.push(closeB)
  await b.evaluate(idbPut, [
    'kv',
    `bearer:${new URL(WEB).origin}`,
    await a.evaluate(idbGet, ['kv', `bearer:${new URL(WEB).origin}`]),
  ])
  await b.reload()
  await unlock(b)
  await b.getByTestId('vaultStateText').filter({ hasText: 'Not on this device' }).first().waitFor()
  await b.getByTestId('downloadVaultButton').first().click()
  await folderStep(b, 'downloadVaultSubmitButton', `Downloaded ${VAULT}`)
  await syncNow(b)
  const onB = await readFiles(b)
  if (onB['a.md'] !== '# from A\n') fail(`B did not receive a.md: ${JSON.stringify(onB)}`)
  console.log('B: connected and a.md downloaded')

  // Both edit a.md: A pushes first, B then hits the conflict.
  await writeFile(a, 'a.md', '# from A, edited\n')
  await syncNow(a)
  await writeFile(b, 'a.md', '# from B, edited\n')
  await syncNow(b)
  await b.getByText('1 conflict needs attention.').waitFor()
  await b.getByRole('button', { name: 'Keep both' }).click()
  await b.getByRole('button', { name: 'Apply resolutions' }).click()
  await b.getByText('Conflict resolutions applied.').waitFor({ timeout: 60_000 })
  const resolved = await readFiles(b)
  if (
    resolved['a.md'] !== '# from B, edited\n' ||
    resolved['a.conflict.md'] !== '# from A, edited\n'
  ) {
    fail(`B resolution wrong: ${JSON.stringify(resolved)}`)
  }
  console.log('B: conflict resolved with Keep both')

  // A pulls both versions.
  await syncNow(a)
  const onA = await readFiles(a)
  if (onA['a.md'] !== '# from B, edited\n' || onA['a.conflict.md'] !== '# from A, edited\n') {
    fail(`A did not receive the resolution: ${JSON.stringify(onA)}`)
  }
  console.log('A: both versions received')

  // Clean up the vault on the server. A poll cycle may be running on A at
  // this moment (the delete is refused while one runs), so retry briefly.
  await a.getByRole('button', { name: 'Delete vault on server' }).first().click()
  await a.getByRole('textbox', { name: /to confirm/ }).fill(VAULT)
  const outcome = a.getByText(
    new RegExp(`Deleted ${VAULT} on the server\\.|A sync is running on this vault`),
  )
  for (let attempt = 0; attempt < 10; attempt++) {
    await a.getByRole('button', { name: 'Delete vault on server' }).last().click()
    await outcome.waitFor()
    if ((await outcome.textContent())?.startsWith('Deleted')) break
    await a.waitForTimeout(2000)
  }
  await a.getByText(`Deleted ${VAULT} on the server.`).waitFor()
  console.log('PASS: browser client end-to-end')
} finally {
  for (const close of devices) await close()
}
