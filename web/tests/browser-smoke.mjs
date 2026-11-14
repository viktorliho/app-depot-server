import { chromium } from 'playwright-core'
import { createHash, randomUUID } from 'node:crypto'
import { execFileSync, spawn } from 'node:child_process'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

async function emptyRevisionIndex(page, path) {
  return page.evaluate(async path => {
    const response = await fetch(`/api/v1${path}/revisions?pageSize=100`)
    if (!response.ok) throw new Error(`Revision list failed: ${response.status}`)
    const revisions = await response.json()
    const index = revisions.items.findIndex(item => JSON.stringify(item.data) === '{}')
    if (index < 0) throw new Error('Previous empty declaration revision is missing')
    return index
  }, path)
}

async function associationWithRoot(page, releaseId, rootPath) {
  return page.evaluate(async ({ releaseId, rootPath }) => {
    const associations = await (await fetch(`/api/v1/releases/${releaseId}/artifacts?pageSize=100`)).json()
    for (const association of associations.items) {
      const artifact = await (await fetch(`/api/v1/artifacts/${encodeURIComponent(association.artifactId)}`)).json()
      if (artifact.kind !== 'Tree') continue
      const inventory = await (await fetch(`/api/v1/artifacts/${encodeURIComponent(association.artifactId)}/inventory?parent=`)).json()
      if (inventory.items.some(item => item.path === rootPath)) return association.id
    }
    throw new Error(`Artifact association for ${rootPath} is missing`)
  }, { releaseId, rootPath })
}

async function verifyRestoredSnapshot(preview, page, manifest, password, ids) {
  const scratch = await mkdtemp(join(tmpdir(), 'app-depot-m10-restore-'))
  const backupDir = join(scratch, 'portable-backup')
  const restoredRoot = join(scratch, 'restored-data')
  let restoredServer
  try {
    await mkdir(backupDir)
    const signedCookie = (await page.context().cookies(preview.base)).find(cookie => cookie.name === 'ad_session')
    if (!signedCookie) throw new Error('Admin cookie missing during backup export')
    for (const member of manifest.members) {
      if (!/^(catalog\.sqlite|cas\/[0-9a-f]{64})$/.test(member.relativePath)) throw new Error(`Unsafe portable backup path: ${member.relativePath}`)
      const response = await fetch(`http://127.0.0.1:${preview.port}/api/v1/backups/${manifest.backupId}/entries/${encodeURIComponent(member.entryId)}`,
        { headers: { Cookie: `ad_session=${signedCookie.value}` } })
      if (!response.ok) throw new Error(`Backup member export failed: ${response.status}`)
      const bytes = Buffer.from(await response.arrayBuffer())
      if (bytes.length !== member.size || createHash('sha256').update(bytes).digest('hex') !== member.sha256) throw new Error(`Backup member integrity failed: ${member.relativePath}`)
      await mkdir(dirname(join(backupDir, member.relativePath)), { recursive: true })
      await writeFile(join(backupDir, member.relativePath), bytes)
    }
    await writeFile(join(backupDir, 'manifest.json'), JSON.stringify(manifest))
    execFileSync(preview.binary, ['restore', '--backup-dir', backupDir, '--data-root', restoredRoot])
    const listener = createServer()
    await new Promise(resolve => listener.listen(0, '127.0.0.1', resolve))
    const port = listener.address().port
    await new Promise(resolve => listener.close(resolve))
    const cleanEnvironment = Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith('APP_DEPOT_')))
    restoredServer = spawn(preview.binary, ['serve', '--mode', 'production'], {
      env: { ...cleanEnvironment, APP_DEPOT_DATA_ROOT: restoredRoot, APP_DEPOT_BIND: `127.0.0.1:${port}`, APP_DEPOT_ASSETS: join(preview.repository, 'web/dist') },
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    let errors = ''
    restoredServer.stderr.on('data', chunk => { errors = (errors + chunk.toString()).slice(-2000) })
    const rootUrl = `http://127.0.0.1:${port}`
    const deadline = Date.now() + 15000
    let ready = false
    while (Date.now() < deadline) {
      if (restoredServer.exitCode !== null) throw new Error(`Restored Server exited: ${errors}`)
      try { if ((await fetch(`${rootUrl}/api/v1/health/ready`)).ok) { ready = true; break } } catch { /* startup */ }
      await new Promise(resolve => setTimeout(resolve, 100))
    }
    if (!ready) throw new Error(`Restored Server not ready: ${errors}`)
    const login = await fetch(`${rootUrl}/api/v1/auth/sessions`, { method: 'POST', headers: { Origin: rootUrl, 'Content-Type': 'application/json' }, body: JSON.stringify({ username: 'admin', password }) })
    if (!login.ok) throw new Error(`Restored Admin login failed: ${login.status}`)
    const restoredCookie = login.headers.get('set-cookie')?.split(';')[0]
    if (!restoredCookie) throw new Error('Restored Server did not issue a session')
    const get = async path => {
      const response = await fetch(`${rootUrl}/api/v1${path}`, { headers: { Cookie: restoredCookie } })
      if (!response.ok) throw new Error(`Restored resource unavailable: ${path} (${response.status})`)
      return response
    }
    const app = await (await get(`/apps/${ids.appId}/declarations`)).json()
    const release = await (await get(`/releases/${ids.releaseId}/declarations`)).json()
    const association = await (await get(`/associations/${ids.associationId}/declarations`)).json()
    const artwork = await (await get(`/apps/${ids.appId}/artwork`)).json()
    if (app.data.appDepot?.kind !== 'tool' || app.data.appDepot?.tags?.length !== 2 ||
        release.data.appDepot?.platforms?.length !== 2 || association.data.appDepot?.entryPoints?.[0]?.target?.path !== 'browser-tree/nested/tree.txt' ||
        artwork.items.length !== 3) throw new Error('Restored metadata, entry points or artwork bindings differ from source')
    for (const item of artwork.items) {
      const bytes = Buffer.from(await (await get(`/apps/${ids.appId}/artwork/${encodeURIComponent(item.role)}/content`)).arrayBuffer())
      if (bytes.length !== item.size || createHash('sha256').update(bytes).digest('hex') !== item.digest) throw new Error(`Restored artwork bytes failed integrity: ${item.role}`)
    }
    const inventory = await (await get(`/artifacts/${encodeURIComponent(ids.artifactId)}/inventory?q=tree.txt`)).json()
    const file = inventory.items.find(item => item.path === 'browser-tree/nested/tree.txt')
    if (!file?.fileId) throw new Error('Restored original tree inventory is incomplete')
    const original = Buffer.from(await (await get(`/artifacts/${encodeURIComponent(ids.artifactId)}/files/${encodeURIComponent(file.fileId)}`)).arrayBuffer())
    if (createHash('sha256').update(original).digest('hex') !== file.sha256) throw new Error('Restored original Artifact bytes failed SHA-256')
  } finally {
    if (restoredServer && restoredServer.exitCode === null) {
      restoredServer.kill('SIGTERM')
      await new Promise(resolve => restoredServer.once('exit', resolve))
    }
    await rm(scratch, { recursive: true, force: true })
  }
}

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
  await page.getByRole('navigation', { name: 'Application navigation' }).getByRole('link', { name: 'Artwork' }).click()
  await page.getByRole('heading', { name: 'Artwork', exact: true }).waitFor()
  await page.getByRole('heading', { name: 'No artwork yet' }).waitFor()
  await page.getByLabel('Image file').setInputFiles(new URL('../../crates/server/tests/fixtures/artwork.png', import.meta.url).pathname)
  await page.getByRole('button', { name: 'Add artwork' }).click()
  await page.getByText('Artwork added.').waitFor()
  await page.getByRole('img', { name: 'Icon for Portal Draft' }).waitFor()
  if (!await page.getByRole('img', { name: 'Icon for Portal Draft' }).evaluate(image => image.complete && image.naturalWidth === 2 && image.naturalHeight === 3)) {
    throw new Error('Artwork preview did not load original image bytes')
  }
  await page.getByLabel('Image file').setInputFiles(new URL('../../crates/server/tests/fixtures/artwork.jpg', import.meta.url).pathname)
  await page.getByRole('button', { name: 'Replace artwork' }).click()
  await page.getByText('Artwork replaced.').waitFor()
  await page.getByRole('button', { name: 'Remove Icon' }).click()
  await page.getByRole('button', { name: 'Remove artwork' }).click()
  await page.getByText('icon removed.').waitFor()
  await page.reload()
  await page.getByRole('heading', { name: 'Artwork', exact: true }).waitFor()
  await page.getByText('No artwork yet').waitFor()
  await page.getByRole('navigation', { name: 'Application navigation' }).getByRole('link', { name: 'Releases' }).click()
  await page.getByRole('heading', { name: 'Releases', exact: true }).waitFor()
  await page.goBack()
  await page.getByRole('heading', { name: 'Artwork', exact: true }).waitFor()
  await page.goForward()
  await page.getByRole('heading', { name: 'Releases', exact: true }).waitFor()
  await page.goto(`${base}apps/${draftId}`)
  await page.getByRole('heading', { name: 'Portal Draft' }).waitFor()
  await page.reload()
  await page.getByRole('heading', { name: 'Portal Draft' }).waitFor()
  await page.getByRole('link', { name: 'Add release from file' }).first().click()
  await page.getByRole('heading', { name: 'Add release from file' }).waitFor()
  await page.getByLabel('File', { exact: true }).setInputFiles({ name: 'draft.txt', mimeType: 'text/plain', buffer: Buffer.from('Draft') })
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText('Saved Draft for Portal Draft').waitFor({ timeout: 30000 })
  let integratedIds
  if (preview) {
  await page.getByRole('link', { name: 'Open release' }).click()
  await page.waitForURL(/\/releases\/[0-9a-f-]{36}$/)
  const firstReleaseId = new URL(page.url()).pathname.split('/').pop()
  await page.getByRole('heading', { name: 'Artifacts' }).waitFor()
  const originalArtifact = (await page.locator('code').filter({ hasText: 'adfile:sha256:' }).first().textContent()).match(/adfile:sha256:[0-9a-f]{64}/)?.[0]
  if (!originalArtifact) throw new Error('File Artifact identity missing')
  await page.goto(`${base}apps/${draftId}/settings`)
  await page.getByRole('heading', { name: 'Application settings' }).waitFor()
  const staleAppEtag = await page.evaluate(async id => (await fetch(`/api/v1/apps/${id}`)).headers.get('ETag'), draftId)
  await page.getByLabel('Name', { exact: true }).fill('Portal Draft Edited')
  await page.getByLabel('Slug', { exact: true }).fill('portal-draft-edited')
  await page.getByLabel('Visibility').selectOption('Public')
  await page.getByRole('button', { name: 'Save changes' }).click()
  await page.getByText('Application settings saved.').waitFor()
  const staleApp = await page.evaluate(async ({ id, etag, key }) => {
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    return (await fetch(`/api/v1/apps/${id}`, { method: 'PATCH', headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': session.csrfToken,
      'If-Match': etag, 'Idempotency-Key': key }, body: JSON.stringify({ name: 'Stale edit' }) })).status
  }, { id: draftId, etag: staleAppEtag, key: randomUUID() })
  if (staleApp !== 412) throw new Error(`Stale App ETag was accepted: ${staleApp}`)
  await page.reload()
  await page.getByRole('navigation', { name: 'Application navigation' }).getByRole('link', { name: 'Declarations' }).click()
  await page.getByRole('heading', { name: 'Application declarations' }).waitFor()
  await page.getByLabel('Current JSON value').fill('null')
  await page.getByRole('button', { name: 'Save new revision' }).click()
  await page.getByText('Saved as a new declaration revision.').waitFor()
  const nullDocument = await page.evaluate(async id => (await (await fetch(`/api/v1/apps/${id}/declarations`)).json()).data, draftId)
  if (nullDocument !== null) throw new Error('Explicit JSON null was not preserved')
  await page.getByRole('button', { name: 'Standard fields' }).click()
  await page.getByText('Standard fields require a JSON object. Use the JSON editor to preserve other values, including null.').waitFor()
  await page.getByRole('button', { name: 'JSON editor' }).click()
  await page.getByRole('button', { name: 'Show revision history' }).click()
  await page.getByRole('button', { name: 'Inspect revision' }).nth(await emptyRevisionIndex(page, `/apps/${draftId}/declarations`)).click()
  await page.getByRole('button', { name: 'Restore as new revision' }).click()
  await page.getByText('Saved as a new declaration revision.').waitFor()
  const restoredAppData = await page.evaluate(async id => (await (await fetch(`/api/v1/apps/${id}/declarations`)).json()).data, draftId)
  if (JSON.stringify(restoredAppData) !== '{}') throw new Error(`App declaration restoration did not create the prior value: ${JSON.stringify(restoredAppData)}`)
  const appDeclarations = page.getByRole('heading', { name: 'Application data' }).locator('..')
  await appDeclarations.getByLabel('Current JSON value').fill('{"custom":{"keep":true},"appDepot":{"extension":"kept"}}')
  await appDeclarations.getByRole('button', { name: 'Save new revision' }).click()
  await appDeclarations.getByRole('button', { name: 'Standard fields' }).click()
  await appDeclarations.getByLabel('Summary').fill('An optional artifact catalog')
  await appDeclarations.getByLabel('Kind (optional free-form label)').fill('tool')
  await appDeclarations.getByLabel('Tags (one per line)').fill('utilities\nutilities')
  await appDeclarations.getByRole('button', { name: 'Save standard metadata' }).click()
  await appDeclarations.getByText('Tags contains a duplicate entry.').waitFor()
  await appDeclarations.getByLabel('Tags (one per line)').fill('utilities\nself-hosted')
  await appDeclarations.getByRole('button', { name: 'Save standard metadata' }).click()
  await appDeclarations.getByText('Saved as a new declaration revision.').waitFor()
  const standardAppData = await page.evaluate(async id => (await (await fetch(`/api/v1/apps/${id}/declarations`)).json()).data, draftId)
  if (standardAppData.custom?.keep !== true || standardAppData.appDepot?.extension !== 'kept' ||
      standardAppData.appDepot?.kind !== 'tool' || standardAppData.appDepot?.tags?.length !== 2) {
    throw new Error(`Standard App metadata lost extension data or typed fields: ${JSON.stringify(standardAppData)}`)
  }
  await appDeclarations.getByRole('button', { name: 'JSON editor' }).click()
  if (!(await appDeclarations.getByLabel('Current JSON value').inputValue()).includes('self-hosted')) throw new Error('Standard fields did not update the full JSON editor')
  await appDeclarations.getByRole('button', { name: 'Standard fields' }).click()
  await appDeclarations.getByLabel('Kind (optional free-form label)').fill('')
  await appDeclarations.getByLabel('Tags (one per line)').fill('')
  await appDeclarations.getByRole('button', { name: 'Save standard metadata' }).click()
  await page.waitForFunction(async id => {
    const data = (await (await fetch(`/api/v1/apps/${id}/declarations`)).json()).data
    return data.appDepot && !('kind' in data.appDepot) && !('tags' in data.appDepot)
  }, draftId)
  const optionalAppData = await page.evaluate(async id => (await (await fetch(`/api/v1/apps/${id}/declarations`)).json()).data, draftId)
  if ('kind' in optionalAppData.appDepot || 'tags' in optionalAppData.appDepot || optionalAppData.appDepot.extension !== 'kept') {
    throw new Error('Clearing optional fields did not remove only those fields')
  }
  await appDeclarations.getByLabel('Kind (optional free-form label)').fill('tool')
  await appDeclarations.getByLabel('Description', { exact: true }).fill('An integrated directory application')
  await appDeclarations.getByLabel('Tags (one per line)').fill('utilities\nself-hosted')
  const integratedMetadataSave = page.waitForResponse(response => response.url().endsWith(`/api/v1/apps/${draftId}/declarations`) && response.request().method() === 'PUT')
  await appDeclarations.getByRole('button', { name: 'Save standard metadata' }).click()
  if ((await integratedMetadataSave).status() !== 200) throw new Error('Integrated App metadata save failed')
  await page.goto(`${base}apps/${draftId}/releases/new`)
  await page.getByRole('heading', { name: 'Create Draft release' }).waitFor()
  await page.getByLabel('Version (optional)').fill('test-v1')
  await page.getByRole('button', { name: 'Create Draft release' }).click()
  await page.waitForURL(/\/releases\/[0-9a-f-]{36}$/)
  const releaseId = new URL(page.url()).pathname.split('/').pop()
  await page.getByRole('heading', { name: 'test-v1' }).waitFor()
  const staleReleaseEtag = await page.evaluate(async id => (await fetch(`/api/v1/releases/${id}`)).headers.get('ETag'), releaseId)
  await page.getByLabel('Version (optional)').fill('test-v2')
  await page.getByRole('button', { name: 'Save version' }).click()
  await page.getByRole('heading', { name: 'test-v2' }).waitFor()
  const staleRelease = await page.evaluate(async ({ id, etag }) => {
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    return (await fetch(`/api/v1/releases/${id}`, { method: 'PATCH', headers: { 'Content-Type': 'application/json',
      'X-CSRF-Token': session.csrfToken, 'If-Match': etag }, body: JSON.stringify({ version: 'stale' }) })).status
  }, { id: releaseId, etag: staleReleaseEtag })
  if (staleRelease !== 412) throw new Error(`Stale Release ETag was accepted: ${staleRelease}`)
  await page.getByRole('link', { name: 'Upload Artifact' }).first().click()
  await page.getByRole('heading', { name: 'Upload Artifact' }).waitFor()
  const fileBytes = Buffer.from('M9.2 file artifact content')
  await page.getByLabel('File', { exact: true }).setInputFiles({ name: 'content.txt', mimeType: 'text/plain', buffer: fileBytes })
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText('Saved Draft for Portal Draft Edited').waitFor({ timeout: 30000 })
  await page.getByRole('link', { name: 'Open release' }).click()
  const fileArtifact = (await page.locator('code').filter({ hasText: 'adfile:sha256:' }).first().textContent()).match(/adfile:sha256:[0-9a-f]{64}/)?.[0]
  if (!fileArtifact) throw new Error('Uploaded File Artifact identity missing')
  const downloaded = await page.evaluate(async id => Array.from(new Uint8Array(await (await fetch(`/api/v1/artifacts/${encodeURIComponent(id)}/content`)).arrayBuffer())), fileArtifact)
  if (createHash('sha256').update(Buffer.from(downloaded)).digest('hex') !== fileArtifact.split(':').pop()) throw new Error('Downloaded File Artifact hash mismatch')
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/uploads/new`)
  const largeBytes = Buffer.alloc(4 * 1024 * 1024 + 8, 65)
  const sentRanges = []
  let injectedFailure = false
  await page.route('**/api/v1/uploads/*/files/*', async route => {
    if (route.request().method() === 'PUT') {
      const range = route.request().headers()['content-range']
      sentRanges.push(range)
      if (range.startsWith('bytes 4194304-') && !injectedFailure) {
        injectedFailure = true
        await route.fulfill({ status: 503, contentType: 'application/problem+json', body: JSON.stringify({ title: 'Temporary transfer failure', status: 503 }) })
        return
      }
    }
    await route.continue()
  })
  await page.getByLabel('File', { exact: true }).setInputFiles({ name: 'resume.bin', mimeType: 'application/octet-stream', buffer: largeBytes })
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText('The server is not ready. Try again after recovery completes.').waitFor()
  await page.reload()
  await page.getByLabel('File', { exact: true }).setInputFiles({ name: 'resume.bin', mimeType: 'application/octet-stream', buffer: largeBytes })
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText('Saved Draft for Portal Draft Edited').waitFor({ timeout: 30000 })
  await page.unroute('**/api/v1/uploads/*/files/*')
  if (sentRanges.filter(range => range.startsWith('bytes 0-')).length !== 1 || sentRanges.filter(range => range.startsWith('bytes 4194304-')).length !== 2) {
    throw new Error(`Upload retry did not honor authoritative ranges: ${JSON.stringify(sentRanges)}`)
  }
  await page.getByRole('link', { name: 'Open release' }).click()
  await page.getByLabel('Current JSON value').first().fill('{"artifact":"data"}')
  await page.getByRole('button', { name: 'Save new revision' }).first().click()
  await page.getByText('Saved as a new declaration revision.').first().waitFor()
  await page.getByRole('button', { name: 'Show revision history' }).first().click()
  const firstAssociationId = await page.evaluate(async id => (await (await fetch(`/api/v1/releases/${id}/artifacts?pageSize=10`)).json()).items[0].id, releaseId)
  await page.getByRole('button', { name: 'Inspect revision' }).nth(await emptyRevisionIndex(page, `/associations/${firstAssociationId}/declarations`)).click()
  await page.getByRole('button', { name: 'Restore as new revision' }).click()
  await page.getByText('Saved as a new declaration revision.').first().waitFor()
  const associationDeclarations = page.getByRole('heading', { name: 'Association declarations' }).first().locator('..')
  await associationDeclarations.getByRole('button', { name: 'Standard fields' }).click()
  await associationDeclarations.getByLabel('Artifact label').fill('Primary payload')
  await associationDeclarations.getByLabel('Purpose').fill('distribution')
  await associationDeclarations.getByRole('button', { name: 'Save standard metadata' }).click()
  await associationDeclarations.getByText('Saved as a new declaration revision.').waitFor()
  const standardAssociationData = await page.evaluate(async id => (await (await fetch(`/api/v1/associations/${id}/declarations`)).json()).data, firstAssociationId)
  if (standardAssociationData.appDepot?.label !== 'Primary payload' || standardAssociationData.appDepot?.purpose !== 'distribution') {
    throw new Error('Standard association metadata was not saved')
  }
  await page.getByLabel('Current JSON value').last().fill('{"release":"data"}')
  const releaseRawSave = page.waitForResponse(response => response.url().endsWith(`/api/v1/releases/${releaseId}/declarations`) && response.request().method() === 'PUT')
  await page.getByRole('button', { name: 'Save new revision' }).last().click()
  const savedReleaseDocument = await (await releaseRawSave).json()
  await page.getByRole('heading', { name: 'Release declarations' }).locator('..').locator('code').filter({ hasText: savedReleaseDocument.revisionId }).waitFor({ state: 'attached' })
  await page.getByText('Saved as a new declaration revision.').last().waitFor()
  const releaseDeclarations = page.getByRole('heading', { name: 'Release declarations' }).locator('..')
  await releaseDeclarations.getByRole('button', { name: 'Standard fields' }).click()
  await releaseDeclarations.getByLabel('Release notes').fill('Initial public content')
  await releaseDeclarations.getByLabel('Platforms (one per line)').fill('linux/amd64\nmacos/arm64')
  await releaseDeclarations.getByLabel('Runtime name').fill('optional-runtime')
  const releaseDeclarationSave = page.waitForResponse(response => response.url().endsWith(`/api/v1/releases/${releaseId}/declarations`) && response.request().method() === 'PUT')
  await releaseDeclarations.getByRole('button', { name: 'Save standard metadata' }).click()
  const releaseDeclarationResponse = await releaseDeclarationSave
  if (releaseDeclarationResponse.status() !== 200) throw new Error(`Release declaration save failed: ${releaseDeclarationResponse.status()}`)
  await releaseDeclarations.getByText('Saved as a new declaration revision.').waitFor()
  const snapshot = await page.evaluate(async id => (await (await fetch(`/api/v1/releases/${id}/declaration-snapshot`)).json()), releaseId)
  if (snapshot.release.data.release !== 'data' || snapshot.release.data.appDepot?.platforms?.length !== 2 ||
      snapshot.release.data.appDepot?.runtime?.name !== 'optional-runtime' || snapshot.associations.length !== 2) {
    throw new Error(`Combined declaration snapshot is incomplete: ${JSON.stringify(snapshot)}`)
  }
  const treeRoot = join(preview.namespace, 'browser-tree')
  await mkdir(join(treeRoot, 'nested'), { recursive: true })
  await writeFile(join(treeRoot, 'nested', 'tree.txt'), 'tree content')
  await writeFile(join(treeRoot, 'nested', 'é漢字.txt'), 'utf8 content')
  await Promise.all(Array.from({ length: 51 }, (_, index) => writeFile(join(treeRoot, `zero-${String(index).padStart(2, '0')}.txt`), '')))
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/uploads/new`)
  await page.getByLabel('Upload mode').selectOption('BareTree')
  await page.getByLabel('Directory files').setInputFiles(treeRoot)
  await page.getByLabel('Empty directory paths').fill('browser-tree/empty')
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText('Saved Draft for Portal Draft Edited').waitFor({ timeout: 30000 })
  await page.getByRole('link', { name: 'Open release' }).click()
  const treeAssociationId = await associationWithRoot(page, releaseId, 'browser-tree')
  const treeArtifactId = await page.evaluate(async id => (await (await fetch(`/api/v1/associations/${id}`)).json()).artifactId, treeAssociationId)
  integratedIds = { appId: draftId, releaseId, associationId: treeAssociationId, artifactId: treeArtifactId }
  const referenceStatuses = await page.evaluate(async id => {
    const points = ['browser-tree/nested/é漢字.txt', 'browser-tree/empty', 'missing.txt', '../escape'].map((path, index) => ({ id: `case-${index}`, target: { type: 'TreeFile', path } }))
    const response = await fetch(`/api/v1/associations/${id}/entry-points/validate`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ data: { appDepot: { entryPoints: points } } }) })
    if (!response.ok) throw new Error(`Entry-point assessment failed: ${response.status}`)
    return (await response.json()).items.map(item => item.status)
  }, treeAssociationId)
  if (JSON.stringify(referenceStatuses) !== JSON.stringify(['Valid', 'NotFile', 'Missing', 'Invalid'])) throw new Error(`Unexpected reference statuses: ${referenceStatuses}`)
  await page.locator(`a[href$="/artifacts/${treeAssociationId}"]`).click()
  await page.getByRole('heading', { name: 'Artifact Explorer' }).waitFor()
  await page.getByRole('button', { name: '📁 browser-tree' }).click()
  await page.getByRole('button', { name: '📁 empty' }).click()
  await page.getByRole('heading', { name: 'Empty directory' }).waitFor()
  await page.getByRole('button', { name: 'browser-tree', exact: true }).click()
  await page.getByRole('button', { name: '📁 nested' }).click()
  await page.getByText('📄 tree.txt').waitFor()
  await page.getByText('📄 é漢字.txt').waitFor()
  const treeFileLink = await page.getByRole('link', { name: 'Download', exact: true }).first().getAttribute('href')
  const treeBytes = await page.evaluate(async href => Array.from(new Uint8Array(await (await fetch(href)).arrayBuffer())), treeFileLink)
  if (Buffer.from(treeBytes).toString() !== 'tree content') throw new Error(`Tree file download mismatch: ${treeFileLink} ${Buffer.from(treeBytes).toString()}`)
  const treeDigest = await page.evaluate(async href => {
    const artifactId = decodeURIComponent(href.split('/artifacts/')[1].split('/files/')[0])
    const inventory = await (await fetch(`/api/v1/artifacts/${encodeURIComponent(artifactId)}/inventory?q=tree.txt`)).json()
    return inventory.items.find(item => item.path === 'browser-tree/nested/tree.txt').sha256
  }, treeFileLink)
  if (createHash('sha256').update(Buffer.from(treeBytes)).digest('hex') !== treeDigest) throw new Error('Tree download SHA-256 mismatch')
  await page.getByRole('row', { name: /tree\.txt/ }).getByRole('button', { name: 'Add entry point' }).click()
  await page.getByRole('button', { name: 'Add entry point' }).last().click()
  await page.getByLabel('Label (optional)').fill('Tree script')
  await page.getByLabel('Target operating systems and CPU architectures (one per line; optional)').fill('linux/x86_64')
  await page.getByRole('button', { name: 'Save entry points' }).click()
  await page.getByText('Entry points saved as a new declaration revision.').waitFor()
  await page.getByRole('row', { name: /tree\.txt/ }).getByRole('button', { name: 'Add entry point' }).click()
  await page.getByRole('button', { name: 'Add entry point' }).last().click()
  await page.getByLabel('Label (optional)').last().fill('Alternative')
  await page.getByRole('button', { name: 'Save entry points' }).click()
  await page.getByText('Entry points saved as a new declaration revision.').waitFor()
  const multiplePoints = await page.evaluate(async id => (await (await fetch(`/api/v1/associations/${id}/declarations`)).json()).data.appDepot.entryPoints, treeAssociationId)
  if (multiplePoints.length !== 2 || multiplePoints[1].label !== 'Alternative') throw new Error('Multiple entry points were not saved')
  await page.getByRole('button', { name: 'Remove entry point' }).last().click()
  await page.getByRole('button', { name: 'Save entry points' }).click()
  await page.getByText('Entry points saved as a new declaration revision.').waitFor()
  const remainingPoints = await page.evaluate(async id => (await (await fetch(`/api/v1/associations/${id}/declarations`)).json()).data.appDepot.entryPoints, treeAssociationId)
  if (remainingPoints.length !== 1 || remainingPoints[0].label !== 'Tree script') throw new Error('Entry point removal did not preserve the first point')
  await page.getByRole('button', { name: 'Change target using Explorer' }).click()
  await page.getByRole('row', { name: /é漢字\.txt/ }).getByRole('button', { name: 'Add entry point' }).click()
  await page.getByRole('button', { name: 'Use selected target' }).click()
  await page.getByRole('button', { name: 'Save entry points' }).click()
  await page.getByText('Entry points saved as a new declaration revision.').waitFor()
  const editedPoint = await page.evaluate(async id => (await (await fetch(`/api/v1/associations/${id}/declarations`)).json()).data.appDepot.entryPoints[0], treeAssociationId)
  if (editedPoint.target.path !== 'browser-tree/nested/é漢字.txt') throw new Error('Entry point target edit failed')
  await page.getByRole('button', { name: 'Change target using Explorer' }).click()
  await page.getByRole('row', { name: /tree\.txt/ }).getByRole('button', { name: 'Add entry point' }).click()
  await page.getByRole('button', { name: 'Use selected target' }).click()
  await page.getByRole('button', { name: 'Save entry points' }).click()
  await page.getByText('Entry points saved as a new declaration revision.').waitFor()
  await page.getByRole('button', { name: 'browser-tree', exact: true }).click()
  await page.getByRole('button', { name: 'Next page' }).click()
  await page.getByText('📄 zero-50.txt').waitFor()
  await page.getByLabel('Search original paths').fill('tree.txt')
  await page.getByRole('button', { name: 'Search', exact: true }).click()
  await page.getByText('browser-tree/nested/tree.txt').waitFor()
  await page.reload()
  await page.getByRole('heading', { name: 'Artifact Explorer' }).waitFor()
  if (await page.getByLabel('Label (optional)').inputValue() !== 'Tree script') throw new Error('Entry point label was not restored after reload')
  const zipPath = join(preview.namespace, 'bundle.zip')
  execFileSync('python3', ['-c', 'import sys,zipfile; z=zipfile.ZipFile(sys.argv[1],"w",zipfile.ZIP_DEFLATED); z.writestr("zip/nested.txt","zip content"); z.close()', zipPath])
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/uploads/new`)
  await page.getByLabel('Upload mode').selectOption('ZipImport')
  await page.getByLabel('File', { exact: true }).setInputFiles(zipPath)
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText('Saved Draft for Portal Draft Edited').waitFor({ timeout: 30000 })
  await page.getByRole('link', { name: 'Open release' }).click()
  const zipAssociationId = await associationWithRoot(page, releaseId, 'zip')
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/artifacts/${zipAssociationId}`)
  await page.getByRole('button', { name: '📁 zip' }).click()
  await page.getByText('📄 nested.txt').waitFor()
  const zipFileLink = await page.getByRole('link', { name: 'Download', exact: true }).first().getAttribute('href')
  const zipBytes = await page.evaluate(async href => Array.from(new Uint8Array(await (await fetch(href)).arrayBuffer())), zipFileLink)
  if (Buffer.from(zipBytes).toString() !== 'zip content') throw new Error('ZIP-imported file download mismatch')
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/artifacts/${firstAssociationId}`)
  await page.getByRole('heading', { name: 'File Artifact' }).waitFor()
  await page.getByRole('button', { name: 'Select as entry point' }).click()
  await page.getByRole('button', { name: 'Add entry point' }).click()
  await page.getByRole('button', { name: 'Save entry points' }).click()
  await page.getByText('Entry points saved as a new declaration revision.').waitFor()
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}`)
  const priorAssociationIds = await page.evaluate(async id => (await (await fetch(`/api/v1/releases/${id}/artifacts?pageSize=100`)).json()).items.map(item => item.id), releaseId)
  await page.getByLabel('Artifact ID').fill(originalArtifact)
  await page.getByRole('button', { name: 'Associate Artifact' }).click()
  await page.getByText('Existing Artifact associated').waitFor()
  const addedAssociationId = await page.evaluate(async ({ id, prior }) => (await (await fetch(`/api/v1/releases/${id}/artifacts?pageSize=100`)).json()).items.find(item => !prior.includes(item.id))?.id, { id: releaseId, prior: priorAssociationIds })
  if (!addedAssociationId) throw new Error('New association was not visible')
  await page.locator(`a[href$="/artifacts/${addedAssociationId}"]`).locator('..').getByRole('button', { name: 'Remove association' }).click()
  await page.getByRole('dialog', { name: 'Remove association' }).getByRole('button', { name: 'Remove association' }).click()
  await page.getByRole('heading', { name: 'Remove artifact association operation' }).waitFor()
  await page.getByRole('button', { name: 'Publish release' }).click()
  await page.getByText('Release published. Artifact composition is now immutable.').waitFor()
  if (await page.getByRole('button', { name: 'Associate Artifact' }).count()) throw new Error('Published composition exposed association action')
  const immutable = await page.evaluate(async ({ releaseId, artifactId, key }) => {
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    return (await fetch(`/api/v1/releases/${releaseId}/artifacts`, { method: 'POST', headers: { 'Content-Type': 'application/json',
      'X-CSRF-Token': session.csrfToken, 'Idempotency-Key': key }, body: JSON.stringify({ artifactId }) })).status
  }, { releaseId, artifactId: originalArtifact, key: randomUUID() })
  if (immutable !== 409) throw new Error(`Published Release composition was mutable: ${immutable}`)
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/artifacts/${treeAssociationId}`)
  await page.getByRole('heading', { name: 'Artifact Explorer' }).waitFor()
  await page.getByLabel('Label (optional)').fill('Published script')
  await page.getByRole('button', { name: 'Save entry points' }).click()
  await page.getByText('Entry points saved as a new declaration revision.').waitFor()
  const publishedEntry = await page.evaluate(async id => (await (await fetch(`/api/v1/associations/${id}/declarations`)).json()).data.appDepot.entryPoints[0], treeAssociationId)
  if (publishedEntry.label !== 'Published script' || publishedEntry.target.path !== 'browser-tree/nested/tree.txt') throw new Error('Published association declaration edit failed')
  await page.goto(`${base}apps/${draftId}/artwork`)
  await page.getByRole('heading', { name: 'Artwork', exact: true }).waitFor()
  const artworkFixtures = {
    png: new URL('../../crates/server/tests/fixtures/artwork.png', import.meta.url).pathname,
    jpeg: new URL('../../crates/server/tests/fixtures/artwork.jpg', import.meta.url).pathname,
    webp: new URL('../../crates/server/tests/fixtures/artwork.webp', import.meta.url).pathname,
  }
  for (const [role, fixture] of [['icon', 'png'], ['logo', 'webp'], ['steam.hero', 'jpeg']]) {
    await page.getByLabel('Artwork role').selectOption(role)
    await page.getByLabel('Image file').setInputFiles(artworkFixtures[fixture])
    await page.getByRole('button', { name: 'Add artwork' }).click()
    await page.getByText('Artwork added.').waitFor()
  }
  await page.getByLabel('Artwork role').selectOption('icon')
  await page.getByRole('button', { name: 'Replace artwork' }).waitFor()
  await page.getByLabel('Image file').setInputFiles(artworkFixtures.jpeg)
  await page.getByRole('button', { name: 'Replace artwork' }).click()
  await page.getByText('Artwork replaced.').waitFor()
  const integratedArtwork = await page.evaluate(async id => (await (await fetch(`/api/v1/apps/${id}/artwork`)).json()).items, draftId)
  if (integratedArtwork.length !== 3 || new Set(integratedArtwork.map(item => item.role)).size !== 3 ||
      !['icon', 'logo', 'steam.hero'].every(role => integratedArtwork.some(item => item.role === role))) throw new Error('Artwork bindings were not current and unique by role')
  const expectedIconDigest = createHash('sha256').update(await readFile(artworkFixtures.jpeg)).digest('hex')
  if (integratedArtwork.find(item => item.role === 'icon').digest !== expectedIconDigest) throw new Error('Icon replacement did not select the new CAS content')
  const iconBytes = await page.evaluate(async id => Array.from(new Uint8Array(await (await fetch(`/api/v1/apps/${id}/artwork/icon/content`)).arrayBuffer())), draftId)
  if (createHash('sha256').update(Buffer.from(iconBytes)).digest('hex') !== expectedIconDigest) throw new Error('Current icon bytes differ from selected digest')
  if (preview) await restartLanPreview(preview)
  await page.reload()
  await page.getByRole('heading', { name: 'Artwork', exact: true }).waitFor()
  const afterRestart = await page.evaluate(async ({ appId, releaseId, associationId }) => {
    const get = async path => (await (await fetch(`/api/v1${path}`)).json())
    return { app: await get(`/apps/${appId}/declarations`), release: await get(`/releases/${releaseId}/declarations`),
      association: await get(`/associations/${associationId}/declarations`), artwork: await get(`/apps/${appId}/artwork`) }
  }, { appId: draftId, releaseId, associationId: treeAssociationId })
  if (afterRestart.app.data.appDepot?.kind !== 'tool' || afterRestart.app.data.appDepot?.tags?.length !== 2 ||
      afterRestart.release.data.appDepot?.platforms?.length !== 2 || afterRestart.association.data.appDepot?.entryPoints?.length !== 1 ||
      afterRestart.artwork.items.length !== 3) throw new Error('Integrated App state did not survive restart')
  await page.goto(`${base}apps/${draftId}/releases/${firstReleaseId}`)
  await page.getByRole('button', { name: 'Publish release' }).click()
  await page.getByText('Release published. Artifact composition is now immutable.').waitFor()
  await page.goto(`${base}apps/${draftId}/channels`)
  await page.getByRole('heading', { name: 'Channels', exact: true }).waitFor()
  await page.getByLabel('Channel name').fill('preview')
  await page.getByLabel('Published Release', { exact: true }).fill(releaseId)
  await page.getByRole('button', { name: 'Create channel' }).click()
  await page.getByText('Channel created.').waitFor()
  const staleChannelEtag = await page.evaluate(async id => (await fetch(`/api/v1/apps/${id}/channels/preview`)).headers.get('ETag'), draftId)
  await page.getByLabel('Move to Published Release', { exact: true }).fill(firstReleaseId)
  await page.getByRole('button', { name: 'Move channel' }).click()
  await page.getByText('Channel moved.').waitFor({ timeout: 5000 }).catch(async () => { throw new Error(`Channel move failed: ${await page.locator('main').innerText()}`) })
  const staleChannel = await page.evaluate(async ({ id, etag, releaseId }) => {
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    return (await fetch(`/api/v1/apps/${id}/channels/preview`, { method: 'PUT', headers: { 'Content-Type': 'application/json',
      'X-CSRF-Token': session.csrfToken, 'If-Match': etag }, body: JSON.stringify({ releaseId }) })).status
  }, { id: draftId, etag: staleChannelEtag, releaseId })
  if (staleChannel !== 412) throw new Error(`Stale Channel ETag was accepted: ${staleChannel}`)
  await page.getByRole('button', { name: 'Movement history' }).click()
  await page.getByRole('link', { name: `Release ${firstReleaseId}` }).last().waitFor()
  await page.getByRole('button', { name: 'Delete channel preview' }).click()
  await page.getByRole('dialog', { name: 'Delete channel preview' }).getByRole('button', { name: 'Delete channel' }).click()
  await page.getByRole('heading', { name: 'No channels' }).waitFor()
  await page.goto(`${base}apps/${draftId}/releases/${firstReleaseId}`)
  await page.getByRole('button', { name: 'Delete release' }).click()
  await page.keyboard.press('Escape')
  if (await page.getByRole('dialog', { name: 'Delete release' }).isVisible()) throw new Error('Confirmation dialog did not close with Escape')
  await page.getByRole('button', { name: 'Delete release' }).click()
  await page.getByRole('dialog', { name: 'Delete release' }).getByRole('button', { name: 'Delete release' }).click()
  await page.getByRole('heading', { name: 'Delete release operation' }).waitFor()
  await page.getByText('Access has been revoked.').waitFor()
  }
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
  await page.getByRole('link', { name: preview ? 'Portal Draft Edited' : 'Portal Draft', exact: true }).waitFor()
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
  await page.getByLabel('File', { exact: true }).setInputFiles({ name: 'a.txt', mimeType: 'text/plain', buffer: Buffer.from('A') })
  keys.length = 0
  await page.getByRole('button', { name: 'Upload and publish' }).click()
  await page.getByText(`Published ${name}`).waitFor({ timeout: 30000 })
  await page.getByRole('link', { name: 'Open application' }).click()
  await page.waitForURL(/\/apps\/[0-9a-f-]{36}$/)
  const privateAppId = new URL(page.url()).pathname.split('/').pop()
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
  if (preview) {
    await verifyRestoredSnapshot(preview, page, manifest, password, integratedIds)
    const sourceState = await page.evaluate(async ({ appId, associationId }) => ({
      app: await (await fetch(`/api/v1/apps/${appId}/declarations`)).json(),
      association: await (await fetch(`/api/v1/associations/${associationId}/declarations`)).json(),
      artwork: await (await fetch(`/api/v1/apps/${appId}/artwork`)).json(),
    }), integratedIds)
    if (sourceState.app.data.appDepot?.kind !== 'tool' || sourceState.association.data.appDepot?.entryPoints?.length !== 1 || sourceState.artwork.items.length !== 3) {
      throw new Error('Source instance changed during isolated offline restore')
    }
    await page.goto(`${base}apps/${draftId}/settings`)
    await page.getByRole('button', { name: 'Delete application' }).click()
    await page.getByRole('dialog', { name: 'Delete application' }).getByRole('button', { name: 'Delete application' }).click()
    await page.getByRole('heading', { name: 'Delete application operation' }).waitFor()
    await page.getByText('Access has been revoked.').waitFor()
    const deleteOperationId = new URL(page.url()).searchParams.get('operation')
    await page.goto(`${base}operations/${deleteOperationId}`)
    await page.getByRole('heading', { name: 'Operation status' }).waitFor()
    await page.getByText('Access has been revoked.').waitFor()
    await page.reload()
    await page.getByRole('heading', { name: 'Operation status' }).waitFor()
    await page.goto(`${base}admin/diagnostics`)
    await page.getByRole('heading', { name: 'Diagnostics' }).waitFor()
    await page.getByText('95th percentile latency').waitFor()
    if ((await page.locator('main').innerText()).includes(preview.namespace)) throw new Error('Diagnostics exposed a private data-root path')
    await page.goto(`${base}admin/settings`)
    await page.getByRole('heading', { name: 'Server Settings' }).waitFor()
    const staleSettingsEtag = await page.evaluate(async () => (await fetch('/api/v1/settings')).headers.get('ETag'))
    await page.getByLabel('Enable anonymous read').check()
    await page.getByRole('button', { name: 'Save setting' }).click()
    await page.getByText('Server settings saved.').waitFor()
    const staleSettings = await page.evaluate(async etag => {
      const session = await (await fetch('/api/v1/auth/sessions/current')).json()
      return (await fetch('/api/v1/settings', { method: 'PATCH', headers: { 'Content-Type': 'application/json',
        'X-CSRF-Token': session.csrfToken, 'If-Match': etag }, body: JSON.stringify({ anonymousReadEnabled: false }) })).status
    }, staleSettingsEtag)
    if (staleSettings !== 412) throw new Error(`Stale settings ETag was accepted: ${staleSettings}`)
    await page.getByLabel('Enable anonymous read').uncheck()
    await page.getByRole('button', { name: 'Save setting' }).click()
    await page.getByText('Server settings saved.').waitFor()
    await page.goto(`${base}admin/users`)
    await page.getByRole('heading', { name: 'Users & Access' }).waitFor()
    await page.getByLabel('Username').fill('managed')
    await page.getByLabel('Initial password').fill('managed password 12345')
    await page.getByRole('button', { name: 'Create user' }).click()
    await page.getByText('User created.').waitFor()
    await page.locator('tr').filter({ hasText: 'managed' }).getByRole('button', { name: 'Manage' }).click()
    const staleUserEtag = await page.evaluate(async () => {
      const users = await (await fetch('/api/v1/users?pageSize=20')).json()
      const managed = users.items.find(user => user.username === 'managed')
      return { id: managed.id, etag: (await fetch(`/api/v1/users/${managed.id}`)).headers.get('ETag') }
    })
    if (!staleUserEtag.id || !staleUserEtag.etag) throw new Error('Managed user was not available for ETag check')
    await page.getByLabel('Publish permission', { exact: true }).check()
    await page.getByRole('button', { name: 'Save permissions' }).click()
    await page.getByRole('dialog', { name: 'Save permissions' }).getByRole('button', { name: 'Apply permissions' }).click()
    await page.getByText('Permissions saved.').waitFor()
    const staleUserStatus = await page.evaluate(async ({ id, etag }) => {
      const session = await (await fetch('/api/v1/auth/sessions/current')).json()
      return (await fetch(`/api/v1/users/${id}`, { method: 'PATCH', headers: { 'Content-Type': 'application/json',
        'X-CSRF-Token': session.csrfToken, 'If-Match': etag }, body: JSON.stringify({ publishPermission: false }) })).status
    }, staleUserEtag)
    if (staleUserStatus !== 412) throw new Error(`Stale user ETag was accepted: ${staleUserStatus}`)
    await page.getByLabel('Publish permission', { exact: true }).uncheck()
    await page.getByRole('button', { name: 'Save permissions' }).click()
    await page.getByRole('dialog', { name: 'Save permissions' }).getByRole('button', { name: 'Apply permissions' }).click()
    await page.getByText('Permissions saved.').waitFor()
    await page.getByLabel('Server Admin', { exact: true }).check()
    await page.getByRole('button', { name: 'Save permissions' }).click()
    await page.getByRole('dialog', { name: 'Save permissions' }).getByRole('button', { name: 'Apply permissions' }).click()
    await page.getByText('Permissions saved.').waitFor()
    await page.getByLabel('Server Admin', { exact: true }).uncheck()
    await page.getByRole('button', { name: 'Save permissions' }).click()
    await page.getByRole('dialog', { name: 'Save permissions' }).getByRole('button', { name: 'Apply permissions' }).click()
    await page.getByText('Permissions saved.').waitFor()
    if (await page.getByRole('button', { name: 'Delete user' }).count()) throw new Error('Unsupported user deletion was exposed')
    await page.goto(`${base}account`)
    await page.getByRole('heading', { name: 'Account' }).waitFor()
    await page.getByText('No active token').waitFor()
    await page.getByRole('button', { name: 'Issue token' }).click()
    await page.getByRole('dialog', { name: 'Issue token' }).getByRole('button', { name: 'Issue token' }).click()
    const oldToken = await page.getByLabel('New token (shown once)').inputValue()
    if (!oldToken) throw new Error('One-time token was not shown')
    await page.goto(`${base}admin/users`)
    await page.goto(`${base}account`)
    if (await page.getByLabel('New token (shown once)').count()) throw new Error('Plaintext token survived navigation')
    await page.getByRole('button', { name: 'Replace token' }).click()
    await page.getByRole('dialog', { name: 'Replace token' }).getByRole('button', { name: 'Replace token' }).click()
    const newToken = await page.getByLabel('New token (shown once)').inputValue()
    if (!newToken || newToken === oldToken) throw new Error('Token replacement did not issue a new secret')
    const oldStatus = await page.evaluate(async token => (await fetch('/api/v1/me/token', { credentials: 'omit', headers: { Authorization: `Bearer ${token}` } })).status, oldToken)
    if (oldStatus !== 401) throw new Error(`Replaced token remained valid: ${oldStatus}`)
    const persistedToken = await page.evaluate(token => [...Object.values(localStorage), ...Object.values(sessionStorage)].some(value => value.includes(token)), newToken)
    if (persistedToken) throw new Error('Plaintext token was stored in browser persistence')
    await page.getByRole('button', { name: 'Dismiss token' }).click()
    if (await page.getByLabel('New token (shown once)').count()) throw new Error('Token display was not cleared')
    await page.getByRole('button', { name: 'Revoke my token' }).click()
    await page.getByRole('dialog', { name: 'Revoke my token' }).getByRole('button', { name: 'Revoke token' }).click()
    await page.getByText('Personal API token revoked.').waitFor()
    const newStatus = await page.evaluate(async token => (await fetch('/api/v1/me/token', { credentials: 'omit', headers: { Authorization: `Bearer ${token}` } })).status, newToken)
    if (newStatus !== 401) throw new Error(`Revoked token remained valid: ${newStatus}`)
    await page.goto(`${base}admin/backups`)
    await page.getByRole('heading', { name: 'Backups', exact: true }).waitFor()
    await page.getByRole('button', { name: 'Start online backup' }).click()
    await page.getByRole('heading', { name: 'Backup operation' }).waitFor()
    const uiBackupOperation = new URL(page.url()).searchParams.get('operation')
    await page.goto(`${base}operations/${uiBackupOperation}`)
    await page.getByRole('heading', { name: 'Operation status' }).waitFor()
    await page.getByText('Backup output available now: Yes.').waitFor({ timeout: 30000 })
    await page.goto(`${base}admin/backups`)
    await page.getByRole('button', { name: 'Inspect' }).first().click()
    await page.getByRole('heading', { name: 'Backup manifest' }).waitFor()
    const manifestDownload = page.waitForEvent('download')
    await page.getByRole('button', { name: 'Download manifest.json' }).click()
    const downloadedManifest = await manifestDownload
    if (downloadedManifest.suggestedFilename() !== 'manifest.json') throw new Error('Portable manifest export was not offered')
    await page.getByRole('button', { name: 'Verify' }).first().click()
    await page.getByText('Verified size and SHA-256').first().waitFor({ timeout: 30000 })
    const backupMemberLink = await page.getByRole('link', { name: 'Download', exact: true }).first().getAttribute('href')
    const memberBytes = await page.evaluate(async href => Array.from(new Uint8Array(await (await fetch(href)).arrayBuffer())), backupMemberLink)
    const firstMember = await page.evaluate(async id => (await (await fetch(`/api/v1/backups/${id}`)).json()).members[0], new URL(page.url()).searchParams.get('backup'))
    if (createHash('sha256').update(Buffer.from(memberBytes)).digest('hex') !== firstMember.sha256) throw new Error('Backup member download hash mismatch')
    await page.goto(`${base}admin/backups?backup=00000000-0000-4000-8000-000000000000`)
    await page.getByText('This item is unavailable or you do not have access to it.').waitFor()
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
  const userListRequests = []
  reader.on('request', request => { if (request.url().includes('/admin/diagnostics')) diagnosticRequests.push(request.url()) })
  reader.on('request', request => { if (request.url().includes('/api/v1/users?')) userListRequests.push(request.url()) })
  await reader.goto(`${base}apps/new`)
  await reader.getByRole('heading', { name: 'Sign in' }).waitFor()
  await reader.getByLabel('Username').fill('reader')
  await reader.getByLabel('Password').fill(readerPassword)
  await reader.getByRole('button', { name: 'Sign in' }).click()
  await reader.getByRole('heading', { name: 'Create application' }).waitFor()
  await reader.getByText('Publish permission is required to create an application.').waitFor()
  await reader.goto(`${base}apps/${privateAppId}/settings`)
  await reader.getByRole('heading', { name: 'Application unavailable' }).waitFor()
  await reader.goto(base)
  await reader.getByRole('heading', { name: 'Server overview' }).waitFor()
  if (diagnosticRequests.length) throw new Error('Non-Admin browser requested protected diagnostics')
  if (await reader.getByRole('navigation', { name: 'Server administration' }).count()) throw new Error('Non-Admin sees administration navigation')
  await reader.goto(`${base}admin/users`)
  await reader.getByRole('heading', { name: 'Access denied' }).waitFor()
  if (userListRequests.length) throw new Error('Non-Admin Portal requested protected Users API')
  if (preview) {
    await reader.goto(`${base}account`)
    await reader.getByRole('heading', { name: 'Account' }).waitFor()
    await reader.getByRole('button', { name: 'Issue token' }).click()
    await reader.getByRole('dialog', { name: 'Issue token' }).getByRole('button', { name: 'Issue token' }).click()
    const readerToken = await reader.getByLabel('New token (shown once)').inputValue()
    if (!readerToken) throw new Error('Reader token was not issued')
    await page.goto(`${base}admin/users`)
    await page.locator('tr').filter({ hasText: 'reader' }).getByRole('button', { name: 'Manage' }).click()
    await page.getByRole('button', { name: 'Revoke personal token' }).click()
    await page.getByRole('dialog', { name: 'Revoke personal token' }).getByRole('button', { name: 'Revoke token' }).click()
    await page.getByText('Personal API token revoked, if one was active.').waitFor()
    const revokedReaderStatus = await reader.evaluate(async token => (await fetch('/api/v1/me/token', { credentials: 'omit', headers: { Authorization: `Bearer ${token}` } })).status, readerToken)
    if (revokedReaderStatus !== 401) throw new Error(`Admin token revocation failed: ${revokedReaderStatus}`)
    await page.getByLabel('Server Admin', { exact: true }).check()
    await page.getByRole('button', { name: 'Save permissions' }).click()
    await page.getByRole('dialog', { name: 'Save permissions' }).getByRole('button', { name: 'Apply permissions' }).click()
    await page.getByText('Permissions saved.').waitFor()
    await reader.evaluate(() => window.dispatchEvent(new Event('focus')))
    await reader.getByRole('navigation', { name: 'Server administration' }).waitFor()
    await page.getByLabel('Server Admin', { exact: true }).uncheck()
    await page.getByRole('button', { name: 'Save permissions' }).click()
    await page.getByRole('dialog', { name: 'Save permissions' }).getByRole('button', { name: 'Apply permissions' }).click()
    await page.getByText('Permissions saved.').waitFor()
    await reader.evaluate(() => window.dispatchEvent(new Event('focus')))
    await reader.getByRole('navigation', { name: 'Server administration' }).waitFor({ state: 'detached' })
    await page.evaluate(async keys => {
      const session = await (await fetch('/api/v1/auth/sessions/current')).json()
      for (let index = 0; index < keys.length; index++) {
        const response = await fetch('/api/v1/users', { method: 'POST', headers: { 'Content-Type': 'application/json',
          'X-CSRF-Token': session.csrfToken, 'Idempotency-Key': keys[index] },
          body: JSON.stringify({ username: `pagination-${index}`, password: 'pagination password 12345', publishPermission: false }) })
        if (response.status !== 201) throw new Error(`Pagination fixture failed: ${response.status}`)
      }
    }, Array.from({ length: 21 }, () => randomUUID()))
    await page.reload()
    await page.getByRole('button', { name: 'Next page' }).click()
    await page.getByRole('button', { name: 'Previous page' }).click()
    await page.getByRole('heading', { name: 'Users & Access' }).waitFor()
  }
  await reader.locator('.account-menu summary').click()
  await reader.locator('.account-menu').getByRole('button', { name: 'Sign out' }).click()
  await reader.getByRole('heading', { name: 'Sign in' }).waitFor()
  await reader.close()
  await page.evaluate(async () => {
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    const response = await fetch('/api/v1/auth/sessions/current', { method: 'DELETE', headers: { 'X-CSRF-Token': session.csrfToken } })
    if (!response.ok) throw new Error(`Session invalidation failed: ${response.status}`)
  })
  await page.reload()
  await page.getByRole('heading', { name: 'Sign in' }).waitFor()
  console.log('Browser smoke passed: setup, auth, App/Release management, File/Tree/ZIP Explorer and entry points, metadata, artwork, declarations, channels, deletion, administration, account, operations, LAN HTTP, backup and isolated restore')
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
