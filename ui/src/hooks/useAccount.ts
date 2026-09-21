import { useCallback, useEffect, useRef, useState } from 'react'
import type { AccountState, AuthCapabilities, InviteInfo, VaultUsage } from '../types'
import { useBackend } from '../backend'
import { isInviteRequired, isUnauthorized, SESSION_EXPIRED, toCommandError } from '../lib/errors'

type Notify = (message: string) => void

// The account on the one server this build talks to: who is signed in, the
// devices and invites, and the sign-in form. A 401 anywhere marks the
// session expired (the bearer is already gone on the backend side).
export function useAccount(notify: Notify) {
  const backend = useBackend()
  const [serverUrl, setServerUrl] = useState('')
  const [account, setAccount] = useState<AccountState | null>(null)
  const [invites, setInvites] = useState<InviteInfo[]>([])
  // What `GET /` says about this server; null until it answered.
  const [capabilities, setCapabilities] = useState<AuthCapabilities | null>(null)
  const [sessionExpired, setSessionExpired] = useState(false)
  const [busy, setBusy] = useState(false)
  // Sign-in form.
  const [authEmail, setAuthEmail] = useState('')
  const [authCode, setAuthCode] = useState('')
  const [inviteCode, setInviteCode] = useState('')
  const [codeSent, setCodeSent] = useState(false)
  // The server refused a sign-up without an invite: show the field even when
  // capabilities said none was needed (they can go stale).
  const [inviteForced, setInviteForced] = useState(false)
  const [inviteFocusAt, setInviteFocusAt] = useState(0)
  const notifyRef = useRef(notify)
  notifyRef.current = notify

  const markExpired = useCallback(() => {
    setSessionExpired(true)
    setAccount({ kind: 'signed_out' })
    setInvites([])
  }, [])

  // Every failure lands here; callers outside the hook use it too.
  const fail = useCallback(
    (error: unknown) => {
      const failure = toCommandError(error)
      if (isUnauthorized(failure)) {
        markExpired()
        notifyRef.current(SESSION_EXPIRED)
      } else {
        notifyRef.current(failure.message)
      }
    },
    [markExpired],
  )

  const refreshInvites = useCallback(async () => {
    try {
      setInvites(await backend.listInvites())
    } catch (error) {
      fail(error)
    }
  }, [backend, fail])

  const refresh = useCallback(async () => {
    try {
      const next = await backend.getAccount()
      setAccount(next)
      if (next.kind === 'account') {
        setSessionExpired(false)
        await refreshInvites()
      } else {
        setInvites([])
      }
    } catch (error) {
      fail(error)
    }
  }, [backend, fail, refreshInvites])

  const refreshCapabilities = useCallback(async () => {
    try {
      setCapabilities(await backend.getAuthCapabilities())
    } catch (error) {
      setCapabilities(null)
      fail(error)
    }
  }, [backend, fail])

  useEffect(() => {
    void backend.getServerUrl().then(setServerUrl)
    void refresh()
    void refreshCapabilities()
    // Devices, invites and usage change from other devices and after syncs.
    return backend.on('state://changed', () => void refresh())
  }, [backend, refresh, refreshCapabilities])

  async function withBusy(action: () => Promise<void>) {
    setBusy(true)
    try {
      await action()
    } finally {
      setBusy(false)
    }
  }

  const sendCode = () =>
    withBusy(async () => {
      try {
        const devCode = await backend.authEmailStart(authEmail)
        setCodeSent(true)
        if (devCode) {
          setAuthCode(devCode)
          notifyRef.current('Dev server returned the code inline.')
        } else {
          notifyRef.current(`Sent a 6-digit code to ${authEmail}.`)
        }
      } catch (error) {
        fail(error)
      }
    })

  const verifyCode = () =>
    withBusy(async () => {
      try {
        const next = await backend.authEmailVerify(authEmail, authCode, inviteCode.trim() || null)
        setAccount(next)
        setSessionExpired(false)
        setCodeSent(false)
        setAuthCode('')
        setInviteCode('')
        setInviteForced(false)
        void refreshInvites()
        notifyRef.current(
          next.kind === 'account' ? `Signed in as ${next.email ?? next.user_id}.` : 'Signed in.',
        )
      } catch (error) {
        const failure = toCommandError(error)
        if (isInviteRequired(failure)) {
          setInviteForced(true)
          setInviteFocusAt(Date.now())
          notifyRef.current('Enter the invite code you were given.')
        } else {
          fail(failure)
        }
      }
    })

  const createInvite = () =>
    withBusy(async () => {
      try {
        await backend.createInvite()
        await refreshInvites()
        notifyRef.current('Invite code created.')
      } catch (error) {
        fail(error)
      }
    })

  const copyInvite = async (code: string) => {
    try {
      await navigator.clipboard.writeText(code)
      notifyRef.current('Invite code copied.')
    } catch (error) {
      fail(error)
    }
  }

  const revokeDevice = (sessionId: string) =>
    withBusy(async () => {
      try {
        setAccount(await backend.revokeSession(sessionId))
        notifyRef.current('Device signed out.')
      } catch (error) {
        fail(error)
      }
    })

  const signOut = () =>
    withBusy(async () => {
      try {
        await backend.signOut()
        setAccount({ kind: 'signed_out' })
        setInvites([])
        notifyRef.current(`Signed out of ${serverUrl}.`)
      } catch (error) {
        fail(error)
      }
    })

  // Returns whether it succeeded so the confirmation form knows to close.
  const deleteAccount = async (): Promise<boolean> => {
    setBusy(true)
    try {
      await backend.deleteAccount()
      setAccount({ kind: 'signed_out' })
      setInvites([])
      notifyRef.current('Account deleted.')
      return true
    } catch (error) {
      fail(error)
      return false
    } finally {
      setBusy(false)
    }
  }

  // Usage for one vault, when the account knows it.
  const vaultUsage = (vaultId: string): VaultUsage | null => {
    if (account?.kind !== 'account' || !account.usage) return null
    const entry = account.usage.vaults.find((vault) => vault.id === vaultId)
    if (!entry) return null
    return { bytes: entry.bytes, max: account.usage.max_vault_bytes }
  }

  return {
    serverUrl,
    account,
    invites,
    capabilities,
    sessionExpired,
    busy,
    fail,
    refresh,
    signedIn: account?.kind === 'account',
    inviteRequired: (capabilities?.invite_required ?? false) || inviteForced,
    inviteFocusAt,
    form: { authEmail, authCode, inviteCode, codeSent },
    setAuthEmail,
    setAuthCode,
    setInviteCode,
    changeEmail: () => setCodeSent(false),
    sendCode,
    verifyCode,
    createInvite,
    copyInvite,
    revokeDevice,
    signOut,
    deleteAccount,
    vaultUsage,
  }
}

export type Account = ReturnType<typeof useAccount>
