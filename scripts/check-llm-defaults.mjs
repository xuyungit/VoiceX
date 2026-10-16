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
      const { checkAsrModelOptions } = await server.ssrLoadModule('/src/utils/asrModels.spec.ts')
      console.log(`Model defaults and ${checkAsrModelOptions()} ASR option assertions passed (${platform} fixture).`)
    } finally {
      await server.close()
    }
  }
} finally {
  if (originalNavigator) Object.defineProperty(globalThis, 'navigator', originalNavigator)
  else delete globalThis.navigator
}
