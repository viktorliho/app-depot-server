import { chromium } from 'playwright-core'
import { readFile } from 'node:fs/promises'

const browserPath = process.env.APP_DEPOT_BROWSER_BINARY ||
  (process.platform === 'darwin' ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : '/usr/bin/google-chrome')
const secretFile = process.env.APP_DEPOT_BOOTSTRAP_SECRET_FILE
if (!secretFile) throw new Error('APP_DEPOT_BOOTSTRAP_SECRET_FILE is required')
const password = (await readFile(secretFile, 'utf8')).replace(/\n$/, '')
const base = process.env.APP_DEPOT_BASE_URL || 'http://127.0.0.1:8787/'
const browser = await chromium.launch({ executablePath: browserPath, headless: true })
try {
  const page = await browser.newPage()
  await page.goto(base)
  await page.getByText('Server ready').waitFor()
  await page.getByLabel('Username').fill('admin')
  await page.getByLabel('Password').fill(password)
  await page.getByRole('button', { name: 'Sign in' }).click()
  await page.getByText('Signed in as admin').waitFor()
  const name = `Browser smoke ${Date.now()}`
  await page.getByLabel('App name').fill(name)
  await page.getByLabel('File').setInputFiles({ name: 'a.txt', mimeType: 'text/plain', buffer: Buffer.from('A') })
  await page.getByRole('button', { name: 'Upload and publish' }).click()
  await page.getByText(`Published ${name}`).waitFor({ timeout: 30000 })
  const backup = await page.evaluate(async () => {
    const session = await (await fetch('/api/v1/auth/sessions/current')).json()
    const response = await fetch('/api/v1/backups', {
      method: 'POST',
      headers: { 'X-CSRF-Token': session.csrfToken, 'Idempotency-Key': crypto.randomUUID() }
    })
    if (response.status !== 202) throw new Error(`Backup acceptance failed: ${response.status}`)
    return response.json()
  })
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
  await browser.close()
}
