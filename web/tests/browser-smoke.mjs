import { chromium } from 'playwright-core'
import { randomUUID } from 'node:crypto'
import { execFileSync, spawn } from 'node:child_process'
import { mkdtemp, rm } from 'node:fs/promises'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

async function startLanPreview() {
  const repository = fileURLToPath(new URL('../..', import.meta.url))
  const binary = join(repository, 'target/debug/app-depot-server')
  const namespace = await mkdtemp(join(tmpdir(), 'app-depot-lan-smoke-'))
  const listener = createServer()
  await new Promise(resolve => listener.listen(0, '127.0.0.1', resolve))
  const port = listener.address().port
  await new Promise(resolve => listener.close(resolve))
  let server
  try {
    execFileSync(binary, ['init-preview', '--namespace', namespace, '--instance', 'preview'])
    server = spawn(binary, ['serve', '--namespace', namespace, '--instance', 'preview', '--mode', 'preview',
      '--bind', `127.0.0.1:${port}`, '--assets', join(repository, 'web/dist')], { stdio: ['ignore', 'pipe', 'pipe'] })
    let output = ''
    let errors = ''
    server.stderr.on('data', chunk => { errors = (errors + chunk.toString()).slice(-2000) })
    let timeoutId
    const token = await Promise.race([
      new Promise((resolve, reject) => {
        server.stdout.on('data', chunk => {
          output += chunk.toString()
          for (const line of output.split('\n').slice(0, -1)) {
            try {
              const event = JSON.parse(line)
              if (event.event === 'initial_setup') resolve(event.token)
            } catch { /* Other startup output is not the setup event. */ }
          }
          output = output.slice(output.lastIndexOf('\n') + 1)
        })
        server.once('exit', code => reject(new Error(`Preview exited ${code}: ${errors}`)))
      }),
      new Promise((_, reject) => { timeoutId = setTimeout(() => reject(new Error(`Setup token timeout: ${errors}`)), 15000) })
    ]).finally(() => clearTimeout(timeoutId))
    const deadline = Date.now() + 15000
    while (Date.now() < deadline) {
      try {
        const response = await fetch(`http://127.0.0.1:${port}/api/v1/health/ready`)
        if (response.ok) return { namespace, server, token, base: `http://app-depot.test:${port}/`, binary, port, repository }
      } catch { /* Startup is still in progress. */ }
      await new Promise(resolve => setTimeout(resolve, 100))
    }
    throw new Error(`Preview readiness timeout: ${errors}`)
  } catch (error) {
    if (server && server.exitCode === null) {
      server.kill('SIGTERM')
      await new Promise(resolve => server.once('exit', resolve))
    }
    await rm(namespace, { recursive: true, force: true })
    throw error
  }
}

async function restartLanPreview(preview) {
  preview.server.kill('SIGTERM')
  await new Promise(resolve => preview.server.once('exit', resolve))
  preview.server = spawn(preview.binary, ['serve', '--namespace', preview.namespace, '--instance', 'preview', '--mode', 'preview',
    '--bind', `127.0.0.1:${preview.port}`, '--assets', join(preview.repository, 'web/dist')], { stdio: ['ignore', 'pipe', 'pipe'] })
  const deadline = Date.now() + 15000
  while (Date.now() < deadline) {
    if (preview.server.exitCode !== null) throw new Error(`Preview restart exited ${preview.server.exitCode}`)
    try {
      const response = await fetch(`http://127.0.0.1:${preview.port}/api/v1/health/ready`)
      if (response.ok) return
    } catch { /* Restart is still in progress. */ }
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error('Preview restart readiness timeout')
}

const selfHosted = process.env.APP_DEPOT_SELF_HOSTED === '1'
const preview = selfHosted ? await startLanPreview() : null
const browserPath = process.env.APP_DEPOT_BROWSER_BINARY ||
  (process.platform === 'darwin' ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : '/usr/bin/google-chrome')
const setupToken = preview?.token || process.env.APP_DEPOT_SETUP_TOKEN
const password = process.env.APP_DEPOT_TEST_PASSWORD || 'browser smoke password 12345'
const base = preview?.base || process.env.APP_DEPOT_BASE_URL || 'http://127.0.0.1:8787/'
const insecureOrigin = selfHosted || process.env.APP_DEPOT_EXPECT_INSECURE_ORIGIN === '1'
let browser
try {
  browser = await chromium.launch({ executablePath: browserPath, headless: true,
    args: insecureOrigin ? ['--host-resolver-rules=MAP app-depot.test 127.0.0.1', '--no-proxy-server'] : [] })
  const page = await browser.newPage()
  await page.goto(base)
  const context = await page.evaluate(() => ({ secure: isSecureContext, randomUuid: typeof crypto.randomUUID,
    randomValues: typeof crypto.getRandomValues }))
  if (insecureOrigin && (context.secure || context.randomUuid !== 'undefined' || context.randomValues !== 'function')) {
    throw new Error(`Expected insecure LAN HTTP browser context, got ${JSON.stringify(context)}`)
  }
  const keys = []
  page.on('request', request => {
    if (request.method() === 'POST' && /\/api\/v1\/(uploads(?:\/[^/]+\/finalize)?|releases\/[^/]+\/publish)$/.test(new URL(request.url()).pathname)) {
      keys.push(request.headers()['idempotency-key'])
    }
  })
  if (setupToken) {
    await page.getByRole('heading', { name: 'Initial Admin setup' }).waitFor()
    if (await page.locator('.sidebar').count()) throw new Error('Setup displayed the authenticated sidebar')
    await page.getByLabel('Setup token').fill(setupToken)
    await page.getByLabel('Password (at least 16 characters)').fill(password)
    await page.getByRole('button', { name: 'Create Admin' }).click()
    await page.getByText('Admin created. Sign in with your new password.').waitFor()
  }
  await page.getByRole('heading', { name: 'Sign in' }).waitFor()
  await page.getByLabel('Username').fill('admin')
  await page.getByLabel('Password').fill(password)
  await page.getByRole('button', { name: 'Sign in' }).click()
  await page.getByRole('heading', { name: 'Server overview' }).waitFor()
  await page.getByRole('heading', { name: 'No applications yet' }).first().waitFor()
  if (preview) await restartLanPreview(preview)
  await page.reload()
  await page.getByRole('heading', { name: 'Server overview' }).waitFor()
  await page.getByRole('button', { name: 'Switch to dark theme' }).click()
  if (await page.locator('html').getAttribute('data-theme') !== 'dark') throw new Error('Dark theme did not activate')
  await page.reload()
  await page.getByRole('heading', { name: 'Server overview' }).waitFor()
  if (await page.locator('html').getAttribute('data-theme') !== 'dark') throw new Error('Theme preference was not restored')
  await page.getByRole('button', { name: 'Switch to light theme' }).click()
  await page.getByRole('navigation', { name: 'Server navigation' }).getByRole('link', { name: 'Applications' }).click()
  await page.getByRole('heading', { name: 'Applications', exact: true }).waitFor()
  await page.getByRole('heading', { name: 'No applications yet' }).waitFor()
  await page.getByRole('link', { name: 'Create application' }).first().click()
  await page.getByLabel('Application name').fill('Portal Draft')
  await page.getByRole('button', { name: 'Create Draft application' }).click()
  await page.getByRole('heading', { name: 'Portal Draft' }).waitFor()
  const draftId = new URL(page.url()).pathname.split('/').pop()
  if (!/^[0-9a-f-]{36}$/.test(draftId)) throw new Error(`Unexpected application route ${page.url()}`)
  await page.getByText('Draft', { exact: true }).first().waitFor()
  await page.getByText('Private', { exact: true }).first().waitFor()
  await page.getByText('No releases yet').waitFor()
  await page.getByText('Technical details').click()
  await page.getByText(draftId).waitFor()
  await page.getByRole('navigation', { name: 'Application navigation' }).getByRole('link', { name: 'Releases' }).click()
  await page.getByRole('heading', { name: 'Releases', exact: true }).waitFor()
  await page.goBack()
  await page.getByRole('heading', { name: 'Portal Draft' }).waitFor()
  await page.goForward()
  await page.getByRole('heading', { name: 'Releases', exact: true }).waitFor()
  await page.goto(`${base}apps/${draftId}`)
  await page.getByRole('heading', { name: 'Portal Draft' }).waitFor()
  await page.reload()
  await page.getByRole('heading', { name: 'Portal Draft' }).waitFor()
  await page.getByRole('link', { name: 'Add release from file' }).first().click()
  await page.getByRole('heading', { name: 'Add release from file' }).waitFor()
  await page.getByLabel('File').setInputFiles({ name: 'draft.txt', mimeType: 'text/plain', buffer: Buffer.from('Draft') })
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText('Saved Draft for Portal Draft').waitFor({ timeout: 30000 })
  await page.goto(`${base}apps`)
  await page.getByRole('heading', { name: 'Applications', exact: true }).waitFor()
  const createKeys = Array.from({ length: 11 }, () => randomUUID())
  await page.evaluate(async createKeys => {
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    for (let index = 0; index < createKeys.length; index++) {
      const response = await fetch('/api/v1/apps', { method: 'POST', headers: { 'Content-Type': 'application/json',
        'X-CSRF-Token': session.csrfToken, 'Idempotency-Key': createKeys[index] },
        body: JSON.stringify({ name: `Pagination App ${index}` }) })
      if (response.status !== 201) throw new Error(`App fixture creation failed: ${response.status}`)
    }
  }, createKeys)
  await page.reload()
  await page.getByRole('button', { name: 'Next page' }).waitFor()
  if (!(await page.getByRole('button', { name: 'Next page' }).isEnabled())) throw new Error('Cursor pagination missing')
  await page.getByRole('button', { name: 'Next page' }).click()
  await page.getByRole('button', { name: 'Previous page' }).click()
  await page.getByLabel('Search applications').fill('Portal Draft')
  await page.getByRole('button', { name: 'Search' }).click()
  await page.waitForURL('**/apps?q=Portal+Draft')
  await page.reload()
  await page.getByRole('link', { name: 'Portal Draft', exact: true }).waitFor()
  await page.getByLabel('Search applications').fill('No such application')
  await page.getByRole('button', { name: 'Search' }).click()
  await page.waitForURL('**/apps?q=No+such+application').catch(async () => {
    throw new Error(`Search URL did not update: ${page.url()}, input=${await page.getByLabel('Search applications').inputValue()}`)
  })
  await page.getByRole('heading', { name: 'No matching applications' }).waitFor({ timeout: 5000 }).catch(async () => {
    throw new Error(`Search empty state missing at ${page.url()}: ${(await page.locator('main').innerText()).slice(0, 1000)}`)
  })
  await page.route('**/api/v1/apps?*', route => route.fulfill({ status: 503, contentType: 'application/problem+json',
    body: JSON.stringify({ type: 'urn:app-depot:problem:retryable_failure', code: 'retryable_failure', title: 'Temporarily unavailable', status: 503 }) }))
  await page.goto(`${base}apps?q=failed-request`)
  await page.getByText('The server is not ready. Try again after recovery completes.').waitFor()
  await page.unroute('**/api/v1/apps?*')
  await page.goto(`${base}apps/00000000-0000-4000-8000-000000000000`)
  await page.getByRole('heading', { name: 'Application unavailable' }).waitFor()
  await page.goto(`${base}not-a-route`)
  await page.getByRole('heading', { name: 'Page not found' }).waitFor()
  await page.setViewportSize({ width: 390, height: 800 })
  await page.getByRole('button', { name: 'Open navigation' }).focus()
  await page.keyboard.press('Enter')
  if (!(await page.getByRole('navigation', { name: 'Server navigation' }).isVisible())) throw new Error('Mobile navigation did not open by keyboard')
  await page.getByRole('navigation', { name: 'Server navigation' }).getByRole('link', { name: 'Applications' }).click()
  await page.getByRole('heading', { name: 'Applications', exact: true }).waitFor()
  await page.setViewportSize({ width: 1280, height: 900 })

  const name = `Browser smoke ${Date.now()}`
  await page.getByRole('link', { name: 'Upload a new application' }).first().click()
  await page.getByLabel('App name').fill(name)
  await page.getByLabel('File').setInputFiles({ name: 'a.txt', mimeType: 'text/plain', buffer: Buffer.from('A') })
  keys.length = 0
  await page.getByRole('button', { name: 'Upload and publish' }).click()
  await page.getByText(`Published ${name}`).waitFor({ timeout: 30000 })
  await page.getByRole('link', { name: 'Open application' }).click()
  await page.getByText('Published', { exact: true }).first().waitFor()
  await page.getByText('Private', { exact: true }).first().waitFor()
  const uuidV4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
  if (keys.length !== 3 || keys.some(key => !uuidV4.test(key)) || new Set(keys).size !== 3) {
    throw new Error(`Upload, finalization, and publish keys are not distinct UUIDv4 values: ${JSON.stringify(keys)}`)
  }
  const backup = await page.evaluate(async key => {
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    const response = await fetch('/api/v1/backups', {
      method: 'POST',
      headers: { 'X-CSRF-Token': session.csrfToken, 'Idempotency-Key': key }
    })
    if (response.status !== 202) throw new Error(`Backup acceptance failed: ${response.status}`)
    return response.json()
  }, randomUUID())
  let completed
  for (let attempt = 0; attempt < 100; attempt++) {
    const response = await page.evaluate(id => fetch(`/api/v1/operations/${id}`).then(r => r.json()), backup.id)
    if (response.status === 'Completed') { completed = response; break }
    if (response.status === 'Failed') throw new Error('Backup operation failed')
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  if (!completed?.backupOutputAvailable) throw new Error('Verified backup did not become available')
  const manifest = await page.evaluate(async id => {
    const response = await fetch(`/api/v1/backups/${id}`)
    if (!response.ok) throw new Error(`Backup manifest failed: ${response.status}`)
    return response.json()
  }, completed.backupId)
  if (manifest.formatVersion !== 1 || !manifest.members.some(member => member.kind === 'SqliteSnapshot')) {
    throw new Error('Backup manifest is incomplete')
  }
  const readerPassword = 'browser reader password 12345'
  await page.evaluate(async ({ key, readerPassword }) => {
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    const response = await fetch('/api/v1/users', { method: 'POST', headers: { 'Content-Type': 'application/json',
      'X-CSRF-Token': session.csrfToken, 'Idempotency-Key': key },
      body: JSON.stringify({ username: 'reader', password: readerPassword, publishPermission: false }) })
    if (response.status !== 201) throw new Error(`Reader creation failed: ${response.status}`)
  }, { key: randomUUID(), readerPassword })
  const reader = await browser.newPage()
  const diagnosticRequests = []
  reader.on('request', request => { if (request.url().includes('/admin/diagnostics')) diagnosticRequests.push(request.url()) })
  await reader.goto(`${base}apps/new`)
  await reader.getByRole('heading', { name: 'Sign in' }).waitFor()
  await reader.getByLabel('Username').fill('reader')
  await reader.getByLabel('Password').fill(readerPassword)
  await reader.getByRole('button', { name: 'Sign in' }).click()
  await reader.getByRole('heading', { name: 'Create application' }).waitFor()
  await reader.getByText('Publish permission is required to create an application.').waitFor()
  await reader.goto(base)
  await reader.getByRole('heading', { name: 'Server overview' }).waitFor()
  if (diagnosticRequests.length) throw new Error('Non-Admin browser requested protected diagnostics')
  await reader.locator('.account-menu summary').click()
  await reader.getByRole('button', { name: 'Sign out' }).click()
  await reader.getByRole('heading', { name: 'Sign in' }).waitFor()
  await reader.close()
  await page.evaluate(async () => {
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    const response = await fetch('/api/v1/auth/sessions/current', { method: 'DELETE', headers: { 'X-CSRF-Token': session.csrfToken } })
    if (!response.ok) throw new Error(`Session invalidation failed: ${response.status}`)
  })
  await page.reload()
  await page.getByRole('heading', { name: 'Sign in' }).waitFor()
  console.log('Browser smoke passed: setup, auth, navigation, Apps, themes, LAN HTTP upload/publish and backup')
} finally {
  await browser?.close()
  if (preview) {
    if (preview.server.exitCode === null) {
      preview.server.kill('SIGTERM')
      await new Promise(resolve => preview.server.once('exit', resolve))
    }
    await rm(preview.namespace, { recursive: true, force: true })
  }
}
