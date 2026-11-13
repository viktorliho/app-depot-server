import { createContext, useCallback, useContext, useEffect, useRef, useState, type FormEvent, type ReactNode } from 'react'
import { Link, NavLink, Navigate, Outlet, useLocation, useNavigate, useParams } from 'react-router'
import { apiRequest, type BrowserSession, type DepotApp, ApiError } from './api'
import { ActionLink, LoadingState, PageHeader, ProblemNotice, StatusBadge } from './components'
import { useResource } from './use-resource'

type AuthState = { phase: 'checking' | 'setup' | 'guest' | 'authenticated' | 'not-ready' | 'error'; session?: BrowserSession; error?: unknown }
const AuthContext = createContext<{ state: AuthState; refresh: () => void; signedIn: (session: BrowserSession) => void; signedOut: () => void } | null>(null)

export function AuthProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<AuthState>({ phase: 'checking' })
  const [revision, setRevision] = useState(0)
  const refresh = useCallback(() => { setState({ phase: 'checking' }); setRevision(value => value + 1) }, [])
  useEffect(() => {
    let active = true
    async function restore() {
      try {
        await apiRequest<{ status: string }>('/health/ready')
        const setup = await apiRequest<{ required: boolean }>('/setup')
        if (setup.value.required) { if (active) setState({ phase: 'setup' }); return }
        try {
          const session = await apiRequest<BrowserSession>('/auth/sessions/current')
          if (active) setState({ phase: 'authenticated', session: session.value })
        } catch (error) {
          if (active) setState(error instanceof ApiError && error.status === 401 ? { phase: 'guest' } : { phase: 'error', error })
        }
      } catch (error) {
        if (active) setState(error instanceof ApiError && error.status === 503 ? { phase: 'not-ready' } : { phase: 'error', error })
      }
    }
    void restore()
    return () => { active = false }
  }, [revision])
  useEffect(() => {
    const unauthorized = () => setState({ phase: 'guest' })
    window.addEventListener('app-depot-unauthorized', unauthorized)
    return () => window.removeEventListener('app-depot-unauthorized', unauthorized)
  }, [])
  return <AuthContext.Provider value={{ state, refresh, signedIn: session => setState({ phase: 'authenticated', session }), signedOut: () => setState({ phase: 'guest' }) }}>{children}</AuthContext.Provider>
}

export function useAuth() {
  const value = useContext(AuthContext)
  if (!value) throw new Error('AuthProvider is required')
  return value
}

type Theme = 'light' | 'dark'
const ThemeContext = createContext<{ theme: Theme; toggle: () => void } | null>(null)

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setTheme] = useState<Theme>(() => {
    try {
      const saved = localStorage.getItem('app-depot-theme')
      if (saved === 'light' || saved === 'dark') return saved
    } catch { /* Storage can be disabled by the browser. */ }
    return matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
  })
  useEffect(() => { document.documentElement.dataset.theme = theme }, [theme])
  const toggle = () => setTheme(current => {
    const next = current === 'light' ? 'dark' : 'light'
    try { localStorage.setItem('app-depot-theme', next) } catch { /* The current tab still keeps the preference. */ }
    return next
  })
  return <ThemeContext.Provider value={{ theme, toggle }}>{children}</ThemeContext.Provider>
}

function ThemeButton() {
  const value = useContext(ThemeContext)
  if (!value) throw new Error('ThemeProvider is required')
  return <button className="text-button" type="button" onClick={value.toggle} aria-label={`Switch to ${value.theme === 'light' ? 'dark' : 'light'} theme`}>{value.theme === 'light' ? 'Dark theme' : 'Light theme'}</button>
}

function RecoveryState({ error, retry }: { error?: unknown; retry: () => void }) {
  return <div className="auth-screen"><div className="auth-card"><h1>App Depot</h1>{error ? <ProblemNotice error={error} retry={retry} /> : <><h2>Server not ready</h2><p>The server may be starting or recovering. Try again when it is ready.</p><button className="button button-primary" onClick={retry}>Check again</button></>}</div></div>
}

export function RequireAuth({ children }: { children: ReactNode }) {
  const { state, refresh } = useAuth()
  const location = useLocation()
  if (state.phase === 'checking') return <div className="boot"><LoadingState label="Opening App Depot…" /></div>
  if (state.phase === 'not-ready') return <RecoveryState retry={refresh} />
  if (state.phase === 'error') return <RecoveryState error={state.error} retry={refresh} />
  if (state.phase === 'setup') return <Navigate to="/setup" replace state={{ from: location.pathname + location.search }} />
  if (state.phase === 'guest') return <Navigate to="/login" replace state={{ from: location.pathname + location.search }} />
  return <>{children}</>
}

export function AuthPage({ setup }: { setup: boolean }) {
  const { state, refresh, signedIn } = useAuth()
  const navigate = useNavigate()
  const location = useLocation()
  const from = (location.state as { from?: string } | null)?.from || '/'
  const [username, setUsername] = useState('admin')
  const [password, setPassword] = useState('')
  const [token, setToken] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  const [notice, setNotice] = useState('')
  if (state.phase === 'checking') return <div className="boot"><LoadingState /></div>
  if (state.phase === 'not-ready' || state.phase === 'error') return <RecoveryState error={state.error} retry={refresh} />
  if (state.phase === 'authenticated') return <Navigate to={from} replace />
  if (setup && state.phase !== 'setup') return <Navigate to="/login" replace state={{ from }} />
  if (!setup && state.phase === 'setup') return <Navigate to="/setup" replace state={{ from }} />
  async function submit(event: FormEvent) {
    event.preventDefault(); setBusy(true); setError(undefined); setNotice('')
    try {
      if (setup) {
        await apiRequest('/setup', { method: 'POST', headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ token, username: username.trim(), password }) })
        setToken(''); setPassword(''); setNotice('Admin created. Sign in with your new password.')
        refresh()
        navigate('/login', { replace: true, state: { from, notice: 'Admin created. Sign in with your new password.' } })
      } else {
        const result = await apiRequest<BrowserSession>('/auth/sessions', { method: 'POST', headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ username: username.trim(), password }) })
        setPassword(''); signedIn(result.value); navigate(from, { replace: true })
      }
    } catch (failure) { setError(failure) }
    finally { setBusy(false) }
  }
  return <div className="auth-screen"><div className="auth-card"><div className="auth-top"><strong>App Depot Portal</strong><ThemeButton /></div>
    <h1>{setup ? 'Initial Admin setup' : 'Sign in'}</h1>
    {setup && <p>Find the one-time setup token in the container startup logs.</p>}
    {(notice || (location.state as { notice?: string } | null)?.notice) && <p className="success-notice" role="status">{notice || (location.state as { notice?: string }).notice}</p>}
    {error !== undefined && <ProblemNotice error={error} />}
    <form onSubmit={submit} className="form-stack">
      {setup && <label>Setup token<input autoComplete="off" value={token} onChange={event => setToken(event.target.value)} required /></label>}
      <label>Username<input autoComplete="username" value={username} onChange={event => setUsername(event.target.value)} required /></label>
      <label>Password{setup && ' (at least 16 characters)'}<input type="password" autoComplete={setup ? 'new-password' : 'current-password'} minLength={setup ? 16 : undefined} value={password} onChange={event => setPassword(event.target.value)} required /></label>
      <button className="button button-primary" disabled={busy}>{busy ? 'Please wait…' : setup ? 'Create Admin' : 'Sign in'}</button>
    </form></div></div>
}

export function AppShell({ app, children }: { app?: DepotApp; children: ReactNode }) {
  const { state, signedOut } = useAuth()
  const { pathname } = useLocation()
  const [menuOpen, setMenuOpen] = useState(false)
  const [compact, setCompact] = useState(() => matchMedia('(max-width: 900px)').matches)
  const [logoutError, setLogoutError] = useState<unknown>()
  const menuButton = useRef<HTMLButtonElement>(null)
  const sidebar = useRef<HTMLElement>(null)
  const previousPath = useRef(pathname)
  const session = state.session!
  const canCreate = session.user.publishPermission || session.user.serverAdmin
  useEffect(() => {
    const media = matchMedia('(max-width: 900px)')
    const update = () => setCompact(media.matches)
    media.addEventListener('change', update)
    return () => media.removeEventListener('change', update)
  }, [])
  useEffect(() => {
    if (previousPath.current !== pathname) {
      previousPath.current = pathname
      if (menuOpen) queueMicrotask(() => document.getElementById('main-content')?.focus())
      setMenuOpen(false)
    }
  }, [pathname])
  useEffect(() => {
    if (compact && menuOpen) sidebar.current?.querySelector<HTMLAnchorElement>('a')?.focus()
  }, [compact, menuOpen])
  useEffect(() => {
    if (!menuOpen) return
    const escape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') { setMenuOpen(false); menuButton.current?.focus() }
    }
    document.addEventListener('keydown', escape)
    return () => document.removeEventListener('keydown', escape)
  }, [menuOpen])
  async function logout() {
    try { await apiRequest('/auth/sessions/current', { method: 'DELETE', csrf: session.csrfToken, authenticated: true }); signedOut() }
    catch (error) { setLogoutError(error) }
  }
  const links = app ? [{ to: `/apps/${app.id}`, label: 'Overview', end: true }, { to: `/apps/${app.id}/releases`, label: 'Releases', end: false }]
    : [{ to: '/', label: 'Overview', end: true }, { to: '/apps', label: 'Applications', end: false }]
  return <div className="app-shell">
    <a className="skip-link" href="#main-content">Skip to content</a>
    <aside ref={sidebar} inert={compact && !menuOpen} className={`sidebar ${menuOpen ? 'sidebar-open' : ''}`}>
      <Link className="brand" to="/">App Depot <span>Portal</span></Link>
      <div className="workspace-switch">{app ? <><span className="eyebrow">APPLICATION WORKSPACE</span><strong title={app.name}>{app.name}</strong><Link to="/apps">← Switch application</Link></> : <><span className="eyebrow">SERVER WORKSPACE</span><strong>Server</strong></>}</div>
      <nav aria-label={app ? 'Application navigation' : 'Server navigation'}>{links.map(link => <NavLink key={link.to} to={link.to} end={link.end} className={({ isActive }) => `nav-link ${isActive ? 'nav-link-active' : ''}`}>{link.label}</NavLink>)}</nav>
      {!app && canCreate && <div className="sidebar-bottom"><Link to="/upload/new-app">Upload a new application</Link></div>}
    </aside>
    {menuOpen && <button className="nav-scrim" aria-label="Close navigation" onClick={() => { setMenuOpen(false); menuButton.current?.focus() }} />}
    <div className="main-column"><header className="topbar"><button ref={menuButton} className="menu-button" aria-label="Open navigation" aria-expanded={menuOpen} onClick={() => setMenuOpen(value => !value)}>☰</button>
      <div className="topbar-context">{app ? <><Link to="/apps">Applications</Link><span aria-hidden="true"> / </span><span>{app.name}</span></> : 'Server workspace'}</div>
      <div className="topbar-actions"><ThemeButton /><details className="account-menu"><summary>{session.user.username}</summary><div><span>{session.user.serverAdmin ? 'Server Admin' : 'Member'}</span><button onClick={() => void logout()}>Sign out</button></div></details></div></header>
      <main id="main-content" tabIndex={-1} className="content">{logoutError !== undefined && <ProblemNotice error={logoutError} />}{children}</main>
    </div>
  </div>
}

export function ServerWorkspace() { return <AppShell><Outlet /></AppShell> }

export function AppWorkspace() {
  const { appId } = useParams()
  const app = useResource(`app:${appId}`, signal => apiRequest<DepotApp>(`/apps/${appId}`, { signal, authenticated: true }).then(result => result.value))
  if (app.loading) return <div className="boot"><LoadingState label="Opening application…" /></div>
  if (app.error) return <AppShell><PageHeader title="Application unavailable" /><ProblemNotice error={app.error} retry={app.retry} /><ActionLink to="/apps" secondary>Back to applications</ActionLink></AppShell>
  return <AppShell app={app.value}><Outlet context={app.value} /></AppShell>
}

export function NotFound() { return <AppShell><PageHeader title="Page not found" description="This portal page is unavailable." /><ActionLink to="/apps" secondary>Open applications</ActionLink></AppShell> }

export function AppIdentity({ app }: { app: DepotApp }) { return <div className="identity-line"><StatusBadge value={app.state} /><StatusBadge value={app.visibility} /></div> }
