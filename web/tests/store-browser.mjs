import { chromium } from 'playwright-core'
import assert from 'node:assert/strict'

const browser = await chromium.launch({
  executablePath: process.env.APP_DEPOT_BROWSER_BINARY || '/usr/bin/google-chrome',
  headless: true,
  args: ['--no-sandbox'],
})
try {
  const page = await browser.newPage()
  const server = process.argv[2]
  await page.goto(server + '/')
  await page.getByLabel('Username').fill('admin')
  await page.getByLabel('Password', { exact: true }).fill(process.env.STORE_FIXTURE_PASSWORD)
  await page.getByRole('button', { name: 'Sign in' }).click()
  await page.getByRole('link', { name: 'Store', exact: true }).click()
  await page.getByRole('button', { name: 'Refresh catalog' }).click()
  await page.getByText('Catalog generation 1').waitFor()
  await page.getByRole('searchbox').fill('absent application')
  await page.locator('summary').filter({ hasText: 'Store Fixture' }).waitFor({ state: 'detached' })
  await page.getByRole('searchbox').fill('Fixture')
  await page.locator('summary').filter({ hasText: 'Store Fixture' }).click()
  await page.getByRole('button', { name: 'Add to server' }).click()
  await page.getByRole('heading', { name: 'Observed applications' }).waitFor()
  await page.getByRole('link', { name: 'Store Fixture', exact: true }).waitFor()
  await page.getByLabel('Check interval (hours)').fill('8')
  await page.getByRole('button', { name: 'Save settings' }).click()
  await page.waitForFunction(async () => (await (await fetch('/api/v1/admin/store/observations')).json())[0].hours === 8)
  await page.getByRole('button', { name: 'Check for updates' }).click()
  assert.equal(await page.getByRole('alert').count(), 0)
} finally {
  await browser.close()
}
