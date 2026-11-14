import { useEffect, useRef, useState } from 'react'
import { apiRequest, type TokenIssue, type TokenMetadata } from './api'
import { LoadingState, PageHeader, ProblemNotice, TechnicalDetails } from './components'
import { ConfirmDialog } from './management'
import { useAuth } from './portal'
import { useResource } from './use-resource'

export function AccountPage() {
  const { state, signedOut } = useAuth()
  const metadata = useResource('personal-token', signal => apiRequest<TokenMetadata>('/me/token', { authenticated: true, signal }).then(result => result.value))
  const [token, setToken] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const [notice, setNotice] = useState('')
  const tokenField = useRef<HTMLTextAreaElement>(null)
  useEffect(() => () => { if (tokenField.current) tokenField.current.value = '' }, [])
  async function issue() {
    setToken(''); setBusy(true); setError(undefined); setNotice('')
    try {
      const result = await apiRequest<TokenIssue>('/me/token', { method: 'PUT', authenticated: true, csrf: state.session!.csrfToken,
        headers: { 'Content-Type': 'application/json' }, body: '{}' })
      setToken(result.value.token); metadata.retry(); setNotice('Token issued. Copy it now; it cannot be retrieved again.')
    } catch (failure) { setError(failure); throw failure } finally { setBusy(false) }
  }
  async function revoke() {
    setToken(''); setBusy(true); setError(undefined); setNotice('')
    try {
      await apiRequest<void>('/me/token', { method: 'DELETE', authenticated: true, csrf: state.session!.csrfToken })
      metadata.retry(); setNotice('Personal API token revoked.')
    } catch (failure) { setError(failure); throw failure } finally { setBusy(false) }
  }
  function copy() {
    const field = tokenField.current
    if (!field) return
    field.focus(); field.select()
    setNotice(document.execCommand('copy') ? 'Token copied. Keep it private.' : 'Copy failed. Select the token and copy it manually.')
  }
  async function signOut() {
    setToken(''); setError(undefined)
    try { await apiRequest<void>('/auth/sessions/current', { method: 'DELETE', authenticated: true, csrf: state.session!.csrfToken }); signedOut() }
    catch (failure) { setError(failure) }
  }
  const user = state.session!.user
  return <><PageHeader title="Account" description="Your signed-in local account and personal API token." />
    <section className="panel"><h2>{user.username}</h2><p>{user.serverAdmin ? 'Server Admin' : 'Member'} · Publish permission: {user.publishPermission ? 'Allowed' : 'Not allowed'}</p>
      <TechnicalDetails><code>User ID: {user.id}</code></TechnicalDetails></section>
    <section className="panel form-panel"><h2>Personal API token</h2>{metadata.loading ? <LoadingState /> : metadata.error ? <ProblemNotice error={metadata.error} retry={metadata.retry} /> : <>
      <p>Status: <strong>{metadata.value!.active ? 'Active' : 'No active token'}</strong>{metadata.value!.issuedAt ? ` · Issued ${new Date(metadata.value!.issuedAt).toLocaleString()}` : ''}</p>
      <p>One token is active at a time. Replacing it immediately invalidates the previous token. A newly issued token is shown only once. Keep it out of logs, URLs and shared files.</p>
      <p className="subtle">HTTP on a trusted LAN has no transport confidentiality. Do not send personal tokens across untrusted networks without HTTPS.</p>
      <div className="form-actions"><ConfirmDialog label={metadata.value!.active ? 'Replace token' : 'Issue token'} detail={metadata.value!.active ? 'Issue a new personal token and immediately invalidate the current one? The new secret is shown once.' : 'Issue a personal API token? The secret is shown once and cannot be retrieved later.'} confirm={metadata.value!.active ? 'Replace token' : 'Issue token'} onConfirm={issue} />
        {metadata.value!.active && <ConfirmDialog label="Revoke my token" detail="Immediately invalidate your personal API token? Browser sessions remain signed in." confirm="Revoke token" onConfirm={revoke} />}</div>
      {token && <div className="one-time-token"><label>New token (shown once)<textarea ref={tokenField} readOnly value={token} rows={3} spellCheck={false} /></label>
        <div className="form-actions"><button className="button button-secondary" onClick={copy}>Copy token</button><button className="button button-secondary" onClick={() => { setToken(''); setNotice('Token display cleared.') }}>Dismiss token</button></div></div>}
      {error !== undefined && <ProblemNotice error={error} />}{notice && <p className="success-notice" role="status">{notice}</p>}
    </>}</section>
    <section className="panel"><h2>Session</h2><p>Signing out invalidates this browser session.</p><button className="button button-secondary" onClick={() => void signOut()} disabled={busy}>Sign out</button></section>
  </>
}
