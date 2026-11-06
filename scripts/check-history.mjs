import { createServer } from 'vite'

const server = await createServer({
  configFile: false,
  server: { middlewareMode: true, watch: null, hmr: false, ws: false },
  optimizeDeps: { noDiscovery: true, include: [] },
  appType: 'custom'
})

try {
  const { checkHistoryLifecycle } = await server.ssrLoadModule('/src/stores/history.spec.ts')
  await checkHistoryLifecycle()
  console.log('History refresh, latest-text copying, and operation failure checks passed.')
} finally {
  await server.close()
}
