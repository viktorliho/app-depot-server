import test from 'node:test'
import assert from 'node:assert/strict'
import { parseExactJson } from '../src/declaration-json.ts'

test('declaration edits preserve large integers and unknown nested values', () => {
  const source = '{"other":{"max":18446744073709551615,"negative":-9007199254740993,"decimal":0.123456789012345678901},"appDepot":{"entryPoints":[{"id":"game","target":{"type":"ArtifactFile"}}]}}'
  const data = parseExactJson(source)
  data.appDepot.entryPoints[0].launch = { method: 'NativePortableV1' }
  const saved = JSON.stringify({ data })
  assert.match(saved, /"max":18446744073709551615/)
  assert.match(saved, /"negative":-9007199254740993/)
  assert.match(saved, /"decimal":0\.123456789012345678901/)
  assert.match(saved, /"method":"NativePortableV1"/)
})

test('non-object declaration values remain valid', () => {
  assert.equal(parseExactJson('null'), null)
  assert.deepEqual(parseExactJson('[true,"data"]'), [true, 'data'])
})
