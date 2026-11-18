import { createPinia, disposePinia } from 'pinia'
import {
  buildAppIconRequestKey,
  getAppFallbackLabel,
  useAppIconStore,
  type AppIconLookupRequest,
} from './appIcons'

function check(condition: boolean, message: string) {
  if (!condition) throw new Error(message)
}

export async function checkAppIconStoreBatchingAndCaching() {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  const pinia = createPinia()
  const batches: AppIconLookupRequest[][] = []

  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: {
      __TAURI_INTERNALS__: {
        invoke(command: string, args: { requests: AppIconLookupRequest[] }) {
          check(command === 'get_app_icons', `Unexpected command: ${command}`)
          batches.push(args.requests)
          const response: Record<string, string> = {}
          for (const req of args.requests) {
            if (req.appKey === 'com.google.antigravity') {
              response[req.requestKey] = 'data:image/png;base64,AAAA'
            }
          }
          return Promise.resolve(response)
        },
      },
    },
  })

  try {
    const store = useAppIconStore(pinia)
    const first = store.getIconUrl({
      platform: 'macos',
      appKey: 'com.google.antigravity',
      appName: 'Antigravity',
    })
    const duplicate = store.getIconUrl({
      platform: 'macOS',
      appKey: 'COM.GOOGLE.ANTIGRAVITY',
      appName: 'Antigravity',
    })
    const missing = store.getIconUrl({
      platform: 'windows',
      appKey: 'unknown.exe',
      appName: 'Unknown',
    })

    check(first === null && duplicate === null && missing === null, 'Initial synchronous lookup returns null while queued')
    await new Promise<void>(resolve => setTimeout(resolve, 0))

    check(batches.length === 1, 'Same-tick lookups must be coalesced into a single IPC batch')
    check(batches[0].length === 2, 'Duplicate normalized keys must be deduplicated in the batch')

    const resolvedUrl = store.getIconUrl({
      platform: 'macos',
      appKey: 'com.google.antigravity',
      appName: 'Antigravity',
    })
    check(resolvedUrl === 'data:image/png;base64,AAAA', 'Resolved icon data URL should be served from store cache')

    const resolvedMissing = store.getIconUrl({
      platform: 'windows',
      appKey: 'unknown.exe',
      appName: 'Unknown',
    })
    check(resolvedMissing === null, 'Missing icon should resolve to null')
    await new Promise<void>(resolve => setTimeout(resolve, 0))
    check(batches.length === 1, 'Cached hits and confirmed misses must not trigger extra IPC calls')

    store.invalidateMissing()
    store.getIconUrl({
      platform: 'windows',
      appKey: 'unknown.exe',
      appName: 'Unknown',
    })
    await new Promise<void>(resolve => setTimeout(resolve, 0))
    check(batches.length === 2, 'invalidateMissing should allow re-querying previously missing icons')

    check(getAppFallbackLabel('Antigravity') === 'AN', 'Fallback initials should take first two letters')
    check(
      buildAppIconRequestKey({ platform: 'macOS', appKey: 'com.apple.Finder' }) ===
        buildAppIconRequestKey({ platform: 'macos', appKey: 'com.apple.finder' }),
      'Request key normalization must be case-insensitive',
    )

    const host = { platform: 'Windows', appKey: 'ApplicationFrameHost.exe' }
    const editorId = 'Vendor.Editor_publisher!App'
    const photoId = 'Vendor.Photo_publisher!App'
    store.getIconUrl({ ...host, appUserModelId: editorId })
    store.getIconUrl({ ...host, appUserModelId: photoId })
    await new Promise<void>(resolve => setTimeout(resolve, 0))
    check(batches[2].length === 2, 'Applications that share a Windows host must request separate icons')
    check(
      batches[2][0].appUserModelId === editorId && batches[2][1].appUserModelId === photoId,
      'App icon lookup must carry the original Windows application identity',
    )
  } finally {
    disposePinia(pinia)
    if (originalWindow) Object.defineProperty(globalThis, 'window', originalWindow)
  }
}
