import { createServer } from 'vite'

// The production compiler throws on malformed messages; development only logs.
process.env.NODE_ENV = 'production'
const server = await createServer({
  configFile: false,
  server: { middlewareMode: true, watch: null, hmr: false },
  optimizeDeps: { noDiscovery: true, include: [] },
  appType: 'custom'
})
try {
  const { checkLocaleMessages } = await server.ssrLoadModule('/src/i18n/locales/messages.spec.ts')
  const count = checkLocaleMessages()
  console.log(`Checked ${count} locale messages and LLM literal examples (production mode).`)
} finally {
  await server.close()
}
