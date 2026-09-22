import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'
import { BackendProvider } from '../backend'
import type { AccountState } from '../types'
import { mockBackend, unlockedAccount, vault } from '../test/mock-backend'
import { SettingsApp } from './SettingsApp'

afterEach(cleanup)

function renderApp(backend: ReturnType<typeof mockBackend>) {
  return render(
    <BackendProvider backend={backend}>
      <SettingsApp />
    </BackendProvider>,
  )
}

describe('the unlock step (spec §12.1)', () => {
  it('sets the passphrase on a new account, twice and at least 12 characters', async () => {
    let account: AccountState = { kind: 'locked', user_id: 'usr_1', email: 'me@x', has_key: false }
    const backend = mockBackend({
      getAccount: () => Promise.resolve(account),
      setPassphrase: (passphrase: string) => {
        expect(passphrase).toBe('correct horse battery')
        account = unlockedAccount()
        return Promise.resolve({ outcome: 'created' as const, account })
      },
    })
    renderApp(backend)
    const field = await screen.findByTestId('unlockField')
    const button = screen.getByTestId('setPassphraseButton') as HTMLButtonElement
    expect(button.disabled).toBe(true)

    fireEvent.change(field, { target: { value: 'short' } })
    fireEvent.change(screen.getByTestId('unlockConfirmField'), { target: { value: 'short' } })
    expect(button.disabled).toBe(true)

    fireEvent.change(field, { target: { value: 'correct horse battery' } })
    fireEvent.change(screen.getByTestId('unlockConfirmField'), { target: { value: 'nope' } })
    fireEvent.click(button)
    expect(await screen.findByText('The passphrases do not match.')).toBeTruthy()

    fireEvent.change(screen.getByTestId('unlockConfirmField'), {
      target: { value: 'correct horse battery' },
    })
    fireEvent.click(button)
    await waitFor(() =>
      expect(backend.calls.some((call) => call.method === 'setPassphrase')).toBe(true),
    )
    expect(
      await screen.findByText('Passphrase set. There is no recovery if it is lost.'),
    ).toBeTruthy()
  })

  it('turns into Unlock after a lost first-set race', async () => {
    let account: AccountState = { kind: 'locked', user_id: 'usr_1', email: 'me@x', has_key: false }
    const backend = mockBackend({
      getAccount: () => Promise.resolve(account),
      setPassphrase: () => {
        account = { kind: 'locked', user_id: 'usr_1', email: 'me@x', has_key: true }
        return Promise.reject({ kind: 'server', status: 409, message: 'set elsewhere' })
      },
      unlock: () => {
        account = unlockedAccount()
        return Promise.resolve(account)
      },
    })
    renderApp(backend)
    const field = await screen.findByTestId('unlockField')
    fireEvent.change(field, { target: { value: 'correct horse battery' } })
    fireEvent.change(screen.getByTestId('unlockConfirmField'), {
      target: { value: 'correct horse battery' },
    })
    fireEvent.click(screen.getByTestId('setPassphraseButton'))
    expect(
      await screen.findByText('A passphrase was already set on another device. Enter it.'),
    ).toBeTruthy()
    // The confirm field is gone: this is the Unlock form now.
    expect(screen.queryByTestId('unlockConfirmField')).toBeNull()
    fireEvent.change(screen.getByTestId('unlockField'), {
      target: { value: 'correct horse battery' },
    })
    fireEvent.click(screen.getByTestId('unlockButton'))
    expect(await screen.findByText('Unlocked.')).toBeTruthy()
  })

  it('shows the protocol page when the server speaks another version', async () => {
    const backend = mockBackend({ getProtocol: () => Promise.resolve({ server: 2, client: 3 }) })
    renderApp(backend)
    expect(await screen.findByTestId('updateRequiredText')).toBeTruthy()
    expect(screen.queryByRole('tab')).toBeNull()
  })
})

describe('the vault list (spec §15.1)', () => {
  it('lists every vault of the account with Download on the ones not here', async () => {
    const backend = mockBackend({
      listVaults: () =>
        Promise.resolve([
          vault(),
          vault({
            id: 'vault_2',
            name: 'Work',
            local_path: null,
            state: { kind: 'not_on_device' },
            devices: [
              {
                id: 'dev-1',
                name: 'MacBook',
                platform: 'macos',
                last_synced: 1_700_000_000,
                last_revision: 3,
              },
            ],
          }),
        ]),
    })
    renderApp(backend)
    const cards = await screen.findAllByTestId('vaultCard')
    expect(cards).toHaveLength(2)
    expect(screen.getAllByTestId('downloadVaultButton')).toHaveLength(1)
    expect(screen.getByText('Not on this device')).toBeTruthy()
    // The row and the selected vault's page both say it.
    expect(screen.getAllByText('Up to date').length).toBeGreaterThan(0)
  })

  it('opens the Download flow from the row and calls downloadVault with the folder', async () => {
    const backend = mockBackend({
      listVaults: () =>
        Promise.resolve([
          vault({
            id: 'vault_2',
            name: 'Work',
            local_path: null,
            state: { kind: 'not_on_device' },
          }),
        ]),
      downloadVault: (request) =>
        Promise.resolve({ id: request.vault_id, name: 'Work', local_path: request.local_path }),
    })
    renderApp(backend)
    fireEvent.click(await screen.findByTestId('downloadVaultButton'))
    expect(await screen.findByRole('heading', { name: 'Download Work' })).toBeTruthy()
    fireEvent.change(screen.getByTestId('addVaultPathField'), {
      target: { value: '/Users/me/Work' },
    })
    fireEvent.click(screen.getByTestId('downloadVaultSubmitButton'))
    expect(await screen.findByRole('heading', { name: 'Downloaded Work' })).toBeTruthy()
    const call = backend.calls.find((entry) => entry.method === 'downloadVault')
    expect(call?.args[0]).toEqual({ vault_id: 'vault_2', local_path: '/Users/me/Work' })
  })

  it('creates a vault from a name and a folder', async () => {
    const backend = mockBackend({
      createVault: (request) =>
        Promise.resolve({
          id: 'vault_9',
          name: request.vault_name,
          local_path: request.local_path,
        }),
    })
    renderApp(backend)
    fireEvent.click((await screen.findAllByTestId('createVaultButton'))[0])
    fireEvent.change(await screen.findByTestId('createVaultNameField'), {
      target: { value: 'Journal' },
    })
    fireEvent.click(screen.getByTestId('addVaultNextButton'))
    fireEvent.change(await screen.findByTestId('addVaultPathField'), {
      target: { value: '/Users/me/Journal' },
    })
    await act(async () => {
      fireEvent.click(screen.getByTestId('createVaultSubmitButton'))
    })
    expect(await screen.findByRole('heading', { name: 'Created Journal' })).toBeTruthy()
    const call = backend.calls.find((entry) => entry.method === 'createVault')
    expect(call?.args[0]).toEqual({ vault_name: 'Journal', local_path: '/Users/me/Journal' })
  })
})

const otherDevice = {
  id: 'dev-2',
  name: 'Work laptop',
  platform: 'macos' as const,
  created: 1_700_000_000,
  last_seen: 1_700_000_000,
  current: false,
  vault_ids: ['vault_1'],
}
const thisDevice = { ...otherDevice, id: 'dev-1', name: 'MacBook', current: true, vault_ids: [] }

function accountWithDevices(devices = [thisDevice, otherDevice]): AccountState {
  return { kind: 'account', user_id: 'usr_1', email: 'me@example.com', devices, usage: null }
}

describe('the tabs (spec §15)', () => {
  it('routes Vaults, Devices and Settings', async () => {
    const backend = mockBackend({
      getAccount: () => Promise.resolve(accountWithDevices()),
      listVaults: () => Promise.resolve([vault()]),
    })
    renderApp(backend)
    const tabs = await screen.findAllByRole('tab')
    expect(tabs.map((tab) => tab.textContent)).toEqual(['Vaults', 'Devices', 'Settings'])

    fireEvent.click(tabs[1])
    const rows = await screen.findAllByTestId('deviceRow')
    expect(rows).toHaveLength(2)
    expect(within(rows[0]).getByText('This device')).toBeTruthy()
    expect(within(rows[0]).getByText('No vaults on this device.')).toBeTruthy()
    expect(within(rows[1]).getByText('Notes')).toBeTruthy()

    fireEvent.click(tabs[2])
    expect((await screen.findByTestId('signedInAsText')).textContent).toContain('me@example.com')
    expect(screen.getByTestId('changePassphraseButton')).toBeTruthy()
    expect(screen.getByTestId('deleteAccountButton')).toBeTruthy()
  })

  it('renames a device in place', async () => {
    let account = accountWithDevices()
    const backend = mockBackend({
      getAccount: () => Promise.resolve(account),
      renameDevice: (deviceId: string, name: string) => {
        account = accountWithDevices([thisDevice, { ...otherDevice, id: deviceId, name }])
        return Promise.resolve(account)
      },
    })
    renderApp(backend)
    fireEvent.click((await screen.findAllByRole('tab'))[1])
    const row = (await screen.findAllByTestId('deviceRow'))[1]
    fireEvent.click(within(row).getByTestId('deviceRenameButton'))
    fireEvent.change(within(row).getByTestId('deviceRenameField'), {
      target: { value: 'Office Mac' },
    })
    fireEvent.click(within(row).getByTestId('deviceRenameButton'))
    expect(await screen.findByText('Device renamed.')).toBeTruthy()
    const call = backend.calls.find((entry) => entry.method === 'renameDevice')
    expect(call?.args).toEqual(['dev-2', 'Office Mac'])
    expect(
      within((await screen.findAllByTestId('deviceRow'))[1]).getByText('Office Mac'),
    ).toBeTruthy()
  })

  it('signs another device out after a confirmation', async () => {
    const backend = mockBackend({
      getAccount: () => Promise.resolve(accountWithDevices()),
      revokeDevice: () => Promise.resolve(accountWithDevices([thisDevice])),
    })
    renderApp(backend)
    fireEvent.click((await screen.findAllByRole('tab'))[1])
    const row = (await screen.findAllByTestId('deviceRow'))[1]
    fireEvent.click(within(row).getByTestId('deviceSignOutButton'))
    expect(
      await screen.findByText('Work laptop is signed out on its next request. Its folders stay.'),
    ).toBeTruthy()
    fireEvent.click(screen.getByTestId('confirmDestructiveButton'))
    expect(await screen.findByText('Device signed out.')).toBeTruthy()
    expect(backend.calls.find((entry) => entry.method === 'revokeDevice')?.args).toEqual(['dev-2'])
  })

  it('changes the passphrase from Settings', async () => {
    const backend = mockBackend({ changePassphrase: () => Promise.resolve() })
    renderApp(backend)
    fireEvent.click((await screen.findAllByRole('tab'))[2])
    const button = (await screen.findByTestId('changePassphraseButton')) as HTMLButtonElement
    fireEvent.change(screen.getByTestId('currentPassphraseField'), { target: { value: 'old one' } })
    fireEvent.change(screen.getByTestId('newPassphraseField'), { target: { value: 'short' } })
    fireEvent.change(screen.getByTestId('newPassphraseConfirmField'), {
      target: { value: 'short' },
    })
    expect(button.disabled).toBe(true)
    fireEvent.change(screen.getByTestId('newPassphraseField'), {
      target: { value: 'correct horse battery' },
    })
    fireEvent.change(screen.getByTestId('newPassphraseConfirmField'), {
      target: { value: 'correct horse battery' },
    })
    fireEvent.click(button)
    expect(await screen.findByText('Passphrase changed.')).toBeTruthy()
    expect(backend.calls.find((entry) => entry.method === 'changePassphrase')?.args).toEqual([
      'old one',
      'correct horse battery',
    ])
  })
})

describe('the vault page (spec §15.2)', () => {
  it('renames the vault in place and re-reads the list', async () => {
    let name = 'Notes'
    const backend = mockBackend({
      listVaults: () => Promise.resolve([vault({ name })]),
      renameVault: (_vaultId: string, next: string) => {
        name = next
        return Promise.resolve()
      },
    })
    renderApp(backend)
    await screen.findByRole('heading', { name: 'Notes' })
    fireEvent.click(screen.getByTestId('renameVaultButton'))
    fireEvent.change(screen.getByTestId('renameVaultField'), { target: { value: 'Journal' } })
    fireEvent.click(screen.getByTestId('renameVaultButton'))
    expect(await screen.findByText('Renamed to Journal.')).toBeTruthy()
    expect(backend.calls.find((entry) => entry.method === 'renameVault')?.args).toEqual([
      'vault_1',
      'Journal',
    ])
    expect(await screen.findByRole('heading', { name: 'Journal' })).toBeTruthy()
  })

  it("shows this vault's activity on the page", async () => {
    const backend = mockBackend({
      listVaults: () => Promise.resolve([vault()]),
      listActivity: (vaultId: string | null) =>
        Promise.resolve(
          vaultId === 'vault_1'
            ? [{ at: 1_700_000_000, vault_id: 'vault_1', kind: 'uploaded' as const, path: 'a.md' }]
            : [],
        ),
    })
    renderApp(backend)
    const row = await screen.findByTestId('activityRow')
    expect(row.textContent).toContain('Uploaded a.md')
    expect(screen.getByRole('heading', { name: 'Activity' })).toBeTruthy()
  })

  it('offers Move folder only where the backend can move one', async () => {
    const backend = mockBackend({
      listVaults: () => Promise.resolve([vault()]),
      moveVaultFolder: () => Promise.resolve(),
    })
    renderApp(backend)
    await screen.findByRole('heading', { name: 'Notes' })
    fireEvent.click(screen.getByTestId('moveFolderButton'))
    fireEvent.change(screen.getByTestId('moveFolderField'), {
      target: { value: '/Users/me/Elsewhere' },
    })
    fireEvent.click(screen.getByTestId('moveFolderButton'))
    expect(await screen.findByText('Moved Notes.')).toBeTruthy()
    expect(backend.calls.find((entry) => entry.method === 'moveVaultFolder')?.args).toEqual([
      'vault_1',
      '/Users/me/Elsewhere',
    ])
    cleanup()
    renderApp(mockBackend({ listVaults: () => Promise.resolve([vault()]) }))
    await screen.findByRole('heading', { name: 'Notes' })
    expect(screen.queryByTestId('moveFolderButton')).toBeNull()
  })
})

describe('history (spec §8.2, §9.3)', () => {
  it('lists the versions of a picked file, previews one and restores it', async () => {
    const backend = mockBackend({
      listVaults: () => Promise.resolve([vault()]),
      listFiles: () => Promise.resolve(['a.md', 'notes/b.md']),
      listVersions: (_vaultId: string, path: string) =>
        Promise.resolve(
          path === 'a.md' ? [{ name: '1700000000', ts: 1_700_000_000, size: 120 }] : [],
        ),
      previewVersion: () => Promise.resolve({ text: '# the old text', size: 14 }),
      restoreVersion: () => Promise.resolve(),
    })
    renderApp(backend)
    const picker = await screen.findByTestId('historyFilePicker')
    expect(screen.getByText('Pick a file to see its versions.')).toBeTruthy()
    fireEvent.change(picker, { target: { value: 'notes/b.md' } })
    expect(await screen.findByText('No earlier versions kept.')).toBeTruthy()
    fireEvent.change(picker, { target: { value: 'a.md' } })
    const row = await screen.findByTestId('historyRow')
    expect(row.getAttribute('data-ts')).toBe('1700000000')
    fireEvent.click(within(row).getByTestId('previewButton'))
    expect(await screen.findByText('# the old text')).toBeTruthy()
    fireEvent.click(within(row).getByTestId('restoreButton'))
    expect(await screen.findByText('Restored a.md. Sync to upload it.')).toBeTruthy()
    expect(backend.calls.find((entry) => entry.method === 'restoreVersion')?.args).toEqual([
      'vault_1',
      'a.md',
      '1700000000',
    ])
  })

  it('lists recently deleted files with their paths and restores one', async () => {
    const backend = mockBackend({
      listVaults: () => Promise.resolve([vault()]),
      listTrash: () =>
        Promise.resolve([{ path: 'gone.md', hash: 'h', size: 40, deleted_at: 1_700_000_000 }]),
      previewTrash: () => Promise.resolve({ text: null, size: 40 }),
      restoreTrash: () => Promise.resolve(),
    })
    renderApp(backend)
    const row = await screen.findByTestId('trashRow')
    expect(row.getAttribute('data-path')).toBe('gone.md')
    fireEvent.click(within(row).getByTestId('previewButton'))
    expect(await screen.findByText('No preview for this file.')).toBeTruthy()
    fireEvent.click(within(row).getByTestId('restoreButton'))
    expect(await screen.findByText('Restored gone.md. Sync to upload it.')).toBeTruthy()
    expect(backend.calls.find((entry) => entry.method === 'restoreTrash')?.args).toEqual([
      'vault_1',
      'gone.md',
    ])
  })

  it('says when nothing was deleted', async () => {
    renderApp(mockBackend({ listVaults: () => Promise.resolve([vault()]) }))
    expect(await screen.findByText('Nothing deleted in the last 30 days.')).toBeTruthy()
  })
})
