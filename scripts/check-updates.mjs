import { createServer } from 'vite'

const server = await createServer({
  configFile: false,
  server: { middlewareMode: true, watch: null, hmr: false, ws: false },
  optimizeDeps: { noDiscovery: true, include: [] },
  appType: 'custom'
})

try {
  const { checkUpdateRefresh } = await server.ssrLoadModule('/src/stores/updates.spec.ts')
  await checkUpdateRefresh()
  console.log('Update refresh, throttling, initialization, errors, and install confirmation checks passed.')
} finally {
  await server.close()
}
