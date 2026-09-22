import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
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
