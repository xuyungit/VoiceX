import { createServer } from 'vite'

const server = await createServer({
  configFile: false,
  server: { middlewareMode: true, watch: null, hmr: false, ws: false },
  optimizeDeps: { noDiscovery: true, include: [] },
  appType: 'custom'
})

try {
  const { checkTextDiff } = await server.ssrLoadModule('/src/utils/textDiff.spec.ts')
  checkTextDiff()
  console.log('Text diff segmentation, alignment, and condensing checks passed.')
} finally {
  await server.close()
}
