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
        if (response.ok) return { namespace, server, token, base: `http://app-depot.test:${port}/` }
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
  await page.getByText('Server ready').waitFor()
  if (setupToken) {
    await page.getByLabel('Setup token').fill(setupToken)
    await page.getByLabel('Password (at least 16 characters)').fill(password)
    await page.getByRole('button', { name: 'Create Admin' }).click()
    await page.getByText('Admin created. Sign in with your new password.').waitFor()
  }
  await page.getByLabel('Username').fill('admin')
  await page.getByLabel('Password').fill(password)
  await page.getByRole('button', { name: 'Sign in' }).click()
  await page.getByText('Signed in as admin').waitFor()
  const name = `Browser smoke ${Date.now()}`
  await page.getByLabel('App name').fill(name)
  await page.getByLabel('File').setInputFiles({ name: 'a.txt', mimeType: 'text/plain', buffer: Buffer.from('A') })
  await page.getByRole('button', { name: 'Upload and publish' }).click()
  await page.getByText(`Published ${name}`).waitFor({ timeout: 30000 })
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
  console.log('Browser smoke passed: login, upload, publish and verified backup through real Preview Server')
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
