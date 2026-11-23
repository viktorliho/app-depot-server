import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { runInNewContext } from 'node:vm'
import { renderToStaticMarkup } from 'react-dom/server'
import ts from 'typescript'

const source = await readFile(new URL('../src/installation-stages.tsx', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, module: ts.ModuleKind.CommonJS } })
const exports = {}
runInNewContext(outputText, { require: createRequire(import.meta.url), exports })
const { InstallationStages } = exports

test('installation stage buttons remain available in either direction and expose the current stage', () => {
  const stages = [{ title: 'Payload', summary: '1 copy operation' }, { title: 'Launch', summary: 'No launch selected' }]
  let selected = 'Launch'
  const props = () => ({ stages, active: selected, onSelect: title => { selected = title } })
  const navigation = InstallationStages(props())
  assert.equal(navigation.props['aria-label'], 'Installation editor sections')
  const [payload, launch] = navigation.props.children
  assert.equal(launch.props['aria-pressed'], true)
  assert.equal(payload.props.disabled, undefined)
  payload.props.onClick()
  assert.equal(selected, 'Payload')
  const html = renderToStaticMarkup(InstallationStages(props()))
  assert.match(html, /aria-pressed="true"><strong>Payload/)
  assert.match(html, /aria-pressed="false"><strong>Launch/)
})
