import { createServer } from 'vite'

const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, 'navigator')
try {
  for (const [platform, expectedMacOS] of [['MacIntel', true], ['Win32', false]]) {
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: { platform, userAgent: platform }
    })
    const server = await createServer({
      configFile: false,
      server: { middlewareMode: true, watch: null, hmr: false, ws: false },
      optimizeDeps: { noDiscovery: true, include: [] },
      appType: 'custom'
    })
    try {
      const { checkLlmDefault } = await server.ssrLoadModule('/src/stores/settings.spec.ts')
      checkLlmDefault(expectedMacOS)
      const { checkLlmModelCache, checkLlmModelCachePersistence } = await server.ssrLoadModule('/src/stores/llmModels.spec.ts')
      checkLlmModelCache()
      await checkLlmModelCachePersistence()
      const { checkAsrModelOptions } = await server.ssrLoadModule('/src/utils/asrModels.spec.ts')
      const { checkSonioxEndpointDelay } = await server.ssrLoadModule('/src/utils/sonioxSettings.spec.ts')
      const asrOptionCount = checkAsrModelOptions()
      const sonioxCount = checkSonioxEndpointDelay()
      console.log(
        `Model defaults and list cache, ${asrOptionCount} ASR option assertions, and ${sonioxCount} Soniox delay assertions passed (${platform} fixture).`
      )
    } finally {
      await server.close()
    }
  }
} finally {
  if (originalNavigator) Object.defineProperty(globalThis, 'navigator', originalNavigator)
  else delete globalThis.navigator
}
