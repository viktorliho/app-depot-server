import { chromium } from 'playwright-core'
import assert from 'node:assert/strict'

const browser = await chromium.launch({
  executablePath:
    process.env.APP_DEPOT_BROWSER_BINARY || '/usr/bin/google-chrome',
  headless: true,
  args: ['--no-sandbox'],
})
try {
  const page = await browser.newPage()
  const server = process.argv[2]
  await page.goto(server + '/')
  await page.getByLabel('Username').fill('admin')
  await page
    .getByLabel('Password', { exact: true })
    .fill(process.env.STORE_FIXTURE_PASSWORD)
  await page.getByRole('button', { name: 'Sign in' }).click()
  await page.getByRole('link', { name: 'Store', exact: true }).click()
  await page.getByRole('button', { name: 'Refresh catalog' }).click()
  await page.getByRole('heading', { name: 'Store Fixture' }).waitFor()
  await page.route('**/api/v1/admin/store/refresh', (route) =>
    route.fulfill({
      status: 503,
      contentType: 'application/problem+json',
      body: JSON.stringify({
        status: 503,
        title: 'Upstream temporarily unavailable',
        code: 'upstream_unavailable',
      }),
    })
  )
  await page.getByRole('button', { name: 'Refresh catalog' }).click()
  await page.getByRole('alert').waitFor()
  await page.getByRole('heading', { name: 'Store Fixture' }).waitFor()
  await page.reload()
  await page.getByRole('heading', { name: 'Store Fixture' }).waitFor()
  await page.unroute('**/api/v1/admin/store/refresh')
  for (const width of [1440, 1024, 390]) {
    await page.setViewportSize({ width, height: 900 })
    assert.ok(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth
      )
    )
  }
  await page.setViewportSize({ width: 1280, height: 900 })
  await page.getByRole('searchbox').fill('absent application')
  await page
    .getByRole('heading', { name: 'Store Fixture' })
    .waitFor({ state: 'detached' })
  await page.getByRole('searchbox').fill('Fixture')
  await page.getByRole('button', { name: 'View application' }).click()
  await page.getByText('Acquisition: Available').waitFor()
  await page.getByRole('checkbox', { name: /Windows/ }).uncheck()
  await page.getByRole('checkbox', { name: /macOS/ }).uncheck()
  await page.getByRole('button', { name: 'Add to server' }).click()
  await page.getByRole('link', { name: 'Open application' }).waitFor()
  await page.getByText('Observation settings', { exact: true }).click()
  await page.getByLabel('Check interval (hours)').fill('8')
  await page.getByRole('button', { name: 'Save settings' }).click()
  await page.waitForFunction(
    async () =>
      (await (await fetch('/api/v1/admin/store/observations')).json())[0]
        .hours === 8
  )
  await page.getByRole('button', { name: 'Check for updates' }).click()
  assert.equal(await page.getByRole('alert').count(), 0)
} finally {
  await browser.close()
}
