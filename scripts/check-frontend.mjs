import { readdirSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'

const root = fileURLToPath(new URL('../', import.meta.url))
const localeSpec = 'src/i18n/locales/messages.spec.ts'

function collectSpecs(dir) {
  return readdirSync(path.join(root, dir), { withFileTypes: true }).flatMap(entry => {
    const file = dir + '/' + entry.name
    if (entry.isDirectory()) return collectSpecs(file)
    return entry.isFile() && entry.name.endsWith('.spec.ts') ? [file] : []
  })
}

const args = process.argv.slice(2)
const requested = args.filter(arg => arg !== '--skip-i18n')
const allSpecs = collectSpecs('src').sort()
for (const file of requested) {
  if (!allSpecs.includes(file)) throw new Error(`Unknown frontend spec: ${file}`)
}
const specs = allSpecs.filter(file =>
  (requested.length === 0 || requested.includes(file)) &&
  (file !== localeSpec || !args.includes('--skip-i18n'))
)
if (specs.length === 0) throw new Error('No frontend specs selected')

// Invalid i18n messages must throw. Provide a consistent WebView platform in Node.
process.env.NODE_ENV = 'production'
Object.defineProperty(globalThis, 'navigator', {
  configurable: true,
  value: { platform: 'MacIntel', userAgent: 'MacIntel' }
})
const server = await createServer({
  root,
  configFile: false,
  server: { middlewareMode: true, watch: null, hmr: false, ws: false },
  optimizeDeps: { noDiscovery: true, include: [] },
  appType: 'custom'
})
let passed = 0
try {
  for (const file of specs) {
    const checks = Object.entries(await server.ssrLoadModule('/' + file))
    if (checks.length === 0) throw new Error(`No check functions exported by ${file}`)
    for (const [name, run] of checks) {
      if (typeof run !== 'function') throw new Error(`${file}: ${name} must be a check function`)
      try {
        await run(server)
      } catch (error) {
        throw new Error(`${file}: ${name} failed`, { cause: error })
      }
      passed++
      console.log(`Passed ${file} › ${name}`)
    }
  }
} finally {
  await server.close()
}
console.log(`Passed ${passed} checks across ${specs.length} frontend specs.`)
