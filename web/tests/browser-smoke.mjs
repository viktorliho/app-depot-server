import { chromium } from 'playwright-core'
import { createHash, randomUUID } from 'node:crypto'
import { execFileSync, spawn } from 'node:child_process'
import { chmod, cp, mkdir, mkdtemp, open, readFile, realpath, rm, stat, symlink, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

async function waitForArtworkImage(page, selector, width, height, label) {
  try {
    await page.locator(selector).scrollIntoViewIfNeeded({ timeout: 10000 })
    await page.waitForFunction(({ selector, width, height }) => {
      const image = document.querySelector(selector)
      return image instanceof HTMLImageElement && image.complete && image.naturalWidth === width && image.naturalHeight === height
    }, { selector, width, height }, { timeout: 10000 })
  } catch (cause) {
    const details = await page.evaluate(async selector => {
      const image = document.querySelector(selector)
      const url = image instanceof HTMLImageElement ? image.currentSrc || image.src : null
      let httpStatus = null
      let fetchError = null
      if (url) {
        try { httpStatus = (await fetch(url, { signal: AbortSignal.timeout(3000) })).status }
        catch (error) { fetchError = error instanceof Error ? error.message : String(error) }
      }
      return { url, complete: image instanceof HTMLImageElement ? image.complete : null,
        naturalWidth: image instanceof HTMLImageElement ? image.naturalWidth : null,
        naturalHeight: image instanceof HTMLImageElement ? image.naturalHeight : null, httpStatus, fetchError }
    }, selector).catch(error => ({ diagnosticError: String(error) }))
    throw new Error(`${label}: ${JSON.stringify(details)}`, { cause })
  }
}

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

const contentRows = (scope, name) => scope.getByRole('listbox', { name: 'Directory contents' }).getByRole('option', { name })
const entryEditor = page => page.getByRole('dialog', { name: 'Entry points' })
async function openEntryEditor(page) {
  await page.getByRole('button', { name: 'Entry points', exact: true }).click()
  await entryEditor(page).waitFor()
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

async function verifyVisualRecipe(page, base, preview, treeAssociationId) {
  await page.goto(`${base}artifacts/new-app`, { waitUntil: 'domcontentloaded' })
  await page.getByLabel('App name').fill('Synthetic native source port')
  await page.getByLabel('File', { exact: true }).setInputFiles({ name: 'source-port', mimeType: 'application/octet-stream', buffer: Buffer.from('synthetic executable bytes') })
  await page.getByRole('button', { name: 'Upload and publish' }).click()
  await page.getByText('Published Synthetic native source port').waitFor()

  await page.goto(`${base}apps/new`, { waitUntil: 'domcontentloaded' })
  await page.getByLabel('Application name').fill('Synthetic derived edition')
  await page.getByLabel('Application kind').selectOption('derived')
  await page.getByRole('button', { name: 'Create Draft application' }).click()
  await page.getByRole('heading', { name: 'Synthetic derived edition' }).waitFor()
  await page.getByText('Derived', { exact: true }).waitFor()
  await page.getByRole('link', { name: 'Create Draft release' }).first().click()
  await page.getByLabel('Version (optional)').fill('Edition 1')
  await page.getByRole('button', { name: 'Create Draft release' }).click()
  const editor = page.locator('.recipe-panel')
  await editor.getByText('Add dependency application').click()
  for (const name of ['Portal Draft Edited', 'Synthetic native source port']) {
    await editor.getByLabel('Search dependency applications').fill(name)
    await editor.getByRole('button', { name: 'Search', exact: true }).click()
    await editor.getByRole('button', { name: `Add ${name}`, exact: true }).click()
  }
  const game = editor.getByRole('group', { name: 'Portal Draft Edited', exact: true })
  await game.getByLabel('Release policy').selectOption('release')
  const releasePicker = game.getByLabel('Published release', { exact: true })
  await releasePicker.locator('option').nth(1).waitFor({ state: 'attached', timeout: 7000 }).catch(async cause => {
    throw new Error(`Release picker did not load: ${await game.innerText()}`, { cause })
  })
  // The most recently published release also has the Tree Artifact for this fixture.
  await releasePicker.selectOption({ index: 1 })
  const first = editor.getByRole('group', { name: 'Copy 1', exact: true })
  const artifactChoice = first.getByLabel('Source Artifact')
  await artifactChoice.selectOption(`id:${treeAssociationId}`)
  await first.getByText('Browse source tree').click()
  await contentRows(first, /^browser-tree Folder/).dblclick()
  await contentRows(first, /^nested Folder/).dblclick()
  await first.getByRole('button', { name: 'Use this directory' }).click()
  await first.getByLabel('Destination path').fill('assets')

  const second = editor.getByRole('group', { name: 'Copy 2', exact: true })
  await second.getByLabel('Destination path').fill('bin/source-port')
  await second.getByLabel('Mark selected file executable').check()
  await second.getByRole('button', { name: 'Move up' }).click()
  await editor.getByRole('button', { name: 'Preview installation layout' }).click()
  await editor.locator('.recipe-layout').getByText('assets/tree.txt', { exact: true }).waitFor()
  await editor.getByLabel('Launch executable').selectOption('bin/source-port')
  await editor.getByLabel('Working directory').fill('assets')
  await editor.getByLabel('Arguments (one literal argument per line)').fill('--content\nassets')
  await editor.getByRole('button', { name: 'Save and validate recipe' }).click()
  await editor.getByText('Draft saved. All targets validated.').waitFor()
  await editor.getByRole('button', { name: 'Publish release' }).click()
  await editor.getByRole('heading', { name: 'Currently resolved components' }).waitFor()
  await editor.locator('.recipe-layout').getByText('assets/tree.txt', { exact: true }).waitFor()
  if (preview) await restartLanPreview(preview)
  await page.reload({ waitUntil: 'domcontentloaded' })
  await editor.locator('.recipe-layout').getByText('assets/tree.txt', { exact: true }).waitFor()
  await editor.getByText('bin/source-port', { exact: true }).first().waitFor()
  if (await editor.getByRole('button', { name: 'Save and validate recipe' }).count()) throw new Error('Published recipe was editable')
}

async function verifyGogImporter(page, base, inbox, fixtures, preview) {
  await cp(join(fixtures, 'multi'), join(inbox, 'synthetic-gog'), { recursive: true })
  await page.goto(`${base}artifacts/new-app`, { waitUntil: 'domcontentloaded' })
  await page.getByLabel('Source').selectOption('server')
  await page.getByLabel('Artifact type').selectOption('GogOfflineInstaller')
  await page.getByRole('button', { name: 'synthetic-gog', exact: true }).click()
  await page.getByRole('button', { name: 'Select this directory' }).click()
  await page.getByRole('button', { name: 'Analyze installer' }).click()
  const review = page.getByRole('group', { name: 'Installer review' })
  await review.waitFor({ timeout: 20000 })
  await review.getByText('Installer evidence and source volumes').click()
  await review.getByText('InnoSetup 5.5.7', { exact: false }).waitFor()
  await review.getByLabel('Game version (optional)').fill('fixture-v1')
  await page.getByLabel('Confirmed application language').fill('ru-RU')
  await page.getByLabel('App name').fill('Synthetic GOG import')
  if (await page.getByRole('button', { name: 'Import and publish', exact: true }).count()) throw new Error('Installer import offered automatic publication')
  await page.getByRole('button', { name: 'Import and save Draft', exact: true }).click()
  await page.getByText('Imported into a Draft release.', { exact: true }).waitFor({ timeout: 30000 })
  await page.getByRole('link', { name: 'Open release', exact: true }).click()
  await page.getByRole('button', { name: 'Publish release' }).waitFor()
  const release = new URL(page.url()).pathname.split('/').pop()
  const manifest = await page.evaluate(async id => {
    const snapshot = await fetch(`/api/v1/releases/${id}/declaration-snapshot`).then(response => response.json())
    return snapshot.associations[0]?.declaration.data.appDepot?.gogOfflineInstaller
  }, release)
  if (manifest?.payload.kind !== 'Tree' || manifest.analysis.volumes.length !== 3 ||
      manifest.user.gameVersion !== 'fixture-v1' || manifest.installReadiness !== 'Unknown') {
    throw new Error('GOG installation declaration is missing or inaccurate')
  }
  await page.getByRole('button', { name: 'Publish release' }).click()
  await page.getByText('Release published. Artifact composition is now immutable.').waitFor()
  const legacyRelease = page.url()
  const removed = await page.evaluate(async id => {
    const path = `/api/v1/releases/${id}/declarations`
    const session = await fetch('/api/v1/auth/sessions/current').then(response => response.json())
    const current = await fetch(path)
    const { data } = await current.json()
    if (data.appDepot?.installation?.version !== 1 || data.appDepot.installation.options?.length || data.appDepot.installation.prerequisites?.length || data.appDepot.installation.localization?.language !== 'ru-RU') throw new Error('Invalid GOG installation seed')
    delete data.appDepot.installation
    return (await fetch(path, { method: 'PUT', headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': session.csrfToken,
      'If-Match': current.headers.get('ETag') }, body: JSON.stringify({ data }) })).status
  }, release)
  if (removed !== 200) throw new Error('Could not prepare legacy Published GOG fixture')
  await page.goto(legacyRelease)
  await page.getByRole('link', { name: 'Installation', exact: true }).click()
  const install = page.getByRole('heading', { name: 'Installation Configuration' }).locator('..')
  await install.getByText('Detect installation components', { exact: true }).click()
  await install.getByRole('button', { name: 'Detect components', exact: true }).click()
  await install.getByText('No recognized packages.', { exact: true }).waitFor()
  await install.getByRole('button', { name: 'Apply to installation editor' }).click()
  await install.getByRole('button', { name: /^Localization/ }).click()
  await install.getByLabel('Localization packaging').selectOption('bundled')
  await install.getByLabel('Language identifiers (comma-separated)').fill('en-US, ru-RU')
  await install.getByRole('button', { name: /^Review installation plan/ }).click()
  await install.getByRole('button', { name: 'Validate and preview' }).click()
  await install.getByText(/files · .* directories/).waitFor()
  await install.getByRole('button', { name: 'Save installation configuration' }).click()
  await install.getByText('Installation configuration saved as a new declaration revision.').waitFor()
  const savedInstallation = await page.evaluate(async id => (await fetch(`/api/v1/releases/${id}/declarations`).then(response => response.json())).data.appDepot.installation, release)
  if (savedInstallation.localization?.mode !== 'bundled' || savedInstallation.localization.languages.join(',') !== 'en-US,ru-RU' || savedInstallation.options?.length) throw new Error('Bundled localization was not saved independently from options')
  if (createHash('sha256').update(await readFile(join(inbox, 'synthetic-gog/spanning-1b.bin'))).digest('hex') !==
      createHash('sha256').update(await readFile(join(fixtures, 'multi/spanning-1b.bin'))).digest('hex')) {
    throw new Error('GOG import modified an Inbox volume')
  }

  await cp(join(fixtures, 'launch'), join(inbox, 'synthetic-launch'), { recursive: true })
  await page.goto(`${base}artifacts/new-app`, { waitUntil: 'domcontentloaded' })
  await page.getByLabel('Source').selectOption('server')
  await page.getByLabel('Artifact type').selectOption('GogOfflineInstaller')
  await page.getByRole('button', { name: 'synthetic-launch', exact: true }).click()
  await page.getByRole('button', { name: 'Select this directory' }).click()
  await page.getByRole('button', { name: 'Analyze installer' }).click()
  const candidate = review.getByLabel('Candidate launch file')
  const executableOption = candidate.locator('option').filter({ hasText: /app\.exe/ })
  await executableOption.waitFor({ state: 'attached' })
  const executable = await executableOption.textContent()
  await candidate.selectOption({ label: executable })
  const generate = review.getByLabel('Create launch entry point from selected executable')
  if (!(await generate.isChecked())) throw new Error('Launch creation was not offered by default')
  await generate.uncheck()
  await generate.check()
  await review.getByRole('combobox', { name: 'Working directory', exact: true }).selectOption('custom')
  await review.getByLabel('Working directory path').fill(executable.includes('/') ? 'app/docs' : 'docs')
  await review.getByRole('button', { name: 'Add launch argument' }).click()
  await review.getByLabel('Launch argument 1').fill('--windowed mode')
  await page.getByLabel('App name').fill('Synthetic GOG launch')
  await page.getByRole('button', { name: 'Import and save Draft', exact: true }).click()
  await page.getByText('Imported into a Draft release.', { exact: true }).waitFor({ timeout: 30000 })
  await page.getByRole('link', { name: 'Open release', exact: true }).click()
  await page.getByRole('link', { name: 'Installation', exact: true }).click()
  await page.getByRole('heading', { name: 'Installation information' }).waitFor()
  await page.getByText('Declared launch file', { exact: true }).waitFor()
  const releaseUrl = page.url()
  if (await page.locator('.json-preview').count()) throw new Error('Release overview rendered advanced JSON')
  await page.getByRole('link', { name: 'Launch', exact: true }).click()
  await page.getByRole('link', { name: 'Edit entry points' }).click()
  await page.getByText('Portable launches: Valid').waitFor()
  if (await page.getByLabel('Argument 1').inputValue() !== '--windowed mode') throw new Error('Generated launch arguments changed')
  const association = new URL(page.url()).pathname.split('/').pop()
  // A v0.5.0 association contains the manifest and no entryPoints field.
  const legacyStatus = await page.evaluate(async id => {
    const path = `/api/v1/associations/${id}/declarations`
    const session = await fetch('/api/v1/auth/sessions/current').then(response => response.json())
    const before = await fetch(path)
    const { data } = await before.json()
    delete data.appDepot.entryPoints
    data.appDepot.custom = { keep: true }
    return (await fetch(path, { method: 'PUT', headers: { 'Content-Type': 'application/json',
      'X-CSRF-Token': session.csrfToken, 'If-Match': before.headers.get('ETag') }, body: JSON.stringify({ data }) })).status
  }, association)
  if (legacyStatus !== 200) throw new Error(`Could not prepare v0.5.0 declaration: ${legacyStatus}`)
  await page.goto(releaseUrl)
  await page.getByText('Selected launch candidate (user provided)', { exact: true }).waitFor()
  await page.getByRole('button', { name: 'Publish release' }).click()
  await page.getByText('Release published. Artifact composition is now immutable.').waitFor()
  await page.getByRole('link', { name: 'Open entry point editor' }).click()
  await page.getByRole('button', { name: 'Create entry point from GOG candidate' }).click()
  const concurrentStatus = await page.evaluate(async id => {
    const path = `/api/v1/associations/${id}/declarations`
    const session = await fetch('/api/v1/auth/sessions/current').then(response => response.json())
    const before = await fetch(path)
    const { data } = await before.json()
    data.appDepot.custom.concurrent = true
    return (await fetch(path, { method: 'PUT', headers: { 'Content-Type': 'application/json',
      'X-CSRF-Token': session.csrfToken, 'If-Match': before.headers.get('ETag') }, body: JSON.stringify({ data }) })).status
  }, association)
  if (concurrentStatus !== 200) throw new Error(`Could not write concurrent declaration: ${concurrentStatus}`)
  const staleSave = page.waitForResponse(response => response.url().endsWith(`/associations/${association}/declarations`) && response.request().method() === 'PUT')
  await page.getByRole('button', { name: 'Save entry points' }).click()
  if ((await staleSave).status() !== 412) throw new Error('Stale entry point editor overwrote the current declaration')
  await page.getByRole('button', { name: 'Reload saved entry points' }).click()
  await page.getByRole('button', { name: 'Create entry point from GOG candidate' }).click()
  await page.getByRole('button', { name: 'Save entry points' }).click()
  await page.getByText('Entry points saved as a new declaration revision.').waitFor()
  await restartLanPreview(preview)
  await page.reload()
  await page.getByText('Portable launches: Valid').waitFor()
  const preserved = await page.evaluate(async id => (await fetch(`/api/v1/associations/${id}/declarations`).then(response => response.json())).data.appDepot, association)
  if (preserved.gogOfflineInstaller?.user.candidateExecutable !== executable || preserved.custom?.keep !== true || preserved.custom?.concurrent !== true ||
      preserved.entryPoints?.[0]?.platforms?.[0] !== 'windows/x86_64') throw new Error('Legacy conversion lost declaration data')

}

async function stopServer(child, label) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return
  const waitForExit = timeoutMs => new Promise(resolve => {
    const onExit = () => { clearTimeout(timer); resolve(true) }
    const timer = setTimeout(() => { child.off('exit', onExit); resolve(false) }, timeoutMs)
    child.once('exit', onExit)
  })
  const gracefulExit = waitForExit(10000)
  child.kill('SIGTERM')
  if (await gracefulExit) return
  console.error(`Browser smoke: ${label} did not stop after SIGTERM; sending SIGKILL`)
  const forcedExit = waitForExit(5000)
  child.kill('SIGKILL')
  if (!await forcedExit) throw new Error(`${label} did not exit after SIGKILL`)
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
    // API request logs can fill an unread child pipe and stall the Server.
    restoredServer.stdout.resume()
    restoredServer.stderr.on('data', chunk => { errors = (errors + chunk.toString()).slice(-2000) })
    const rootUrl = `http://127.0.0.1:${port}`
    const deadline = Date.now() + 15000
    let ready = false
    while (Date.now() < deadline) {
      if (restoredServer.exitCode !== null) throw new Error(`Restored Server exited: ${errors}`)
      try { if ((await fetch(`${rootUrl}/api/v1/health/ready`, { signal: AbortSignal.timeout(1500) })).ok) { ready = true; break } } catch { /* startup */ }
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
    await stopServer(restoredServer, 'restored Server')
    await rm(scratch, { recursive: true, force: true })
  }
}

async function startLanPreview() {
  const repository = fileURLToPath(new URL('../..', import.meta.url))
  const binary = join(repository, 'target/debug/app-depot-server')
  const namespace = await mkdtemp(join(tmpdir(), 'app-depot-lan-smoke-'))
  const importSource = await mkdtemp(join(tmpdir(), 'app-depot-import-source-'))
  await writeFile(join(importSource, 'server-file.txt'), 'Server-side import fixture')
  await mkdir(join(importSource, 'server-tree/empty'), { recursive: true })
  await mkdir(join(importSource, 'server-tree/nested'), { recursive: true })
  await writeFile(join(importSource, 'server-tree/nested/content.txt'), 'Server tree content')
  execFileSync('python3', ['-c', 'import sys,zipfile; z=zipfile.ZipFile(sys.argv[1],"w",zipfile.ZIP_DEFLATED); z.writestr("zip/nested.txt","Server ZIP content"); z.close()', join(importSource, 'server.zip')])
  const importRoots = JSON.stringify([{ id: 'shared', path: await realpath(importSource), allowedUserIds: [] }])
  const listener = createServer()
  await new Promise(resolve => listener.listen(0, '127.0.0.1', resolve))
  const port = listener.address().port
  await new Promise(resolve => listener.close(resolve))
  let server
  try {
    execFileSync(binary, ['init-preview', '--namespace', namespace, '--instance', 'preview'])
    server = spawn(binary, ['serve', '--namespace', namespace, '--instance', 'preview', '--mode', 'preview',
      '--bind', `127.0.0.1:${port}`, '--assets', join(repository, 'web/dist')], { stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, APP_DEPOT_IMPORT_ROOTS: importRoots, APP_DEPOT_SMOKE_TRACE: '1' } })
    let output = ''
    let errors = ''
    server.stderr.on('data', chunk => { errors = (errors + chunk.toString()).slice(-8000) })
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
        const response = await fetch(`http://127.0.0.1:${port}/api/v1/health/ready`, { signal: AbortSignal.timeout(1500) })
        if (response.ok) return { namespace, importSource, importRoots, server, token, base: `http://app-depot.test:${port}/`, binary, port, repository, serverErrors: () => errors }
      } catch { /* Startup is still in progress. */ }
      await new Promise(resolve => setTimeout(resolve, 100))
    }
    throw new Error(`Preview readiness timeout: ${errors}`)
  } catch (error) {
    await stopServer(server, 'Preview Server during startup')
    await rm(namespace, { recursive: true, force: true })
    await rm(importSource, { recursive: true, force: true })
    throw error
  }
}

async function restartLanPreview(preview) {
  await stopServer(preview.server, 'Preview Server before restart')
  preview.server = spawn(preview.binary, ['serve', '--namespace', preview.namespace, '--instance', 'preview', '--mode', 'preview',
    '--bind', `127.0.0.1:${preview.port}`, '--assets', join(preview.repository, 'web/dist')], { stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, APP_DEPOT_IMPORT_ROOTS: preview.importRoots, APP_DEPOT_SMOKE_TRACE: '1' } })
  let errors = ''
  preview.server.stdout.resume()
  preview.server.stderr.on('data', chunk => { errors = (errors + chunk.toString()).slice(-8000) })
  preview.serverErrors = () => errors
  const deadline = Date.now() + 15000
  while (Date.now() < deadline) {
    if (preview.server.exitCode !== null) throw new Error(`Preview restart exited ${preview.server.exitCode}: ${errors}`)
    try {
      const response = await fetch(`http://127.0.0.1:${preview.port}/api/v1/health/ready`, { signal: AbortSignal.timeout(1500) })
      if (response.ok) return
    } catch { /* Restart is still in progress. */ }
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`Preview restart readiness timeout: ${errors}`)
}

const selfHosted = process.env.APP_DEPOT_SELF_HOSTED === '1'
let smokeStage = 'starting Preview Server'
let stageStarted = Date.now()
function stage(name) {
  smokeStage = name
  stageStarted = Date.now()
  console.log(`Browser smoke: ${name}`)
}
const heartbeat = setInterval(() => {
  console.log(`Browser smoke: waiting in ${smokeStage} for ${Math.round((Date.now() - stageStarted) / 1000)}s`)
}, 30000)
heartbeat.unref()
const deadline = setTimeout(() => {
  console.error(`Browser smoke timed out in ${smokeStage} after ${Math.round((Date.now() - stageStarted) / 1000)}s; inspect the last stage and Server operation before retrying.`)
  process.exit(1)
}, 5 * 60 * 1000)
const preview = selfHosted ? await startLanPreview() : null
stage('opening browser and completing setup')
const browserPath = process.env.APP_DEPOT_BROWSER_BINARY ||
  (process.platform === 'darwin' ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : '/usr/bin/google-chrome')
const setupToken = preview?.token || process.env.APP_DEPOT_SETUP_TOKEN
const password = process.env.APP_DEPOT_TEST_PASSWORD || 'browser smoke password 12345'
const base = preview?.base || process.env.APP_DEPOT_BASE_URL || 'http://127.0.0.1:8787/'
const insecureOrigin = selfHosted || process.env.APP_DEPOT_EXPECT_INSECURE_ORIGIN === '1'
const pendingRequests = new Map()
const failedRequests = []
async function diagnosePreview(page, appId) {
  const endpoints = {}
  for (const [name, path] of Object.entries({ ready: '/api/v1/health/ready', document: '/' })) {
    if (!preview) break
    try {
      const response = await fetch(`http://127.0.0.1:${preview.port}${path}`, { signal: AbortSignal.timeout(3000) })
      endpoints[name] = response.status
      await response.arrayBuffer()
    } catch (error) { endpoints[name] = error instanceof Error ? error.name : String(error) }
  }
  if (preview && page && appId) {
    try {
      const cookie = (await page.context().cookies(preview.base)).find(item => item.name === 'ad_session')
      const response = await fetch(`http://127.0.0.1:${preview.port}/api/v1/apps/${appId}`, {
        headers: cookie ? { Cookie: `ad_session=${cookie.value}` } : {}, signal: AbortSignal.timeout(3000),
      })
      endpoints.catalog = response.status
      await response.arrayBuffer()
    } catch (error) { endpoints.catalog = error instanceof Error ? error.name : String(error) }
  }
  let serverProcess
  if (preview?.server.pid) {
    try { serverProcess = execFileSync('ps', ['-T', '-p', String(preview.server.pid), '-o', 'pid,tid,stat,wchan:24,%cpu,comm'], { encoding: 'utf8' }).trim() }
    catch (error) { serverProcess = error instanceof Error ? error.message : String(error) }
  }
  return { endpoints, serverExit: preview?.server.exitCode, serverSignal: preview?.server.signalCode,
    serverProcess, serverErrors: preview?.serverErrors?.(),
    pendingRequests: [...pendingRequests.values()].map(request => new URL(request.url()).pathname).slice(-15),
    failedRequests: failedRequests.slice(-10) }
}
let browser
let page
try {
  browser = await chromium.launch({ executablePath: browserPath, headless: true,
    args: insecureOrigin ? ['--host-resolver-rules=MAP app-depot.test 127.0.0.1', '--no-proxy-server'] : [] })
  page = await browser.newPage()
  page.on('request', request => pendingRequests.set(request, request))
  page.on('requestfinished', request => pendingRequests.delete(request))
  page.on('requestfailed', request => {
    pendingRequests.delete(request)
    failedRequests.push({ path: new URL(request.url()).pathname, failure: request.failure()?.errorText })
    if (failedRequests.length > 20) failedRequests.shift()
  })
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
  stage('checking Inbox, session and navigation')
  if (preview) {
    const build = await page.evaluate(async () => (await (await fetch('/api/v1/capabilities')).json()).build)
    if (!build || build.version !== null || build.revision !== null) throw new Error(`Local build metadata was misleading: ${JSON.stringify(build)}`)
    await page.getByText('App Depot Server: Development build').waitFor()
  }
  await page.getByRole('link', { name: 'Store', exact: true }).click()
  await page.getByRole('heading', { name: 'Store', exact: true }).waitFor()
  await page.getByRole('button', { name: 'Refresh catalog' }).waitFor()
  await page.getByRole('searchbox', { name: 'Search applications' }).waitFor()
  await page.goto(base)
  let inboxPath
  if (preview) {
    await page.goto(`${base}admin/import-sources`)
    await page.getByRole('heading', { name: 'Import Sources' }).waitFor()
    const sourceState = await page.evaluate(async () => {
      const response = await fetch('/api/v1/admin/import-sources')
      if (!response.ok) throw new Error(`Admin import sources failed: ${response.status}`)
      return response.json()
    })
    const inbox = sourceState.items.find(item => item.id === 'inbox')
    const shared = sourceState.items.find(item => item.id === 'shared')
    if (!inbox?.available || inbox.kind !== 'Inbox' || !inbox.importReadOnly || !shared?.available) {
      throw new Error(`Inbox or configured import root unavailable: ${JSON.stringify(sourceState)}`)
    }
    inboxPath = inbox.containerPath
    await writeFile(join(inboxPath, 'inbox-file.txt'), 'Inbox file fixture')
    await mkdir(join(inboxPath, 'inbox-tree/empty'), { recursive: true })
    await mkdir(join(inboxPath, 'inbox-tree/nested'), { recursive: true })
    await writeFile(join(inboxPath, 'inbox-tree/nested/content.txt'), 'Inbox tree content')
    execFileSync('python3', ['-c', 'import sys,zipfile; z=zipfile.ZipFile(sys.argv[1],"w",zipfile.ZIP_DEFLATED); z.writestr("zip/nested.txt","Inbox ZIP content"); z.close()', join(inboxPath, 'inbox.zip')])
    await symlink(dirname(inboxPath), join(inboxPath, 'escape-managed-parent'))
    await page.goto(base)
  }
  if (preview) await restartLanPreview(preview)
  if (inboxPath && (await readFile(join(inboxPath, 'inbox-file.txt'), 'utf8')) !== 'Inbox file fixture') {
    throw new Error('Inbox content did not survive Server restart')
  }
  await page.reload()
  await page.getByRole('heading', { name: 'Server overview' }).waitFor()
  await page.getByRole('button', { name: 'Switch to dark theme' }).click()
  if (await page.locator('html').getAttribute('data-theme') !== 'dark') throw new Error('Dark theme did not activate')
  await page.reload()
  await page.getByRole('heading', { name: 'Server overview' }).waitFor()
  if (await page.locator('html').getAttribute('data-theme') !== 'dark') throw new Error('Theme preference was not restored')
  await page.getByRole('button', { name: 'Switch to light theme' }).click()
  if (preview) {
    await page.goto(`${base}artifacts/new-app`)
    await page.getByRole('heading', { name: 'Add Artifact' }).waitFor()
    await page.getByLabel('Source').selectOption('server')
    await page.getByLabel('Artifact type').selectOption('BareTree')
    await page.getByLabel('Import root').selectOption('inbox')
    await page.reload()
    if (await page.getByLabel('Source').inputValue() !== 'server' || await page.getByLabel('Artifact type').inputValue() !== 'BareTree') {
      throw new Error('Add Artifact source or type was lost on direct reload')
    }
    await page.goBack()
    if (await page.getByLabel('Artifact type').inputValue() !== 'BareFile') throw new Error('Artifact type did not follow browser Back')
    await page.goForward()
    if (await page.getByLabel('Artifact type').inputValue() !== 'BareTree') throw new Error('Artifact type did not follow browser Forward')
    await page.goto(`${base}apps`)
  }
  await page.getByRole('navigation', { name: 'Server navigation' }).getByRole('link', { name: 'Applications' }).click()
  await page.getByRole('heading', { name: 'Applications', exact: true }).waitFor()
  await page.getByRole('heading', { name: 'No applications yet' }).waitFor()
  await page.getByRole('link', { name: 'Create application' }).first().click()
  await page.getByLabel('Application name').fill('Portal Draft')
  await page.getByRole('button', { name: 'Create Draft application' }).click()
  await page.getByRole('heading', { name: 'Portal Draft' }).waitFor()
  stage('managing App metadata, artwork and releases')
  if (preview) await page.getByText('App Depot Server: Development build').waitFor()
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
  const firstPicker = page.waitForEvent('filechooser')
  await page.getByRole('button', { name: 'Add Icon artwork' }).focus()
  await page.keyboard.press('Enter')
  await (await firstPicker).setFiles(new URL('../../crates/server/tests/fixtures/artwork.png', import.meta.url).pathname)
  await page.getByText('Artwork added.').waitFor()
  const iconPreview = 'img[alt="Icon for Portal Draft"]'
  await waitForArtworkImage(page, iconPreview, 2, 3, 'Artwork preview did not load original image bytes')
  const oldIconEtag = await page.evaluate(async id => (await (await fetch(`/api/v1/apps/${id}/artwork/icon`)).json()).etag, draftId)
  const invalidPicker = page.waitForEvent('filechooser')
  await page.getByRole('button', { name: 'Replace Icon artwork' }).click()
  await (await invalidPicker).setFiles({ name: 'invalid.png', mimeType: 'image/png', buffer: Buffer.from('not an image') })
  await page.getByRole('alert').first().waitFor()
  await waitForArtworkImage(page, iconPreview, 2, 3, 'Invalid replacement hid current artwork')
  const replacePicker = page.waitForEvent('filechooser')
  await page.getByRole('button', { name: 'Replace Icon artwork' }).click()
  await (await replacePicker).setFiles(new URL('../../crates/server/tests/fixtures/artwork.jpg', import.meta.url).pathname)
  await page.getByText('Artwork replaced.').waitFor()
  const icoPicker = page.waitForEvent('filechooser')
  await page.getByRole('button', { name: 'Replace Icon artwork' }).click()
  await (await icoPicker).setFiles(new URL('../../crates/server/tests/fixtures/artwork-multi.ico', import.meta.url).pathname)
  await waitForArtworkImage(page, iconPreview, 64, 64, 'ICO preview did not load normalized PNG bytes')
  const icoMetadata = await page.evaluate(async id => (await (await fetch(`/api/v1/apps/${id}/artwork/icon`)).json()), draftId)
  if (icoMetadata.mimeType !== 'image/png' || icoMetadata.width !== 64 || icoMetadata.height !== 64) throw new Error('Browser ICO upload was not normalized to PNG')
  await page.goto(`${base}apps`)
  const listIcon = `table tbody tr:has(a[href="/apps/${draftId}"]) .artwork-thumb img`
  await waitForArtworkImage(page, listIcon, 64, 64, 'Application list did not show the ICO icon')
  await page.goto(`${base}apps/${draftId}/artwork`)
  await page.getByRole('heading', { name: 'Artwork', exact: true }).waitFor()
  const staleArtwork = await page.evaluate(async ({ id, etag }) => {
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    return (await fetch(`/api/v1/apps/${id}/artwork/icon`, { method: 'DELETE', headers: { 'X-CSRF-Token': session.csrfToken, 'If-Match': etag } })).status
  }, { id: draftId, etag: oldIconEtag })
  if (staleArtwork !== 412) throw new Error(`Stale artwork ETag was accepted: ${staleArtwork}`)
  await page.getByRole('button', { name: 'Remove Icon' }).click()
  await page.getByRole('dialog', { name: 'Remove Icon' }).getByRole('button', { name: 'Remove artwork' }).click()
  await page.getByText('Artwork removed.').waitFor()
  await page.reload()
  await page.getByRole('heading', { name: 'Artwork', exact: true }).waitFor()
  await page.getByText('No artwork yet').waitFor()
  await page.getByLabel('Role identifier').fill('website.banner')
  const customPicker = page.waitForEvent('filechooser')
  await page.getByRole('button', { name: 'Add custom role' }).click()
  await (await customPicker).setFiles(new URL('../../crates/server/tests/fixtures/artwork.webp', import.meta.url).pathname)
  await page.getByText('Artwork added.').waitFor()
  await page.getByRole('heading', { name: 'Custom roles' }).waitFor()
  await page.getByRole('button', { name: 'Remove website.banner' }).click()
  await page.getByRole('dialog', { name: 'Remove website.banner' }).getByRole('button', { name: 'Remove artwork' }).click()
  await page.getByRole('heading', { name: 'Custom roles' }).waitFor({ state: 'hidden' })
  await page.getByRole('heading', { name: 'No artwork yet' }).waitFor()
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
  await page.getByRole('link', { name: 'Add Artifact' }).first().click()
  await page.getByRole('heading', { name: 'Add Artifact' }).waitFor()
  await page.getByLabel('File', { exact: true }).setInputFiles({ name: 'draft.txt', mimeType: 'text/plain', buffer: Buffer.from('Draft') })
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText('Saved Draft for Portal Draft').waitFor({ timeout: 30000 })
  let integratedIds
  if (preview) {
  await page.getByRole('link', { name: 'Open release' }).click()
  await page.waitForURL(/\/releases\/[0-9a-f-]{36}$/)
  const firstReleaseId = new URL(page.url()).pathname.split('/').pop()
  await page.getByRole('link', { name: 'Files and downloads' }).click()
  await page.getByRole('heading', { name: 'Contents / Artifacts' }).waitFor()
  await page.getByRole('link', { name: 'Explore content' }).waitFor()
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
  await page.getByRole('button', { name: 'Standard fields' }).waitFor()
  if (await page.getByRole('button', { name: 'Standard fields' }).getAttribute('aria-pressed') !== 'true') throw new Error('Standard metadata did not open by default')
  await page.getByRole('button', { name: 'Advanced JSON' }).click()
  await page.getByLabel('Current JSON value').fill('null')
  await page.getByRole('button', { name: 'Save new revision' }).click()
  await page.getByText('Saved as a new declaration revision.').waitFor()
  const nullDocument = await page.evaluate(async id => (await (await fetch(`/api/v1/apps/${id}/declarations`)).json()).data, draftId)
  if (nullDocument !== null) throw new Error('Explicit JSON null was not preserved')
  await page.getByText('This declaration cannot be edited as standard fields.').waitFor()
  if (await page.getByRole('button', { name: 'Standard fields' }).isEnabled()) throw new Error('Non-object JSON was offered to standard editor')
  await page.getByLabel('Current JSON value').fill('{"appDepot":42,"custom":true}')
  await page.getByRole('button', { name: 'Save new revision' }).click()
  await page.waitForFunction(async id => (await (await fetch(`/api/v1/apps/${id}/declarations`)).json()).data?.appDepot === 42, draftId)
  if (await page.getByRole('button', { name: 'Standard fields' }).isEnabled()) throw new Error('Incompatible appDepot structure was offered to standard editor')
  await page.getByRole('button', { name: 'Show revision history' }).click()
  await page.getByRole('button', { name: 'Inspect revision' }).nth(await emptyRevisionIndex(page, `/apps/${draftId}/declarations`)).click()
  await page.getByRole('button', { name: 'Restore as new revision' }).click()
  await page.getByText('Saved as a new declaration revision.').waitFor()
  const restoredAppData = await page.evaluate(async id => (await (await fetch(`/api/v1/apps/${id}/declarations`)).json()).data, draftId)
  if (JSON.stringify(restoredAppData) !== '{}') throw new Error(`App declaration restoration did not create the prior value: ${JSON.stringify(restoredAppData)}`)
  const appDeclarations = page.getByRole('heading', { name: 'Application data' }).locator('..')
  await appDeclarations.getByRole('button', { name: 'Advanced JSON' }).click()
  await appDeclarations.getByLabel('Current JSON value').fill('{"custom":{"keep":true},"appDepot":{"extension":"kept"}}')
  let delayedDeclarationRead = false
  await page.route(`**/api/v1/apps/${draftId}/declarations`, async route => {
    if (route.request().method() === 'GET' && !delayedDeclarationRead) {
      delayedDeclarationRead = true
      await new Promise(resolve => setTimeout(resolve, 500))
    }
    await route.continue()
  })
  const rawAppSave = page.waitForResponse(response => response.url().endsWith(`/api/v1/apps/${draftId}/declarations`) && response.request().method() === 'PUT')
  await appDeclarations.getByRole('button', { name: 'Save new revision' }).click()
  if ((await rawAppSave).status() !== 200) throw new Error('Raw App declaration save failed')
  if (await appDeclarations.getByText('Saved as a new declaration revision.').isVisible()) throw new Error('Save confirmation preceded the current declaration refresh')
  await appDeclarations.getByText('Saved as a new declaration revision.').waitFor()
  await page.unroute(`**/api/v1/apps/${draftId}/declarations`)
  await appDeclarations.getByRole('button', { name: 'Standard fields' }).click()
  await appDeclarations.getByLabel('Summary').fill('An optional artifact catalog')
  await appDeclarations.getByRole('button', { name: 'Advanced JSON' }).click()
  await appDeclarations.getByRole('button', { name: 'Standard fields' }).click()
  if (await appDeclarations.getByLabel('Summary').inputValue() !== 'An optional artifact catalog') throw new Error('Switching editors discarded an unsaved standard field')
  await appDeclarations.getByLabel('Kind', { exact: true }).selectOption('custom')
  await appDeclarations.getByLabel('Custom kind').fill('tool')
  await appDeclarations.getByLabel('Developer').fill('Example Studio')
  await appDeclarations.getByLabel('Publisher').fill('Example Publishing')
  await appDeclarations.getByLabel('Homepage URL').fill('not-a-url')
  await appDeclarations.getByRole('button', { name: 'Save standard metadata' }).click()
  if (await appDeclarations.getByLabel('Homepage URL').evaluate(input => input.validity.valid)) throw new Error('Invalid homepage URL passed browser validation')
  await appDeclarations.getByLabel('Homepage URL').fill('https://example.test/app')
  await appDeclarations.getByLabel('License').fill('MIT')
  await appDeclarations.getByLabel('Categories (one per line)').fill('utilities')
  await appDeclarations.getByLabel('Tags (one per line)').fill('utilities\nutilities')
  await appDeclarations.getByRole('button', { name: 'Save standard metadata' }).click()
  await appDeclarations.getByText('Tags contains a duplicate entry.').waitFor()
  await appDeclarations.getByLabel('Tags (one per line)').fill('utilities\nself-hosted')
  await appDeclarations.getByRole('button', { name: 'Save standard metadata' }).click()
  await appDeclarations.getByText('Saved as a new declaration revision.').waitFor()
  const standardAppData = await page.evaluate(async id => (await (await fetch(`/api/v1/apps/${id}/declarations`)).json()).data, draftId)
  if (standardAppData.custom?.keep !== true || standardAppData.appDepot?.extension !== 'kept' ||
      standardAppData.appDepot?.kind !== 'tool' || standardAppData.appDepot?.tags?.length !== 2 ||
      standardAppData.appDepot?.developer !== 'Example Studio' || standardAppData.appDepot?.categories?.[0] !== 'utilities') {
    throw new Error(`Standard App metadata lost extension data or typed fields: ${JSON.stringify(standardAppData)}`)
  }
  await appDeclarations.getByRole('button', { name: 'Advanced JSON' }).click()
  if (!(await appDeclarations.getByLabel('Current JSON value').inputValue()).includes('self-hosted')) throw new Error('Standard fields did not update the full JSON editor')
  await appDeclarations.getByRole('button', { name: 'Standard fields' }).click()
  await appDeclarations.getByLabel('Kind', { exact: true }).selectOption('')
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
  await appDeclarations.getByLabel('Kind', { exact: true }).selectOption('custom')
  await appDeclarations.getByLabel('Custom kind').fill('tool')
  await appDeclarations.getByLabel('Description', { exact: true }).fill('An integrated directory application')
  await appDeclarations.getByLabel('Tags (one per line)').fill('utilities\nself-hosted')
  const integratedMetadataSave = page.waitForResponse(response => response.url().endsWith(`/api/v1/apps/${draftId}/declarations`) && response.request().method() === 'PUT')
  await appDeclarations.getByRole('button', { name: 'Save standard metadata' }).click()
  if ((await integratedMetadataSave).status() !== 200) throw new Error('Integrated App metadata save failed')
  await page.goto(`${base}apps/${draftId}`)
  await page.getByRole('heading', { name: 'Application metadata' }).waitFor()
  await page.getByText('Example Studio').waitFor()
  await page.getByRole('link', { name: 'Edit metadata' }).waitFor()
  await page.goto(`${base}apps/${draftId}/releases/new`)
  await page.getByRole('heading', { name: 'Create Draft release' }).waitFor()
  await page.getByLabel('Version (optional)').fill('test-v1')
  await page.getByRole('button', { name: 'Create Draft release' }).click()
  await page.waitForURL(/\/releases\/[0-9a-f-]{36}$/)
  const releaseId = new URL(page.url()).pathname.split('/').pop()
  await page.getByRole('heading', { name: 'test-v1' }).waitFor()
  const staleReleaseEtag = await page.evaluate(async id => (await fetch(`/api/v1/releases/${id}`)).headers.get('ETag'), releaseId)
  await page.getByRole('navigation', { name: 'Release sections' }).getByRole('link', { name: 'Advanced', exact: true }).click()
  await page.getByLabel('Version (optional)').fill('test-v2')
  await page.getByRole('button', { name: 'Save version' }).click()
  await page.getByRole('heading', { name: 'test-v2' }).waitFor()
  const staleRelease = await page.evaluate(async ({ id, etag }) => {
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    return (await fetch(`/api/v1/releases/${id}`, { method: 'PATCH', headers: { 'Content-Type': 'application/json',
      'X-CSRF-Token': session.csrfToken, 'If-Match': etag }, body: JSON.stringify({ version: 'stale' }) })).status
  }, { id: releaseId, etag: staleReleaseEtag })
  if (staleRelease !== 412) throw new Error(`Stale Release ETag was accepted: ${staleRelease}`)
  await page.getByRole('link', { name: 'Add Artifact' }).first().click()
  await page.getByRole('heading', { name: 'Add Artifact' }).waitFor()
  const fileBytes = Buffer.from('M9.2 file artifact content')
  await page.getByLabel('File', { exact: true }).setInputFiles({ name: 'content.txt', mimeType: 'text/plain', buffer: fileBytes })
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText('Saved Draft for Portal Draft Edited').waitFor({ timeout: 30000 })
  await page.getByRole('region', { name: 'Upload progress' }).getByText('Completed').waitFor()
  await page.getByRole('link', { name: 'Open release' }).click()
  const fileArtifact = await page.evaluate(async id => (await (await fetch(`/api/v1/releases/${id}/artifacts`)).json()).items[0]?.artifactId, releaseId)
  if (!fileArtifact) throw new Error('Uploaded File Artifact identity missing')
  const downloaded = await page.evaluate(async id => Array.from(new Uint8Array(await (await fetch(`/api/v1/artifacts/${encodeURIComponent(id)}/content`)).arrayBuffer())), fileArtifact)
  if (createHash('sha256').update(Buffer.from(downloaded)).digest('hex') !== fileArtifact.split(':').pop()) throw new Error('Downloaded File Artifact hash mismatch')
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/uploads/new`)
  stage('testing interrupted browser upload and authoritative resume')
  const largeBytes = Buffer.alloc(4 * 1024 * 1024 + 8, 65)
  const sentRanges = []
  let injectedFailure = false
  let releaseFailure
  const transferGate = new Promise(resolve => { releaseFailure = resolve })
  await page.route('**/api/v1/uploads/*/files/*', async route => {
    if (route.request().method() === 'PUT') {
      const range = route.request().headers()['content-range']
      sentRanges.push(range)
      if (range.startsWith('bytes 4194304-') && !injectedFailure) {
        injectedFailure = true
        await transferGate
        await route.fulfill({ status: 503, contentType: 'application/problem+json', body: JSON.stringify({ title: 'Temporary transfer failure', status: 503 }) })
        return
      }
    }
    await route.continue()
  })
  await page.getByLabel('File', { exact: true }).setInputFiles({ name: 'resume.bin', mimeType: 'application/octet-stream', buffer: largeBytes })
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText('99% of bytes acknowledged by Server').waitFor()
  releaseFailure()
  await page.getByText('The server is not ready. Try again after recovery completes.').waitFor()
  await page.reload()
  await page.getByLabel('File', { exact: true }).setInputFiles({ name: 'resume.bin', mimeType: 'application/octet-stream', buffer: largeBytes })
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText('Saved Draft for Portal Draft Edited').waitFor({ timeout: 30000 })
  await page.unroute('**/api/v1/uploads/*/files/*')
  if (sentRanges.filter(range => range.startsWith('bytes 0-')).length !== 1 || sentRanges.filter(range => range.startsWith('bytes 4194304-')).length !== 2) {
    throw new Error(`Upload retry did not honor authoritative ranges: ${JSON.stringify(sentRanges)}`)
  }
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/uploads/new`)
  await page.evaluate(() => {
    const original = File.prototype.slice
    let reads = 0
    File.prototype.slice = function (start, end, contentType) {
      if (this.name === 'changed-source.bin' && ++reads > 1) return new Blob([new Uint8Array([66])])
      return original.call(this, start, end, contentType)
    }
  })
  await page.getByLabel('File', { exact: true }).setInputFiles({ name: 'changed-source.bin', mimeType: 'application/octet-stream', buffer: Buffer.from('A') })
  await page.getByRole('button', { name: 'Upload and publish' }).click()
  await page.getByText(/Source file changed-source\.bin changed after hashing/).waitFor({ timeout: 30000 })
  if (await page.getByRole('link', { name: 'Open release' }).count()) throw new Error('Changed source was registered or published')
  if (await page.getByRole('button', { name: 'Upload and publish' }).isEnabled()) throw new Error('Changed source remained selected for retry')
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/uploads/new`)
  await page.getByLabel('File', { exact: true }).setInputFiles({ name: 'mtime-changed.bin', mimeType: 'application/octet-stream', buffer: Buffer.from('A') })
  await page.evaluate(() => {
    const original = Object.getOwnPropertyDescriptor(File.prototype, 'lastModified').get
    let reads = 0
    Object.defineProperty(File.prototype, 'lastModified', { configurable: true, get() {
      const value = original.call(this)
      return this.name === 'mtime-changed.bin' && ++reads >= 4 ? value + 1000 : value
    } })
  })
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText(/Source file mtime-changed\.bin changed size or modification time after selection/).waitFor({ timeout: 30000 })
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}`)
  await page.getByRole('link', { name: 'Artifacts', exact: true }).click()
  await page.getByText('Advanced association details', { exact: true }).first().click()
  const associationDeclarations = page.getByRole('heading', { name: 'Association declarations' }).first().locator('..')
  await associationDeclarations.getByRole('button', { name: 'Advanced JSON' }).click()
  await associationDeclarations.getByLabel('Current JSON value').fill('{"artifact":"data"}')
  await associationDeclarations.getByRole('button', { name: 'Save new revision' }).click()
  await associationDeclarations.getByText('Saved as a new declaration revision.').waitFor()
  await associationDeclarations.getByRole('button', { name: 'Show revision history' }).click()
  const firstAssociationId = await page.evaluate(async id => (await (await fetch(`/api/v1/releases/${id}/artifacts?pageSize=10`)).json()).items[0].id, releaseId)
  const fileLaunchAssessment = await page.evaluate(async id => {
    const response = await fetch(`/api/v1/associations/${id}/entry-points/validate`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ data: { appDepot: { entryPoints: [{ id: 'file', target: { type: 'ArtifactFile' },
        platforms: ['linux/x86_64'], launch: { method: 'NativePortableV1' } }] } } })
    })
    return { status: response.status, body: await response.json() }
  }, firstAssociationId)
  if (fileLaunchAssessment.status !== 200 || fileLaunchAssessment.body.items[0].status !== 'Valid' ||
      fileLaunchAssessment.body.items[0].launchStatus !== 'Valid') throw new Error('Portable File target assessment failed')
  await page.getByRole('button', { name: 'Inspect revision' }).nth(await emptyRevisionIndex(page, `/associations/${firstAssociationId}/declarations`)).click()
  await page.getByRole('button', { name: 'Restore as new revision' }).click()
  await page.getByText('Saved as a new declaration revision.').first().waitFor()
  await associationDeclarations.getByRole('button', { name: 'Standard fields' }).click()
  await associationDeclarations.getByLabel('Artifact label').fill('Primary payload')
  await associationDeclarations.getByLabel('Purpose').fill('distribution')
  await associationDeclarations.getByRole('button', { name: 'Save standard metadata' }).click()
  await associationDeclarations.getByText('Saved as a new declaration revision.').waitFor()
  const standardAssociationData = await page.evaluate(async id => (await (await fetch(`/api/v1/associations/${id}/declarations`)).json()).data, firstAssociationId)
  if (standardAssociationData.appDepot?.label !== 'Primary payload' || standardAssociationData.appDepot?.purpose !== 'distribution') {
    throw new Error('Standard association metadata was not saved')
  }
  await page.getByRole('navigation', { name: 'Release sections' }).getByRole('link', { name: 'Advanced', exact: true }).click()
  const releaseDeclarations = page.getByRole('heading', { name: 'Release declarations' }).locator('..')
  await releaseDeclarations.getByRole('button', { name: 'Advanced JSON' }).click()
  await releaseDeclarations.getByLabel('Current JSON value').fill('{"release":"data"}')
  const releaseRawSave = page.waitForResponse(response => response.url().endsWith(`/api/v1/releases/${releaseId}/declarations`) && response.request().method() === 'PUT')
  await releaseDeclarations.getByRole('button', { name: 'Save new revision' }).click()
  const savedReleaseDocument = await (await releaseRawSave).json()
  await page.getByRole('heading', { name: 'Release declarations' }).locator('..').locator('code').filter({ hasText: savedReleaseDocument.revisionId }).waitFor({ state: 'attached' })
  await page.getByText('Saved as a new declaration revision.').last().waitFor()
  await releaseDeclarations.getByRole('button', { name: 'Standard fields' }).click()
  await releaseDeclarations.getByLabel('Release notes').fill('Initial public content')
  await releaseDeclarations.getByLabel('Platforms (one per line; custom values allowed)').fill('macos/arm64')
  await releaseDeclarations.getByLabel('linux/x86_64').check()
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
  stage('uploading tree and ZIP Artifacts and browsing contents')
  await mkdir(join(treeRoot, 'nested'), { recursive: true })
  await writeFile(join(treeRoot, 'nested', 'tree.txt'), 'tree content')
  await writeFile(join(treeRoot, 'nested', 'é漢字.txt'), 'longer UTF-8 test content')
  const minimalPe = Buffer.alloc(512)
  minimalPe.write('MZ')
  minimalPe.writeUInt32LE(64, 60)
  minimalPe.write('PE\0\0', 64)
  minimalPe.writeUInt16LE(0x14c, 68)
  minimalPe.writeUInt16LE(1, 70)
  minimalPe.writeUInt16LE(224, 84)
  minimalPe.writeUInt16LE(2, 86)
  minimalPe.writeUInt16LE(0x10b, 88)
  const markers = {
    'redist/VC/vcredist_x86.exe': 'Microsoft Visual C++ Redistributable',
    'redist/PhysX/PhysX_9.09.0203_SystemSoftware.exe': 'NVIDIA PhysX',
    'redist/DirectX/DXSETUP.exe': 'DirectX Setup',
  }
  for (const [path, mark] of Object.entries(markers)) {
    await mkdir(join(treeRoot, path, '..'), { recursive: true })
    await writeFile(join(treeRoot, path), Buffer.concat([minimalPe, Buffer.from(mark)]))
  }
  const dll = Buffer.from(minimalPe)
  dll.writeUInt16LE(0x2002, 86)
  for (const name of ['dsetup.dll', 'dsetup32.dll']) await writeFile(join(treeRoot, 'redist/DirectX', name), dll)
  const cabinet = Buffer.alloc(36)
  cabinet.write('MSCF')
  cabinet.writeUInt32LE(cabinet.length, 8)
  await writeFile(join(treeRoot, 'redist/DirectX/dxupdate.cab'), cabinet)
  await mkdir(join(treeRoot, 'zz-depth/one/two/three'), { recursive: true })
  await writeFile(join(treeRoot, 'zz-depth/one/two/three/marker.txt'), 'deep marker')
  await writeFile(join(treeRoot, '.DS_Store'), 'changing finder metadata')
  await writeFile(join(treeRoot, 'nested', '._tree.txt'), 'appledouble metadata')
  await mkdir(join(treeRoot, '.Trashes'), { recursive: true })
  await writeFile(join(treeRoot, '.Trashes', 'old'), 'trash metadata')
  await writeFile(join(treeRoot, '.hidden-game'), 'valid hidden game data')
  for (let start = 0; start < 6144; start += 128) {
    await Promise.all(Array.from({ length: 128 }, (_, index) =>
      writeFile(join(treeRoot, `zero-${String(start + index).padStart(4, '0')}.txt`), '')))
  }
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/uploads/new`)
  await page.getByLabel('Artifact type').selectOption('BareTree')
  await page.getByLabel('Directory files').setInputFiles(treeRoot)
  await page.getByText(/3 OS metadata files excluded/).waitFor()
  await page.getByLabel('Empty directory paths').fill('browser-tree/empty')
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText('Saved Draft for Portal Draft Edited').waitFor({ timeout: 60000 })
  await page.getByRole('link', { name: 'Open release' }).click()
  const treeAssociationId = await associationWithRoot(page, releaseId, 'browser-tree')
  await page.evaluate(async id => {
    const path = `/api/v1/associations/${id}/declarations`
    const current = await fetch(path)
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    const saved = await fetch(path, { method: 'PUT', headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': session.csrfToken,
      'If-Match': current.headers.get('ETag') }, body: '{"data":{"custom":{"big":18446744073709551615},"appDepot":{"extension":{"keep":true}}}}' })
    if (!saved.ok) throw new Error(`Could not seed exact declaration: ${saved.status}`)
  }, treeAssociationId)
  const treeArtifactId = await page.evaluate(async id => (await (await fetch(`/api/v1/associations/${id}`)).json()).artifactId, treeAssociationId)
  integratedIds = { appId: draftId, releaseId, associationId: treeAssociationId, artifactId: treeArtifactId }
  const referenceStatuses = await page.evaluate(async id => {
    const points = ['browser-tree/nested/é漢字.txt', 'browser-tree/empty', 'missing.txt', '../escape'].map((path, index) => ({ id: `case-${index}`, target: { type: 'TreeFile', path } }))
    const response = await fetch(`/api/v1/associations/${id}/entry-points/validate`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ data: { appDepot: { entryPoints: points } } }) })
    if (!response.ok) throw new Error(`Entry-point assessment failed: ${response.status}`)
    return (await response.json()).items.map(item => item.status)
  }, treeAssociationId)
  if (JSON.stringify(referenceStatuses) !== JSON.stringify(['Valid', 'NotFile', 'Missing', 'Invalid'])) throw new Error(`Unexpected reference statuses: ${referenceStatuses}`)
  await page.getByRole('link', { name: 'Artifacts', exact: true }).click()
  await page.locator(`a[href$="/artifacts/${treeAssociationId}"]`).click()
  await page.getByRole('heading', { name: 'Artifact Explorer' }).waitFor()
  const folderTree = page.getByRole('navigation', { name: 'Directory tree' })
  const crumbs = page.getByRole('navigation', { name: 'Artifact path' })
  const treeRequests = []
  const trackFolders = request => {
    const url = new URL(request.url())
    if (url.pathname.endsWith('/inventory') && url.searchParams.get('kind') === 'Directory') treeRequests.push(url.searchParams.get('parent'))
  }
  page.on('request', trackFolders)
  await folderTree.getByRole('button', { name: 'Expand browser-tree' }).click()
  await folderTree.getByRole('button', { name: 'nested', exact: true }).waitFor()
  if (treeRequests.some(path => path.startsWith('browser-tree/'))) throw new Error('Folder expansion fetched grandchildren')
  await folderTree.getByRole('button', { name: 'Collapse browser-tree' }).click()
  await folderTree.getByRole('button', { name: 'nested', exact: true }).waitFor({ state: 'hidden' })
  page.off('request', trackFolders)
  await contentRows(page, /^browser-tree Folder/).press('Enter')
  await contentRows(page, /^empty Folder/).dblclick()
  await page.getByRole('heading', { name: 'Empty directory' }).waitFor()
  await crumbs.getByRole('button', { name: 'browser-tree', exact: true }).click()
  await contentRows(page, /^nested Folder/).dblclick()
  await contentRows(page, /^tree\.txt File/).waitFor()
  await contentRows(page, /^é漢字\.txt File/).waitFor()
  await contentRows(page, /^tree\.txt File/).click()
  await page.evaluate(() => {
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: undefined })
    const copy = document.execCommand.bind(document)
    document.execCommand = (command, ...args) => {
      if (command === 'copy') {
        window.testCopiedPath = document.activeElement.value
        return true
      }
      return copy(command, ...args)
    }
  })
  await page.getByRole('button', { name: 'Copy relative path' }).click()
  await page.getByText('Relative path copied.').waitFor()
  if (await page.evaluate(() => window.testCopiedPath) !== 'browser-tree/nested/tree.txt') throw new Error('LAN HTTP path copy mismatch')
  const treeFileLink = await page.getByRole('link', { name: 'Download', exact: true }).first().getAttribute('href')
  const treeBytes = await page.evaluate(async href => Array.from(new Uint8Array(await (await fetch(href)).arrayBuffer())), treeFileLink)
  if (Buffer.from(treeBytes).toString() !== 'tree content') throw new Error(`Tree file download mismatch: ${treeFileLink} ${Buffer.from(treeBytes).toString()}`)
  const treeDigest = await page.evaluate(async href => {
    const artifactId = decodeURIComponent(href.split('/artifacts/')[1].split('/files/')[0])
    const inventory = await (await fetch(`/api/v1/artifacts/${encodeURIComponent(artifactId)}/inventory?q=tree.txt`)).json()
    return inventory.items.find(item => item.path === 'browser-tree/nested/tree.txt').sha256
  }, treeFileLink)
  if (createHash('sha256').update(Buffer.from(treeBytes)).digest('hex') !== treeDigest) throw new Error('Tree download SHA-256 mismatch')
  const selectionInventory = await page.evaluate(async id => {
    const get = async q => (await (await fetch(`/api/v1/artifacts/${encodeURIComponent(id)}/inventory?q=${encodeURIComponent(q)}`)).json()).items
    return { finder: await get('.DS_Store'), trash: await get('.Trashes'), hidden: await get('.hidden-game') }
  }, treeArtifactId)
  if (selectionInventory.finder.length || selectionInventory.trash.length || !selectionInventory.hidden.some(item => item.path === 'browser-tree/.hidden-game')) {
    throw new Error('Default OS metadata exclusion removed valid hidden content or included metadata')
  }
  await page.getByRole('button', { name: 'Add entry point' }).click()
  let dialog = entryEditor(page)
  await dialog.getByLabel('Label (optional)').fill('Tree script')
  await dialog.getByLabel('Add target platform').selectOption('linux/x86_64')
  await dialog.getByLabel('Execution method').selectOption('native')
  await dialog.getByLabel('Working directory').selectOption('custom')
  await dialog.getByText('Browse working directories', { exact: true }).click()
  await contentRows(dialog, /^browser-tree Folder/).dblclick()
  await contentRows(dialog, /^nested Folder/).dblclick()
  await dialog.getByRole('button', { name: 'Use this directory' }).click()
  await dialog.getByRole('button', { name: 'Add argument' }).click()
  await dialog.getByLabel('Argument 1').fill('--windowed')
  await dialog.getByRole('button', { name: 'Add argument' }).click()
  await dialog.getByRole('button', { name: 'Save entry points' }).click()
  await page.getByText('Entry points saved as a new declaration revision.').waitFor()
  await crumbs.getByRole('button', { name: 'nested', exact: true }).waitFor()
  const savedLaunch = await page.evaluate(async id => {
    const text = await (await fetch(`/api/v1/associations/${id}/declarations`)).text()
    const data = JSON.parse(text).data
    return { exact: text.includes('18446744073709551615'), method: data.appDepot.entryPoints[0].launch.method,
      directory: data.appDepot.entryPoints[0].launch.workingDirectory, arguments: data.appDepot.entryPoints[0].launch.arguments,
      unknown: data.appDepot.extension.keep }
  }, treeAssociationId)
  if (!savedLaunch.exact || savedLaunch.method !== 'NativePortableV1' || savedLaunch.directory !== 'browser-tree/nested' ||
      JSON.stringify(savedLaunch.arguments) !== JSON.stringify(['--windowed', '']) || savedLaunch.unknown !== true) {
    throw new Error(`Structured portable launch did not preserve declaration JSON: ${JSON.stringify(savedLaunch)}`)
  }
  await page.reload()
  await crumbs.getByRole('button', { name: 'nested', exact: true }).waitFor()
  await openEntryEditor(page)
  if (await dialog.getByLabel('Execution method').inputValue() !== 'native' ||
      await dialog.getByLabel('Argument 1').inputValue() !== '--windowed') throw new Error('Portable launch controls did not reload saved values')
  await dialog.getByRole('button', { name: 'Close editor' }).click()
  await contentRows(page, /^tree\.txt File/).click()
  await page.getByRole('button', { name: 'Add entry point' }).click()
  await dialog.getByLabel('Label (optional)').last().fill('Alternative')
  await dialog.getByRole('button', { name: 'Save entry points' }).click()
  await page.getByText('Entry points saved as a new declaration revision.').waitFor()
  const multiplePoints = await page.evaluate(async id => (await (await fetch(`/api/v1/associations/${id}/declarations`)).json()).data.appDepot.entryPoints, treeAssociationId)
  if (multiplePoints.length !== 2 || multiplePoints[1].label !== 'Alternative') throw new Error('Multiple entry points were not saved')
  await openEntryEditor(page)
  await dialog.getByRole('button', { name: 'Remove entry point' }).last().click()
  await dialog.getByRole('button', { name: 'Save entry points' }).click()
  await page.getByText('Entry points saved as a new declaration revision.').waitFor()
  const remainingPoints = await page.evaluate(async id => (await (await fetch(`/api/v1/associations/${id}/declarations`)).json()).data.appDepot.entryPoints, treeAssociationId)
  if (remainingPoints.length !== 1 || remainingPoints[0].label !== 'Tree script') throw new Error('Entry point removal did not preserve the first point')
  const replaceTarget = async name => {
    await openEntryEditor(page)
    await dialog.getByText('Change target using Explorer', { exact: true }).click()
    await contentRows(dialog, /^browser-tree Folder/).dblclick()
    await contentRows(dialog, /^nested Folder/).dblclick()
    await contentRows(dialog, name).click()
    await dialog.getByRole('button', { name: 'Use this file' }).click()
    await dialog.getByRole('button', { name: 'Save entry points' }).click()
    await page.getByText('Entry points saved as a new declaration revision.').waitFor()
  }
  await replaceTarget(/^é漢字\.txt File/)
  const editedPoint = await page.evaluate(async id => (await (await fetch(`/api/v1/associations/${id}/declarations`)).json()).data.appDepot.entryPoints[0], treeAssociationId)
  if (editedPoint.target.path !== 'browser-tree/nested/é漢字.txt') throw new Error('Entry point target edit failed')
  await replaceTarget(/^tree\.txt File/)
  await crumbs.getByRole('button', { name: 'browser-tree', exact: true }).click()
  await page.getByRole('navigation', { name: 'Directory pages' }).getByRole('button', { name: 'Next page' }).click()
  await contentRows(page, /^zero-0050\.txt File/).waitFor()
  if (await contentRows(page).count() !== 50) throw new Error('Large Tree was not bounded to 50 rows')
  await page.getByRole('navigation', { name: 'Directory pages' }).getByRole('button', { name: 'Previous page' }).click()
  await contentRows(page, /^zero-0000\.txt File/).waitFor()
  await contentRows(page).first().getByText('empty', { exact: true }).waitFor()
  const ordering = await page.evaluate(async id => {
    const prefix = `/api/v1/artifacts/${encodeURIComponent(id)}/inventory?parent=browser-tree&pageSize=2`
    const folders = await fetch(prefix + '&kind=Directory&order=folders').then(response => response.json())
    const oldOrder = await fetch(prefix).then(response => response.json())
    const invalid = await fetch(prefix + '&order=size&cursor=' + encodeURIComponent(oldOrder.nextCursor))
    return { paths: folders.items.map(item => item.path), invalid: invalid.status }
  }, treeArtifactId)
  if (!ordering.paths.every(path => ['browser-tree/empty', 'browser-tree/nested'].includes(path)) ||
      ordering.paths.length !== 2 || ordering.invalid !== 400) throw new Error(`Inventory directory filter or cursor binding failed: ${JSON.stringify(ordering)}`)

  const slowPath = 'browser-tree/zz-depth'
  const matchSlow = url => url.pathname.endsWith('/inventory') && url.searchParams.get('parent') === slowPath && !url.searchParams.has('kind')
  await page.route(matchSlow, async route => {
    await new Promise(resolve => setTimeout(resolve, 1200))
    await route.continue().catch(() => {})
  })
  await contentRows(page, /^zz-depth Folder/).dblclick()
  await page.getByText('Loading directory…').waitFor()
  await crumbs.getByRole('button', { name: 'browser-tree', exact: true }).click()
  await contentRows(page, /^zero-0000\.txt File/).waitFor()
  await page.unroute(matchSlow, undefined, { behavior: 'wait' })
  if (await contentRows(page, /^one Folder/).count()) throw new Error('Stale response replaced the directory')
  await contentRows(page, /^zz-depth Folder/).dblclick()
  for (const name of ['one', 'two', 'three']) await contentRows(page, new RegExp(`^${name} Folder`)).press('Enter')
  await contentRows(page, /^marker\.txt File/).waitFor()
  await page.goBack()
  await contentRows(page, /^three Folder/).waitFor()
  await page.goForward()
  await contentRows(page, /^marker\.txt File/).waitFor()
  await page.getByLabel('Search original paths').fill('tree.txt')
  await page.getByRole('button', { name: 'Search', exact: true }).click()
  const hit = page.getByRole('listbox', { name: 'Search results' }).getByRole('option', { name: /^tree\.txt File/ })
  await hit.getByText('browser-tree/nested/tree.txt', { exact: true }).waitFor()
  await hit.click()
  await page.getByRole('button', { name: 'Show in folder' }).click()
  await page.getByRole('listbox', { name: 'Directory contents' }).getByRole('option', { name: /^tree\.txt File/, selected: true }).waitFor({ timeout: 8000 }).catch(async cause => {
    throw new Error(`Search match was not highlighted at ${page.url()}: ${await contentRows(page).allTextContents()}`, { cause })
  })
  await page.getByLabel('Search original paths').fill('zero-6100.txt')
  await page.getByRole('button', { name: 'Search', exact: true }).click()
  await page.getByRole('listbox', { name: 'Search results' }).getByRole('option', { name: /^zero-6100\.txt File/ }).click()
  await page.getByRole('button', { name: 'Show in folder' }).click()
  await page.getByText('Search match shown above this page.').waitFor()
  await page.getByRole('listbox', { name: 'Directory contents' }).getByRole('option', { name: /^zero-6100\.txt File/, selected: true }).waitFor()
  if (await contentRows(page).count() !== 51) throw new Error('Revealing a later search match fetched extra pages')
  await contentRows(page, /^nested Folder/).dblclick()
  await crumbs.getByRole('button', { name: 'nested', exact: true }).waitFor({ timeout: 8000 }).catch(async cause => {
    throw new Error(`Opening the nested folder after revealing a later result failed at ${page.url()}`, { cause })
  })
  await contentRows(page, /^tree\.txt File/).waitFor({ timeout: 8000 })
  await page.getByLabel('Search original paths').fill('marker.txt')
  await page.getByRole('button', { name: 'Search', exact: true }).click()
  await page.getByRole('listbox', { name: 'Search results' }).getByRole('option', { name: /^marker\.txt File/ }).waitFor()
  await page.getByRole('button', { name: 'Clear search' }).click()
  await contentRows(page, /^tree\.txt File/).waitFor({ timeout: 8000 }).catch(async cause => {
    throw new Error(`Clearing search did not restore the nested folder at ${page.url()}`, { cause })
  })
  await page.getByLabel('Sort by').selectOption('size')
  await contentRows(page).first().getByText('é漢字.txt', { exact: true }).waitFor()
  await page.reload()
  await crumbs.getByRole('button', { name: 'nested', exact: true }).waitFor()
  await openEntryEditor(page)
  if (await dialog.getByLabel('Label (optional)').inputValue() !== 'Tree script') throw new Error('Entry point label was not restored after reload')
  await dialog.getByRole('button', { name: 'Close editor' }).click()

  const viewport = page.viewportSize()
  await page.setViewportSize({ width: 390, height: 780 })
  await page.getByRole('button', { name: 'Show folders' }).click()
  await folderTree.waitFor()
  await page.getByRole('button', { name: 'Hide folders' }).click()
  await contentRows(page, /^tree\.txt File/).focus()
  await page.keyboard.press('ArrowDown')
  if (await contentRows(page, /^é漢字\.txt File/).getAttribute('aria-selected') !== 'true') throw new Error('Keyboard selection failed at narrow width')
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)
  if (overflow) throw new Error('Artifact Explorer overflowed a narrow viewport')
  await page.setViewportSize(viewport)

  await page.getByRole('link', { name: 'Back to Release' }).click()
  await page.getByRole('link', { name: 'Installation', exact: true }).click()
  const configuration = page.getByRole('heading', { name: 'Installation Configuration' }).locator('..')
  await configuration.getByRole('button', { name: 'Configure installation', exact: true }).click()
  await configuration.getByLabel('Artifact association').selectOption(treeAssociationId)
  await configuration.getByText('Browse source tree', { exact: true }).click()
  await contentRows(configuration, /^browser-tree Folder/).dblclick()
  await contentRows(configuration, /^nested Folder/).dblclick()
  await contentRows(configuration, /^tree\.txt File/).click()
  await configuration.getByRole('button', { name: 'Use this file' }).click()
  await configuration.getByText('Source selection · browser-tree/nested/tree.txt').waitFor()
  await configuration.getByRole('button', { name: /^Review installation plan/ }).click()
  await configuration.getByLabel('Preview platform').selectOption('linux/x86_64')
  await configuration.getByRole('button', { name: 'Validate and preview' }).click()
  await configuration.getByText('1 files · 0 directories').waitFor()
  const priorViewport = page.viewportSize()
  for (const width of [540, 960]) {
    await page.setViewportSize({ width, height: 640 })
    const plan = configuration.locator('.installation-plan')
    await plan.scrollIntoViewIfNeeded()
    const overlaps = await plan.evaluate(list =>
      list.closest('section').getBoundingClientRect().bottom >
      document.querySelector('.installation-actions').getBoundingClientRect().top)
    if (overlaps) throw new Error('Save toolbar overlaps installation preview')
  }
  if (priorViewport) await page.setViewportSize(priorViewport)

  await configuration.getByRole('button', { name: /^Payload/ }).click()
  await configuration.getByLabel('Artifact association').selectOption('')
  await configuration.getByLabel('Artifact association').selectOption(treeAssociationId)
  await configuration.getByLabel('Installation destination').fill('')
  await configuration.getByRole('button', { name: /^Localization/ }).click()
  await configuration.getByLabel('Localization packaging').selectOption('fixed')
  await configuration.getByLabel('Language identifier').fill('ru-RU')
  await configuration.getByRole('button', { name: /^Prerequisites/ }).click()
  await configuration.getByText('Detect installation components', { exact: true }).click()
  await configuration.getByRole('button', { name: 'Detect components', exact: true }).click()
  await configuration.getByLabel('Microsoft DirectX', { exact: true }).waitFor()
  await configuration.getByLabel('NVIDIA PhysX', { exact: true }).uncheck()
  await configuration.getByRole('button', { name: 'Apply to installation editor' }).click()
  await configuration.getByRole('heading', { name: /1\. Microsoft DirectX/ }).waitFor()
  await configuration.getByRole('button', { name: 'Move down', exact: true }).first().click()
  await configuration.getByRole('heading', { name: /1\. Microsoft Visual C/ }).waitFor()
  await configuration.getByRole('button', { name: 'Add prerequisite installer' }).click()
  await configuration.getByRole('button', { name: 'Save installation configuration' }).click()
  await configuration.getByText('Enter a package name.', { exact: true }).waitFor()
  await configuration.getByLabel('Prerequisite name').fill('Fixture prerequisite')
  await configuration.getByLabel('Artifact association').selectOption(firstAssociationId)
  await configuration.getByRole('button', { name: /^Localization/ }).click()
  await configuration.getByLabel('Localization packaging').selectOption('selectable')
  await configuration.getByRole('button', { name: 'Add language choices' }).click()
  await configuration.getByRole('button', { name: 'Add choice', exact: true }).click()
  await configuration.getByRole('button', { name: 'Add choice', exact: true }).click()
  await configuration.getByLabel('Language identifier', { exact: true }).nth(0).fill('en-US')
  await configuration.getByLabel('Language identifier', { exact: true }).nth(1).fill('ru-RU')
  await configuration.getByLabel('Choice name', { exact: true }).nth(0).fill('English')
  await configuration.getByLabel('Choice name', { exact: true }).nth(1).fill('Russian')
  await configuration.getByLabel('Default choice', { exact: true }).nth(1).check()
  await configuration.getByRole('button', { name: 'Add copy operation', exact: true }).nth(1).click()
  await configuration.getByLabel('Artifact association').selectOption(firstAssociationId)
  await configuration.getByLabel('Installation destination').fill('lang-ru.dat')
  await configuration.getByRole('button', { name: /^Optional components/ }).click()
  await configuration.getByRole('button', { name: 'Add component choices' }).click()
  await configuration.getByRole('button', { name: 'Add choice', exact: true }).click()
  await configuration.getByLabel('Choice name').fill('Game manual')
  await configuration.getByLabel('Default choice').check()
  await configuration.getByRole('button', { name: 'Add copy operation' }).click()
  await configuration.getByLabel('Artifact association').selectOption(firstAssociationId)
  await configuration.getByLabel('Installation destination').fill('manual.dat')
  await configuration.getByRole('button', { name: /^Review installation plan/ }).click()
  await configuration.getByLabel('Game manual', { exact: true }).uncheck()
  await configuration.getByRole('button', { name: 'Validate and preview' }).click()
  await configuration.getByText('Valid for Linux x86_64', { exact: true }).waitFor()
  await configuration.getByRole('button', { name: 'Browse resulting files' }).click()
  await contentRows(configuration, /^lang-ru\.dat File/).waitFor()
  if (await contentRows(configuration, /^manual\.dat File/).count()) throw new Error('An unchecked component appeared in the plan')
  await configuration.getByRole('button', { name: /^Localization/ }).click()
  page.once('dialog', dialog => dialog.accept())
  await configuration.getByLabel('Localization packaging').selectOption('fixed')
  await configuration.getByLabel('Language identifier').fill('ru-RU')
  await configuration.getByRole('button', { name: /^Launch/ }).focus()
  await page.keyboard.press('Enter')
  await configuration.getByRole('heading', { name: 'Launch integration' }).waitFor()
  for (const width of [1440, 1024, 390]) {
    await page.setViewportSize({ width, height: 850 })
    if (await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)) throw new Error(`Installation workbench overflows at ${width}px`)
  }
  await page.setViewportSize(viewport)
  await configuration.getByRole('button', { name: /^Review installation plan/ }).click()
  await configuration.getByRole('button', { name: 'Validate and preview' }).click()
  await configuration.getByText(/61[0-9]{2} files ·/).waitFor()
  await configuration.getByRole('button', { name: 'Browse resulting files' }).click()
  await contentRows(configuration, /^browser-tree Folder/).dblclick()
  if (await configuration.getByRole('option').count() > 80) throw new Error('Resolved inventory is not paged')
  await configuration.getByRole('button', { name: 'Save installation configuration' }).click()
  await configuration.getByText('Installation configuration saved as a new declaration revision.').waitFor()
  const editorUrl = page.url()
  await page.reload()
  await configuration.getByRole('button', { name: /^Prerequisites/ }).click()
  await configuration.getByRole('heading', { name: /Fixture prerequisite/ }).waitFor()
  await configuration.getByRole('button', { name: /^Localization/ }).click()
  await configuration.getByLabel('Language identifier').fill('en-US')
  await page.getByRole('navigation', { name: 'Release sections' }).getByRole('link', { name: 'Overview' }).click()
  await page.goBack()
  if (page.url() !== editorUrl || await configuration.getByLabel('Language identifier').inputValue() !== 'en-US') throw new Error('Unsaved installation changes were lost during release navigation')
  await page.evaluate(async id => {
    const path = `/api/v1/releases/${id}/declarations`
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    const current = await fetch(path)
    const { data } = await current.json()
    const result = await fetch(path, {method:'PUT', headers:{'Content-Type':'application/json', 'If-Match':current.headers.get('ETag'), 'X-CSRF-Token':session.csrfToken},
      body: JSON.stringify({data: {...data, concurrent: true}}) })
    if (!result.ok) throw new Error(`Could not seed conflict: ${result.status}`)
  }, releaseId)
  await configuration.getByRole('button', { name: 'Save installation configuration' }).click()
  await configuration.getByText(/Conflict · a newer declaration exists/).waitFor()
  if (await configuration.getByLabel('Language identifier').inputValue() !== 'en-US') throw new Error('Conflict discarded the local draft')
  page.once('dialog', dialog => dialog.accept())
  await configuration.getByRole('button', { name: 'Discard changes' }).click()
  await configuration.getByRole('button', { name: /^Localization/ }).click()
  if (await configuration.getByLabel('Language identifier').inputValue() !== 'ru-RU') throw new Error('Discard did not reload the saved revision')
  const exactRoot = join(preview.namespace, 'exact-tree')
  await mkdir(exactRoot)
  await writeFile(join(exactRoot, '.DS_Store'), 'exact metadata')
  await writeFile(join(exactRoot, '.hidden-game'), 'exact hidden content')
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/uploads/new`)
  await page.getByLabel('Artifact type').selectOption('BareTree')
  await page.getByLabel('Directory files').setInputFiles(exactRoot)
  await page.getByText(/1 OS metadata files excluded/).waitFor()
  await page.getByLabel('Exclude common macOS and Windows metadata files').uncheck()
  await page.getByText(/0 OS metadata files excluded/).waitFor()
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText('Saved Draft for Portal Draft Edited').waitFor({ timeout: 30000 })
  const exactAssociation = await associationWithRoot(page, releaseId, 'exact-tree')
  const exactArtifact = await page.evaluate(async id => (await (await fetch(`/api/v1/associations/${id}`)).json()).artifactId, exactAssociation)
  const exactMetadata = await page.evaluate(async id => (await (await fetch(`/api/v1/artifacts/${encodeURIComponent(id)}/inventory?q=.DS_Store`)).json()).items, exactArtifact)
  if (!exactMetadata.some(item => item.path === 'exact-tree/.DS_Store')) throw new Error('Exact tree upload excluded OS metadata')
  const zipPath = join(preview.namespace, 'bundle.zip')
  execFileSync('python3', ['-c', 'import sys,zipfile; z=zipfile.ZipFile(sys.argv[1],"w",zipfile.ZIP_DEFLATED); z.writestr("zip/nested.txt","zip content"); z.close()', zipPath])
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/uploads/new`)
  await page.getByLabel('Artifact type').selectOption('ZipImport')
  await page.getByLabel('File', { exact: true }).setInputFiles(zipPath)
  await page.getByRole('button', { name: 'Save Draft' }).click()
  await page.getByText('Saved Draft for Portal Draft Edited').waitFor({ timeout: 30000 })
  await page.getByRole('link', { name: 'Open release' }).click()
  const zipAssociationId = await associationWithRoot(page, releaseId, 'zip')
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/artifacts/${zipAssociationId}`)
  await contentRows(page, /^zip Folder/).dblclick()
  await contentRows(page, /^nested\.txt File/).click()
  const zipFileLink = await page.getByRole('link', { name: 'Download', exact: true }).first().getAttribute('href')
  const zipBytes = await page.evaluate(async href => Array.from(new Uint8Array(await (await fetch(href)).arrayBuffer())), zipFileLink)
  if (Buffer.from(zipBytes).toString() !== 'zip content') throw new Error('ZIP-imported file download mismatch')
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/artifacts/${firstAssociationId}`)
  await page.getByRole('heading', { name: 'File Artifact' }).waitFor()
  await page.getByRole('button', { name: 'Add entry point' }).click()
  await page.getByRole('button', { name: 'Save entry points' }).click()
  await page.getByText('Entry points saved as a new declaration revision.').waitFor()
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}`)
  const priorAssociationIds = await page.evaluate(async id => (await (await fetch(`/api/v1/releases/${id}/artifacts?pageSize=100`)).json()).items.map(item => item.id), releaseId)
  await page.getByRole('navigation', { name: 'Release sections' }).getByRole('link', { name: 'Advanced', exact: true }).click()
  await page.getByLabel('Artifact ID').fill(originalArtifact)
  await page.getByRole('button', { name: 'Associate Artifact' }).click()
  await page.getByText('Existing Artifact associated').waitFor()
  const addedAssociationId = await page.evaluate(async ({ id, prior }) => (await (await fetch(`/api/v1/releases/${id}/artifacts?pageSize=100`)).json()).items.find(item => !prior.includes(item.id))?.id, { id: releaseId, prior: priorAssociationIds })
  if (!addedAssociationId) throw new Error('New association was not visible')
  await page.getByRole('link', { name: 'Artifacts', exact: true }).click()
  await page.locator(`a[href$="/artifacts/${addedAssociationId}"]`).locator('..').getByRole('button', { name: 'Remove association' }).click()
  await page.getByRole('dialog', { name: 'Remove association' }).getByRole('button', { name: 'Remove association' }).click()
  const associationRemoval = page.getByRole('heading', { name: 'Remove artifact association operation' }).locator('..')
  await associationRemoval.waitFor()
  await associationRemoval.getByText('Completed', { exact: true }).waitFor({ timeout: 15000 }).catch(async failure => {
    throw new Error(`Artifact association cleanup did not complete before publication: ${await associationRemoval.innerText()}`, { cause: failure })
  })
  await page.getByRole('button', { name: 'Publish release' }).click()
  await page.getByText('Release published. Artifact composition is now immutable.').waitFor()
  stage('creating and publishing a Derived App through the visual editor')
  await verifyVisualRecipe(page, base, preview, treeAssociationId)
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}`, { waitUntil: 'domcontentloaded' })
  stage('verifying publication, channels and App lifecycle')
  await page.getByRole('navigation', { name: 'Application navigation' }).getByRole('link', { name: 'Overview' }).click()
  await page.getByText('App publication: Published').waitFor()
  await page.getByText('Visibility: Public').waitFor()
  await page.reload()
  await page.getByText('App publication: Published').waitFor()
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}`)
  if (await page.getByRole('button', { name: 'Associate Artifact' }).count()) throw new Error('Published composition exposed association action')
  const immutable = await page.evaluate(async ({ releaseId, artifactId, key }) => {
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    const response = await fetch(`/api/v1/releases/${releaseId}/artifacts`, { method: 'POST', headers: { 'Content-Type': 'application/json',
      'X-CSRF-Token': session.csrfToken, 'Idempotency-Key': key }, body: JSON.stringify({ artifactId }) })
    await response.arrayBuffer()
    return response.status
  }, { releaseId, artifactId: originalArtifact, key: randomUUID() })
  if (immutable !== 409) throw new Error(`Published Release composition was mutable: ${immutable}`)
  const catalogAfterRejection = await page.evaluate(async id => {
    const response = await fetch(`/api/v1/apps/${id}`, { signal: AbortSignal.timeout(5000) })
    await response.arrayBuffer()
    return response.status
  }, draftId).catch(async failure => {
    throw new Error(`Catalog request stalled after immutable association rejection: ${JSON.stringify(await diagnosePreview(page, draftId))}`, { cause: failure })
  })
  if (catalogAfterRejection !== 200) throw new Error(`Catalog did not recover after immutable association rejection: ${catalogAfterRejection}`)
  stage('checking published Artifact Explorer and declaration edit')
  // The Portal is an SPA: a document load is sufficient before checking its
  // actual API-backed screen. Waiting for every image to load can stall the
  // navigation without telling us whether the Explorer itself works.
  await page.goto(`${base}apps/${draftId}/releases/${releaseId}/artifacts/${treeAssociationId}`, { waitUntil: 'domcontentloaded' }).catch(async failure => {
    throw new Error(`Explorer navigation stalled: ${JSON.stringify(await diagnosePreview(page, draftId))}`, { cause: failure })
  })
  await page.getByRole('heading', { name: 'Artifact Explorer' }).waitFor().catch(async failure => {
    const details = await page.evaluate(async ({ appId, releaseId, associationId }) => {
      const endpoints = { ready: '/health/ready', app: `/apps/${appId}`, release: `/releases/${releaseId}`,
        association: `/associations/${associationId}`, declarations: `/associations/${associationId}/declarations` }
      const statuses = Object.fromEntries(await Promise.all(Object.entries(endpoints).map(async ([name, path]) => {
        try { return [name, (await fetch(`/api/v1${path}`, { signal: AbortSignal.timeout(3000) })).status] }
        catch (error) { return [name, error instanceof Error ? error.name : String(error)] }
      })))
      return { route: location.pathname, screen: document.querySelector('main')?.innerText.slice(0, 800), statuses }
    }, { appId: draftId, releaseId, associationId: treeAssociationId }).catch(error => ({ diagnosticError: String(error) }))
    throw new Error(`Published Artifact Explorer did not open: ${JSON.stringify({ ...details, preview: await diagnosePreview(page, draftId) })}`, { cause: failure })
  })
  await openEntryEditor(page)
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
    const label = role === 'icon' ? 'Icon' : role === 'logo' ? 'Logo' : 'Hero'
    const chooser = page.waitForEvent('filechooser')
    const card = role === 'logo' ? page.getByRole('heading', { name: 'General' }).locator('..').getByRole('button', { name: `Add ${label} artwork` }) : page.getByRole('button', { name: `Add ${label} artwork` })
    await card.click()
    await (await chooser).setFiles(artworkFixtures[fixture])
    await page.locator('.artwork-card').filter({ has: page.getByRole('heading', { name: label, exact: true }) }).getByText('Artwork added.').waitFor()
  }
  const integratedReplace = page.waitForEvent('filechooser')
  await page.getByRole('button', { name: 'Replace Icon artwork' }).click()
  await (await integratedReplace).setFiles(artworkFixtures.jpeg)
  await page.locator('.artwork-card').filter({ has: page.getByRole('heading', { name: 'Icon', exact: true }) }).getByText('Artwork replaced.').waitFor()
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
  await page.getByRole('navigation', { name: 'Release sections' }).getByRole('link', { name: 'Advanced', exact: true }).click()
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
  stage('testing new-App upload and publication')
  await page.getByRole('link', { name: 'Add Artifact to a new App' }).first().click()
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
  stage('testing online backup and isolated offline restore')
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
    stage('testing deletion, administration and account access')
    const sourceState = await page.evaluate(async ({ appId, associationId }) => ({
      app: await (await fetch(`/api/v1/apps/${appId}/declarations`)).json(),
      association: await (await fetch(`/api/v1/associations/${associationId}/declarations`)).json(),
      artwork: await (await fetch(`/api/v1/apps/${appId}/artwork`)).json(),
    }), integratedIds)
    if (sourceState.app.data.appDepot?.kind !== 'tool' || sourceState.association.data.appDepot?.entryPoints?.length !== 1 || sourceState.artwork.items.length !== 3) {
      throw new Error('Source instance changed during isolated offline restore')
    }
    await page.goto(`${base}apps/${draftId}/releases/${integratedIds.releaseId}`)
    await page.getByRole('navigation', { name: 'Release sections' }).getByRole('link', { name: 'Advanced', exact: true }).click()
    await page.getByRole('button', { name: 'Delete release' }).click()
    await page.getByRole('dialog', { name: 'Delete release' }).getByRole('button', { name: 'Delete release' }).click()
    await page.getByRole('heading', { name: 'Delete release operation' }).waitFor()
    await page.goto(`${base}apps/${draftId}`)
    await page.getByText('App publication: Draft').waitFor()
    await page.reload()
    await page.getByText('App publication: Draft').waitFor()
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
  const restricted = await page.evaluate(async appId => {
    const release = (await fetch(`/api/v1/apps/${appId}/releases`).then(response => response.json())).items[0]
    const association = (await fetch(`/api/v1/releases/${release.id}/artifacts`).then(response => response.json())).items[0]
    return { releaseId: release.id, associationId: association.id }
  }, privateAppId)
  const restrictedUrl = `${base}apps/${privateAppId}/releases/${restricted.releaseId}/artifacts/${restricted.associationId}`
  await reader.goto(restrictedUrl)
  await reader.getByRole('heading', { name: 'Application unavailable' }).waitFor()
  const setFixtureVisibility = visibility => page.evaluate(async ({ id, visibility }) => {
    const app = await fetch(`/api/v1/apps/${id}`)
    const session = await fetch('/api/v1/auth/sessions/current').then(response => response.json())
    const change = await fetch(`/api/v1/apps/${id}`, { method: 'PATCH',
      headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': session.csrfToken, 'If-Match': app.headers.get('ETag') },
      body: JSON.stringify({ visibility }) })
    if (!change.ok) throw new Error(`Explorer visibility fixture update failed: ${change.status}`)
  }, { id: privateAppId, visibility })
  await setFixtureVisibility('Public')
  await reader.goto(restrictedUrl)
  await reader.getByRole('link', { name: 'Download original file' }).waitFor()
  if (await reader.getByRole('button', { name: 'Add entry point' }).count()) throw new Error('Unprivileged reader saw the add-entry-point action')
  await openEntryEditor(reader)
  await entryEditor(reader).getByText('Saved references: Absent').waitFor()
  if (await entryEditor(reader).getByRole('button', { name: 'Save entry points' }).count()) throw new Error('Unprivileged reader saw an entry point write action')
  await setFixtureVisibility('Private')
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
    let serverFileIdentity
    let serverTreeTarget
    stage('importing File, Tree and ZIP Artifacts from configured roots and Inbox')
    for (const [rootId, sourceName, mode, appName] of [
      ['shared', 'server-file.txt', 'BareFile', 'Server imported file'],
      ['shared', 'server-tree', 'BareTree', 'Server imported tree'],
      ['shared', 'server.zip', 'ZipImport', 'Server imported ZIP'],
      ['inbox', 'inbox-file.txt', 'BareFile', 'Inbox imported file'],
      ['inbox', 'inbox-tree', 'BareTree', 'Inbox imported tree'],
      ['inbox', 'inbox.zip', 'ZipImport', 'Inbox imported ZIP']
    ]) {
      stage(`importing ${mode} from ${rootId}: ${sourceName}`)
      await page.goto(`${base}${rootId === 'inbox' ? 'artifacts/new-app?source=server' : 'import/new-app'}`)
      await page.getByRole('heading', { name: 'Add Artifact' }).waitFor()
      await page.getByLabel('Artifact type').selectOption(mode)
      await page.getByLabel('Import root').selectOption(rootId)
      await page.getByLabel('App name').fill(appName)
      await page.getByRole('button', { name: sourceName }).click()
      if (mode === 'BareTree') await page.getByRole('button', { name: 'Select this directory' }).click()
      const publishImport = rootId === 'inbox' && mode === 'ZipImport'
      await page.getByRole('button', { name: publishImport ? 'Import and publish' : 'Import and save Draft' }).click()
      try { await page.getByRole('heading', { name: 'Completed' }).waitFor({ timeout: 30000 }) }
      catch { throw new Error(`Server import ${mode} did not complete: ${(await page.locator('body').innerText()).slice(-1200)}`) }
      await page.getByText('Artifact registration completed.').waitFor()
      await page.getByText(publishImport ? 'The Release is Published.' : 'The Release remains Draft until you publish it.').waitFor()
      if (await page.getByLabel('Import in progress').count()) throw new Error('Completed import retained indeterminate progress')
      if ((await page.getByRole('region', { name: 'Import progress' }).innerText()).includes('Server-side work is in progress')) {
        throw new Error('Completed import retained active work message')
      }
      const releaseLink = page.getByRole('link', { name: 'Open release' })
      await releaseLink.waitFor()
      const importedReleaseId = (await releaseLink.getAttribute('href')).split('/').pop()
      if (rootId === 'shared' && mode === 'BareTree') {
        const appLink = await page.getByRole('link', { name: 'Open application' }).getAttribute('href')
        serverTreeTarget = { appId: appLink.split('/').pop(), releaseId: importedReleaseId }
      }
      const imported = await page.evaluate(async ({ releaseId, mode }) => {
        const associationResponse = await fetch(`/api/v1/releases/${releaseId}/artifacts?pageSize=100`)
        if (!associationResponse.ok) throw new Error(`Imported associations unavailable: ${associationResponse.status}`)
        const associations = await associationResponse.json()
        if (associations.items.length !== 1) throw new Error(`Expected one imported Artifact, found ${associations.items.length}`)
        const artifactId = associations.items[0].artifactId
        if (mode === 'BareFile') {
          const response = await fetch(`/api/v1/artifacts/${encodeURIComponent(artifactId)}/content`)
          if (!response.ok) throw new Error(`Imported File download failed: ${response.status}`)
          return { artifactId, bytes: Array.from(new Uint8Array(await response.arrayBuffer())) }
        }
        const inventoryResponse = await fetch(`/api/v1/artifacts/${encodeURIComponent(artifactId)}/inventory?pageSize=100`)
        if (!inventoryResponse.ok) throw new Error(`Imported Tree inventory failed: ${inventoryResponse.status}`)
        const inventory = await inventoryResponse.json()
        const file = inventory.items.find(item => item.type === 'File')
        if (!file) throw new Error('Imported Tree has no file')
        const response = await fetch(`/api/v1/artifacts/${encodeURIComponent(artifactId)}/files/${encodeURIComponent(file.fileId)}`)
        if (!response.ok) throw new Error(`Imported Tree file download failed: ${response.status}`)
        return { artifactId, inventory: inventory.items.map(item => item.path), path: file.path,
          bytes: Array.from(new Uint8Array(await response.arrayBuffer())) }
      }, { releaseId: importedReleaseId, mode })
      const expected = rootId === 'inbox'
        ? mode === 'BareFile' ? Buffer.from('Inbox file fixture') : mode === 'BareTree' ? Buffer.from('Inbox tree content') : Buffer.from('Inbox ZIP content')
        : mode === 'BareFile' ? Buffer.from('Server-side import fixture') : mode === 'BareTree' ? Buffer.from('Server tree content') : Buffer.from('Server ZIP content')
      if (!Buffer.from(imported.bytes).equals(expected)) throw new Error(`Server import ${mode} returned incorrect bytes`)
      if (mode === 'BareFile' && imported.artifactId !== `adfile:sha256:${createHash('sha256').update(expected).digest('hex')}`) {
        throw new Error('Server import File identity differs from original-byte identity')
      }
      if (rootId === 'shared' && mode === 'BareFile') serverFileIdentity = imported.artifactId
      if (mode === 'BareTree' && (!imported.inventory.includes('empty') || imported.path !== 'nested/content.txt')) {
        throw new Error('Server import did not retain Tree paths and empty directory')
      }
      if (mode === 'ZipImport' && imported.path !== 'zip/nested.txt') throw new Error('Server ZIP import path is incorrect')
      if (rootId === 'inbox' && mode === 'BareFile') {
        if ((await readFile(join(inboxPath, sourceName), 'utf8')) !== 'Inbox file fixture') throw new Error('Import modified Inbox source')
        await rm(join(inboxPath, sourceName))
        const afterRemoval = await page.evaluate(async artifactId => {
          const response = await fetch(`/api/v1/artifacts/${encodeURIComponent(artifactId)}/content`)
          if (!response.ok) throw new Error(`Registered Artifact vanished after source removal: ${response.status}`)
          return Array.from(new Uint8Array(await response.arrayBuffer()))
        }, imported.artifactId)
        if (!Buffer.from(afterRemoval).equals(expected)) throw new Error('CAS Artifact changed after Inbox source removal')
      }
    }
    stage('adding a second Artifact to a Draft Release')
    await page.goto(`${base}apps/${serverTreeTarget.appId}/releases/${serverTreeTarget.releaseId}/artifacts/new?source=server&mode=BareTree`)
    await page.getByRole('heading', { name: 'Add Artifact' }).waitFor()
    await page.getByLabel('Import root').selectOption('inbox')
    await page.getByRole('button', { name: 'inbox-tree' }).click()
    await page.getByRole('button', { name: 'Select this directory' }).click()
    await page.getByRole('button', { name: 'Import and save Draft' }).click()
    await page.getByText('Artifact registration completed.').waitFor({ timeout: 30000 })
    const associatedCount = await page.evaluate(async releaseId => (await (await fetch(`/api/v1/releases/${releaseId}/artifacts?pageSize=100`)).json()).items.length, serverTreeTarget.releaseId)
    if (associatedCount !== 2) throw new Error(`Draft Release did not retain both Artifacts: ${associatedCount}`)
    stage('verifying opaque ZIP File Artifact')
    await page.goto(`${base}artifacts/new-app?source=server&mode=BareFile`)
    await page.getByLabel('App name').fill('Opaque Inbox ZIP')
    await page.getByRole('button', { name: 'inbox.zip' }).click()
    await page.getByRole('button', { name: 'Import and save Draft' }).click()
    const opaqueReleaseLink = page.getByRole('link', { name: 'Open release' })
    await opaqueReleaseLink.waitFor()
    const opaqueReleaseId = (await opaqueReleaseLink.getAttribute('href')).split('/').pop()
    const opaqueZip = await page.evaluate(async releaseId => {
      const associations = await (await fetch(`/api/v1/releases/${releaseId}/artifacts?pageSize=100`)).json()
      const artifactId = associations.items[0].artifactId
      const metadata = await (await fetch(`/api/v1/artifacts/${encodeURIComponent(artifactId)}`)).json()
      const content = await (await fetch(`/api/v1/artifacts/${encodeURIComponent(artifactId)}/content`)).arrayBuffer()
      return { kind: metadata.kind, bytes: Array.from(new Uint8Array(content)) }
    }, opaqueReleaseId)
    if (opaqueZip.kind !== 'File' || !Buffer.from(opaqueZip.bytes).equals(await readFile(join(inboxPath, 'inbox.zip')))) {
      throw new Error('Opaque ZIP was extracted or its original bytes changed')
    }
    await page.goto(`${base}upload/new-app`)
    await page.getByLabel('App name').fill('HTTP identity comparison')
    await page.getByLabel('File', { exact: true }).setInputFiles({ name: 'server-file.txt', mimeType: 'text/plain', buffer: Buffer.from('Server-side import fixture') })
    await page.getByRole('button', { name: 'Save Draft' }).click()
    await page.getByRole('link', { name: 'Open release' }).waitFor()
    const httpReleaseId = (await page.getByRole('link', { name: 'Open release' }).getAttribute('href')).split('/').pop()
    const httpIdentity = await page.evaluate(async id => {
      const response = await fetch(`/api/v1/releases/${id}/artifacts?pageSize=100`)
      if (!response.ok) throw new Error(`HTTP upload association unavailable: ${response.status}`)
      return (await response.json()).items[0].artifactId
    }, httpReleaseId)
    if (httpIdentity !== serverFileIdentity) throw new Error('HTTP and Server import created different File Artifact identities for the same bytes')
    if (process.env.GOG_TEST_FIXTURES) {
      stage('analyzing, importing and publishing a synthetic multipart Inno installer')
      await verifyGogImporter(page, base, inboxPath, process.env.GOG_TEST_FIXTURES, preview)
    }
    stage('checking import authorization, traversal and CSRF boundaries')
    const importBoundary = await page.evaluate(async () => {
      const invalidPath = await fetch('/api/v1/server-imports/roots/shared/entries?path=..%2Foutside')
      const symlinkEscape = await fetch('/api/v1/server-imports/roots/inbox/entries?path=escape-managed-parent')
      const inboxListing = await (await fetch('/api/v1/server-imports/roots/inbox/entries')).json()
      const session = await (await fetch('/api/v1/auth/sessions/current')).json()
      const noCsrf = await fetch('/api/v1/server-imports', { method: 'POST', headers: {
        'Content-Type': 'application/json', 'Idempotency-Key': 'missing-csrf' },
      body: JSON.stringify({ rootId: 'shared', path: 'server-file.txt', mode: 'BareFile', target: { newAppName: 'Denied import' } }) })
      return { invalidPath: invalidPath.status, symlinkEscape: symlinkEscape.status,
        symlinkListed: inboxListing.items.some(item => item.name === 'escape-managed-parent'), noCsrf: noCsrf.status, csrfPresent: !!session.csrfToken }
    })
    if (importBoundary.invalidPath !== 400 || importBoundary.symlinkEscape !== 400 || importBoundary.symlinkListed || importBoundary.noCsrf !== 403 || !importBoundary.csrfPresent) {
      throw new Error(`Server import path or CSRF boundary failed: ${JSON.stringify(importBoundary)}`)
    }
    const readerRoots = await reader.evaluate(async () => {
      const list = await fetch('/api/v1/server-imports/roots')
      const page = await fetch('/api/v1/server-imports/roots/shared/entries')
      const adminSources = await fetch('/api/v1/admin/import-sources')
      const session = await (await fetch('/api/v1/auth/sessions/current')).json()
      const start = await fetch('/api/v1/server-imports', { method: 'POST', headers: {
        'Content-Type': 'application/json', 'X-CSRF-Token': session.csrfToken, 'Idempotency-Key': 'reader-denied-import' },
      body: JSON.stringify({ rootId: 'shared', path: 'server-file.txt', mode: 'BareFile', target: { newAppName: 'Denied' } }) })
      return { list: list.status, roots: (await list.json()).items, page: page.status, adminSources: adminSources.status, start: start.status }
    })
    if (readerRoots.list !== 200 || readerRoots.roots.length || readerRoots.page !== 404 || readerRoots.adminSources !== 403 || readerRoots.start !== 404) {
      throw new Error(`Server import root authorization failed: ${JSON.stringify(readerRoots)}`)
    }
    stage('checking changed-source rejection during Server import')
    const changingPath = join(preview.importSource, 'changing.bin')
    const changingFile = await open(changingPath, 'w')
    await changingFile.truncate(64 * 1024 * 1024)
    await changingFile.close()
    await page.exposeFunction('changeImportSource', async () => {
      const file = await open(changingPath, 'r+')
      await file.write(Buffer.from('changed'), 0, 7, 0)
      await file.close()
    })
    const changedImport = await page.evaluate(async key => {
      const session = await (await fetch('/api/v1/auth/sessions/current')).json()
      const response = await fetch('/api/v1/server-imports', { method: 'POST', headers: {
        'Content-Type': 'application/json', 'X-CSRF-Token': session.csrfToken, 'Idempotency-Key': key },
      body: JSON.stringify({ rootId: 'shared', path: 'changing.bin', mode: 'BareFile', target: { newAppName: 'Changed server source' } }) })
      if (!response.ok || !response.body) throw new Error(`Changed-source fixture failed to start: ${response.status}`)
      const reader = response.body.getReader()
      const decoder = new TextDecoder()
      let buffered = ''
      let changed = false
      for (;;) {
        const chunk = await reader.read()
        if (chunk.done) throw new Error('Changed-source import ended without a terminal event')
        buffered += decoder.decode(chunk.value)
        let boundary
        while ((boundary = buffered.indexOf('\n')) >= 0) {
          const line = buffered.slice(0, boundary)
          buffered = buffered.slice(boundary + 1)
          if (!line.startsWith('data: ')) continue
          const event = JSON.parse(line.slice(6))
          if (event.phase === 'Receiving' && !changed) {
            changed = true
            await window.changeImportSource()
          }
          if (event.phase === 'Failed' || event.phase === 'Completed') return event
        }
      }
    }, randomUUID())
    if (changedImport.phase !== 'Failed' || !['source_changed', 'hash_mismatch'].includes(changedImport.problem?.code)) {
      throw new Error(`Changed Server source was not rejected: ${JSON.stringify(changedImport)}`)
    }
    stage('checking interrupted Server import and restart recovery')
    const restartFile = await open(join(preview.importSource, 'restart.bin'), 'w')
    await restartFile.truncate(128 * 1024 * 1024)
    await restartFile.close()
    const restartKey = randomUUID()
    const restartRequest = { rootId: 'shared', path: 'restart.bin', mode: 'BareFile', target: { newAppName: 'Restarted server import' } }
    const startedSession = await page.evaluate(async ({ key, body }) => {
      const session = await (await fetch('/api/v1/auth/sessions/current')).json()
      const response = await fetch('/api/v1/server-imports', { method: 'POST', headers: {
        'Content-Type': 'application/json', 'X-CSRF-Token': session.csrfToken, 'Idempotency-Key': key }, body: JSON.stringify(body) })
      if (!response.ok || !response.body) throw new Error(`Restart import start failed: ${response.status}`)
      const reader = response.body.getReader()
      const decoder = new TextDecoder()
      let text = ''
      for (;;) {
        const chunk = await reader.read()
        if (chunk.done) throw new Error('Import ended before a resumable session was created')
        text += decoder.decode(chunk.value)
        let boundary
        while ((boundary = text.indexOf('\n')) >= 0) {
          const line = text.slice(0, boundary)
          text = text.slice(boundary + 1)
          if (!line.startsWith('data: ')) continue
          const event = JSON.parse(line.slice(6))
          if (event.phase === 'Receiving' && event.sessionId) {
            await reader.cancel()
            return event.sessionId
          }
        }
      }
    }, { key: restartKey, body: restartRequest })
    preview.server.kill('SIGKILL')
    await new Promise(resolve => preview.server.once('exit', resolve))
    await restartLanPreview(preview)
    await page.reload()
    const resumed = await page.evaluate(async ({ key, body }) => {
      const session = await (await fetch('/api/v1/auth/sessions/current')).json()
      const response = await fetch('/api/v1/server-imports', { method: 'POST', headers: {
        'Content-Type': 'application/json', 'X-CSRF-Token': session.csrfToken, 'Idempotency-Key': key }, body: JSON.stringify(body) })
      if (!response.ok) throw new Error(`Import retry failed: ${response.status}`)
      return (await response.text()).split('\n').filter(line => line.startsWith('data: ')).map(line => JSON.parse(line.slice(6)))
    }, { key: restartKey, body: restartRequest })
    if (resumed.at(-1)?.result?.phase !== 'Completed' || resumed.at(-1)?.sessionId !== startedSession) {
      throw new Error(`Interrupted Server import did not resume its session: ${JSON.stringify(resumed.at(-1))}`)
    }
    stage('checking active Server import cancellation')
    const cancelled = await page.evaluate(async key => {
      const session = await (await fetch('/api/v1/auth/sessions/current')).json()
      const started = await fetch('/api/v1/server-imports', { method: 'POST', headers: {
        'Content-Type': 'application/json', 'X-CSRF-Token': session.csrfToken, 'Idempotency-Key': key },
      body: JSON.stringify({ rootId: 'shared', path: 'restart.bin', mode: 'BareFile', target: { newAppName: 'Cancelled server import' } }) })
      if (!started.ok) throw new Error(`Cancel fixture failed to start: ${started.status}`)
      const stop = await fetch('/api/v1/server-imports', { method: 'DELETE', headers: {
        'Content-Type': 'application/json', 'X-CSRF-Token': session.csrfToken }, body: JSON.stringify({ requestKey: key }) })
      if (stop.status !== 202) throw new Error(`Active import cancellation was not accepted: ${stop.status}`)
      const events = (await started.text()).split('\n').filter(line => line.startsWith('data: ')).map(line => JSON.parse(line.slice(6)))
      return events.at(-1)
    }, randomUUID())
    if (cancelled?.phase !== 'Failed' || cancelled.problem?.code !== 'session_cancelled') {
      throw new Error(`Cancelled Server import did not stop safely: ${JSON.stringify(cancelled)}`)
    }
    stage('checking unavailable import roots and reader permissions')
    const inboxMode = (await stat(inboxPath)).mode & 0o777
    const sharedMode = (await stat(preview.importSource)).mode & 0o777
    try {
      await chmod(inboxPath, 0)
      await chmod(preview.importSource, 0)
      await page.goto(`${base}artifacts/new-app?source=server`)
      await page.getByText('No server import roots are available to your account.').waitFor()
    } finally {
      await chmod(inboxPath, inboxMode)
      await chmod(preview.importSource, sharedMode)
    }
    await reader.goto(`${base}import/new-app`)
    await reader.getByText('You do not have permission to import to this target.').waitFor()
  }
  await reader.locator('.account-menu summary').click()
  await reader.locator('.account-menu').getByRole('button', { name: 'Sign out' }).click()
  await reader.getByRole('heading', { name: 'Sign in' }).waitFor()
  await reader.close()
  stage('signing out and cleaning up Preview resources')
  await page.evaluate(async () => {
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    const response = await fetch('/api/v1/auth/sessions/current', { method: 'DELETE', headers: { 'X-CSRF-Token': session.csrfToken } })
    if (!response.ok) throw new Error(`Session invalidation failed: ${response.status}`)
  })
  await page.reload()
  await page.getByRole('heading', { name: 'Sign in' }).waitFor()
  console.log('Browser smoke passed: setup, auth, App/Release management, File/Tree/ZIP Explorer and entry points, metadata, artwork, declarations, channels, visual recipes, deletion, administration, account, operations, LAN HTTP, backup and isolated restore')
} catch (failure) {
  console.error(`Browser smoke failure context: ${JSON.stringify(await diagnosePreview(page))}`)
  throw failure
} finally {
  stage('closing browser and Preview Server')
  await browser?.close()
  if (preview) {
    await stopServer(preview.server, 'Preview Server during cleanup')
    await rm(preview.namespace, { recursive: true, force: true })
    await rm(preview.importSource, { recursive: true, force: true })
  }
  clearInterval(heartbeat)
  clearTimeout(deadline)
}
