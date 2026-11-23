import { test } from 'node:test'
import assert from 'node:assert/strict'
import { isRelativeInstallPath, planTotals, platformName, prerequisiteIssue, publisherEvidence } from '../src/installation-review.ts'

test('publisher defaults satisfy evidence and identify missing installer references', () => {
  const item = { name: 'DirectX', associationId: 'tree', path: '', evidence: publisherEvidence, platforms: ['windows/x86_64'] }
  assert.equal(prerequisiteIssue(item, true)?.field, 'source')
  assert.equal(prerequisiteIssue({ ...item, associationId: 'single-file' }, false), undefined)
  assert.equal(prerequisiteIssue({ ...item, path: 'redist/DXSETUP.exe', evidence: ' ' }, true)?.field, 'evidence')
  assert.equal(prerequisiteIssue({ ...item, path: 'redist/DXSETUP.exe' }, true), undefined)
  assert.equal(platformName(item.platforms[0]), 'Windows x86_64')
})

test('plan totals count final output paths and logical bytes', () => {
  const files = Object.fromEntries(Array.from({ length: 6023 }, (_, i) => [`data/${i}`, { size: i }]))
  assert.deepEqual(planTotals(files, ['data']), { files: 6023, directories: 1, bytes: 18135253 })
})

test('copy destinations reject obvious escapes and accept the root and relative folders', () => {
  for (const path of ['', 'data/files', '.hidden']) assert.equal(isRelativeInstallPath(path), true)
  for (const path of ['/root', 'a/../b', 'C:/game', 'a//b', 'a\\b']) assert.equal(isRelativeInstallPath(path), false)
})
