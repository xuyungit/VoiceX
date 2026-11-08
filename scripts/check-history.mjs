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
  const { checkHistorySearchRequests } = await server.ssrLoadModule('/src/stores/history.spec.ts')
  await checkHistorySearchRequests()
  const { checkHistorySearchContinuity } = await server.ssrLoadModule('/src/stores/history.spec.ts')
  await checkHistorySearchContinuity()
  const { checkHistorySearchPreview } = await server.ssrLoadModule('/src/utils/historySearch.spec.ts')
  checkHistorySearchPreview()
  console.log('History lifecycle, search races, pagination, and match preview checks passed.')
} finally {
  await server.close()
}
