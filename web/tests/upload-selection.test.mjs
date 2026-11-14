import test from 'node:test'
import assert from 'node:assert/strict'
import { isOsMetadataPath, selectTreeFiles } from '../src/upload-selection.ts'

function file(path) { return { name: path.split('/').at(-1), webkitRelativePath: path } }

test('nested macOS and Windows metadata is excluded without filtering ordinary hidden content', () => {
  const metadata = [
    'game/.DS_Store', 'game/nested/._scene.cfg', 'game/.Spotlight-V100/index',
    'game/.Trashes/501/old', 'game/.fseventsd/events', 'game/nested/Thumbs.db',
    'game/nested/EHTHUMBS.DB', 'game/nested/Desktop.ini',
  ]
  const content = [
    'game/.hidden-game', 'game/.config/settings', 'game/nested/start.exe',
    'game/nested/Thumbs.db.backup', 'game/nested/game.cfg',
  ]
  const originals = [...metadata, ...content].map(file)
  const selected = selectTreeFiles(originals, true)
  assert.equal(selected.excludedCount, metadata.length)
  assert.deepEqual(selected.files.map(item => item.path), content.sort())
  assert.ok(selected.files.every(item => originals.includes(item.file)))
  for (const path of metadata) assert.equal(isOsMetadataPath(path), true, path)
  for (const path of content) assert.equal(isOsMetadataPath(path), false, path)
})

test('disabling the policy keeps every source file and does not change their names', () => {
  const paths = ['game/.DS_Store', 'game/nested/._file', 'game/.hidden-game', 'game/start.exe']
  const originals = paths.map(file)
  const selected = selectTreeFiles(originals, false)
  assert.equal(selected.excludedCount, 0)
  assert.deepEqual(selected.files.map(item => item.path), paths.sort())
  assert.ok(selected.files.every(item => originals.includes(item.file)))
})
