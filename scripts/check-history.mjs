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
  const { checkHistoryDeferredCounts } = await server.ssrLoadModule('/src/stores/history.spec.ts')
  await checkHistoryDeferredCounts()
  const { checkHistorySearchPreview } = await server.ssrLoadModule('/src/utils/historySearch.spec.ts')
  checkHistorySearchPreview()
  const { checkHistoryPlayback } = await server.ssrLoadModule('/src/utils/historyPlayback.spec.ts')
  await checkHistoryPlayback()
  console.log('History lifecycle, search races, pagination, match preview, and playback checks passed.')
} finally {
  await server.close()
}
