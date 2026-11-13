import React, { useEffect, useState } from 'react'
import { createRoot } from 'react-dom/client'
import { sha256 } from '@noble/hashes/sha256'
import { bytesToHex } from '@noble/hashes/utils'
import './style.css'

type BrowserSession = { user: { username: string }, csrfToken: string }
type UploadSession = { id: string, file: { fileId: string }, phase: string }
type FinalizationResult = { phase: string, releaseId?: string, artifactId?: string, problem?: { title: string } }

async function request<T>(path: string, options: RequestInit = {}): Promise<{ value: T, response: Response }> {
  const response = await fetch(`/api/v1${path}`, { credentials: 'same-origin', ...options })
  if (!response.ok) {
    const body = await response.json().catch(() => ({})) as { title?: string }
    throw new Error(body.title || `Request failed (${response.status})`)
  }
  return { value: await response.json() as T, response }
}

async function fileSha256(file: File): Promise<string> {
  const hash = sha256.create()
  const chunk = 4 * 1024 * 1024
  for (let offset = 0; offset < file.size; offset += chunk) {
    hash.update(new Uint8Array(await file.slice(offset, offset + chunk).arrayBuffer()))
  }
  return bytesToHex(hash.digest())
}

function App() {
  const [health, setHealth] = useState('Checking server')
  const [session, setSession] = useState<BrowserSession | null>(null)
  const [username, setUsername] = useState('admin')
  const [password, setPassword] = useState('')
  const [appName, setAppName] = useState('')
  const [file, setFile] = useState<File | null>(null)
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState('')

  useEffect(() => {
    fetch('/api/v1/health/ready').then(async response => {
      const body = await response.json() as { status: string }
      setHealth(response.ok && body.status === 'Ready' ? 'Server ready' : 'Server unavailable')
    }).catch(() => setHealth('Server unavailable'))
    request<BrowserSession>('/auth/sessions/current').then(({ value }) => setSession(value)).catch(() => {})
  }, [])

  async function login(event: React.FormEvent) {
    event.preventDefault()
    setBusy(true); setMessage('')
    try {
      const { value } = await request<BrowserSession>('/auth/sessions', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username, password })
      })
      setSession(value); setPassword('')
    } catch (error) { setMessage((error as Error).message) }
    finally { setBusy(false) }
  }

  async function publish(file: File, name: string, csrf: string, publishNow: boolean) {
    const headers = { 'X-CSRF-Token': csrf }
    const digest = await fileSha256(file)
    const created = await request<UploadSession>('/uploads', {
      method: 'POST', headers: { ...headers, 'Content-Type': 'application/json', 'Idempotency-Key': crypto.randomUUID() },
      body: JSON.stringify({ mode: 'BareFile', target: { newAppName: name }, file: { size: file.size, sha256: digest } })
    })
    const upload = created.value
    const chunk = 4 * 1024 * 1024
    for (let start = 0; start < file.size; start += chunk) {
      const end = Math.min(start + chunk, file.size)
      const response = await fetch(`/api/v1/uploads/${upload.id}/files/${upload.file.fileId}`, {
        method: 'PUT', credentials: 'same-origin',
        headers: { ...headers, 'Content-Type': 'application/octet-stream', 'Content-Range': `bytes ${start}-${end - 1}/${file.size}` },
        body: file.slice(start, end)
      })
      if (!response.ok) throw new Error(`Upload failed (${response.status})`)
      setMessage(`Received ${end.toLocaleString()} of ${file.size.toLocaleString()} bytes`)
    }
    const { value: result } = await request<FinalizationResult>(`/uploads/${upload.id}/finalize`, {
      method: 'POST', headers: { ...headers, 'Idempotency-Key': crypto.randomUUID() }
    })
    if (result.phase !== 'Completed' || !result.releaseId) throw new Error(result.problem?.title || 'Finalization did not complete')
    if (publishNow) {
      const release = await request<unknown>(`/releases/${result.releaseId}`)
      await request<unknown>(`/releases/${result.releaseId}/publish`, {
        method: 'POST', headers: { ...headers, 'Idempotency-Key': crypto.randomUUID(), 'If-Match': release.response.headers.get('ETag') || '' }
      })
      setMessage(`Published ${name}`)
    } else setMessage(`Saved ${name} as Draft`)
  }

  async function submit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!file || !session || !appName.trim()) return
    const button = (event.nativeEvent as SubmitEvent).submitter as HTMLButtonElement | null
    const publishNow = button?.value === 'publish'
    setBusy(true); setMessage('Hashing file')
    try { await publish(file, appName.trim(), session.csrfToken, publishNow) }
    catch (error) { setMessage((error as Error).message) }
    finally { setBusy(false) }
  }

  return <main>
    <header><h1>App Depot</h1><p role="status">{health}</p></header>
    {!session ? <section><h2>Sign in</h2><form onSubmit={login}>
      <label>Username<input value={username} onChange={event => setUsername(event.target.value)} required /></label>
      <label>Password<input type="password" value={password} onChange={event => setPassword(event.target.value)} required /></label>
      <button disabled={busy}>Sign in</button>
    </form></section> : <section><p>Signed in as {session.user.username}</p><h2>New App</h2><form onSubmit={submit}>
      <label>App name<input value={appName} onChange={event => setAppName(event.target.value)} required /></label>
      <label>File<input type="file" onChange={event => setFile(event.target.files?.[0] || null)} required /></label>
      <div className="actions"><button value="draft" disabled={busy || !file}>Save Draft</button><button value="publish" disabled={busy || !file}>Upload and publish</button></div>
    </form></section>}
    {message && <p role="alert">{message}</p>}
  </main>
}

createRoot(document.getElementById('root')!).render(<React.StrictMode><App /></React.StrictMode>)
