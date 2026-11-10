import { createPinia, disposePinia } from 'pinia'
import { emit } from '@tauri-apps/api/event'
import { mockIPC, clearMocks } from '@tauri-apps/api/mocks'
import { useUpdatesStore, type UpdateStatus } from './updates'

function check(condition: boolean, message: string) {
  if (!condition) throw new Error(message)
}

export async function checkUpdateRefresh() {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  const originalNow = Date.now
  const pinia = createPinia()
  let now = 1000
  let reads = 0
  let checks = 0
  const installs: string[] = []
  let backend: UpdateStatus = {
    enabled: true, phase: 'available',
    available: { version: '0.2.0', notes: 'Old notes', publishedAt: null },
    downloaded: 0, total: null, error: null, lastCheckedAt: 1
  }
  let releaseCheck: (() => void) | undefined
  let checkFailure: string | null = null
  Object.defineProperty(globalThis, 'window', { configurable: true, value: { crypto: globalThis.crypto } })
  Date.now = () => now * 1000
  mockIPC(async (command, args) => {
    if (command === 'get_update_status') {
      reads++
      return backend
    }
    if (command === 'check_for_update') {
      checks++
      await emit('update:status', { ...backend, phase: 'checking' })
      if (checkFailure) {
        backend = { ...backend, phase: 'available', error: checkFailure }
        await emit('update:status', backend)
        throw checkFailure
      }
      await new Promise<void>(resolve => { releaseCheck = resolve })
      backend = { ...backend, phase: 'available', error: null, lastCheckedAt: now,
        available: { version: '0.5.0', notes: 'Latest notes', publishedAt: now } }
      await emit('update:status', backend)
      return backend
    }
    if (command === 'install_update') {
      if (!args || Array.isArray(args) || args instanceof ArrayBuffer || args instanceof Uint8Array) {
        throw new Error('Install must send its expected version')
      }
      installs.push(String(args.expectedVersion))
      return 'refreshed'
    }
    throw new Error(`Unexpected command: ${command}`)
  }, { shouldMockEvents: true })
  const flush = () => new Promise<void>(resolve => setTimeout(resolve, 0))
  try {
    const store = useUpdatesStore(pinia)
    const init = store.init()
    const opening = store.refresh()
    await init
    await flush()
    check(reads === 1 && checks === 1 && store.working, 'About and App must share initialization and refresh an old offer')
    await store.refresh()
    check(checks === 1, 'About must not duplicate a check already in progress')
    releaseCheck!()
    await opening
    check(store.status?.available?.version === '0.5.0' && store.status.available.notes === 'Latest notes', 'A refresh must replace the version and notes together')
    await store.refresh()
    now += 59
    await store.refresh()
    check(checks === 1, 'Rapid navigation must reuse the recent successful check')
    now += 1
    const later = store.refresh()
    await flush()
    check(checks === 2, 'Reopening About after the throttle must check again')
    releaseCheck!()
    await later

    const manual = store.check()
    const duplicate = store.check()
    await flush()
    check(checks === 3, 'Manual checks bypass the About throttle and coalesce concurrent requests')
    releaseCheck!()
    await Promise.all([manual, duplicate])

    for (const phase of ['downloading', 'installing'] as const) {
      backend = { ...backend, phase }
      await emit('update:status', backend)
      now += 61
      await store.refresh()
    }
    backend = { ...backend, enabled: false, phase: 'idle' }
    await emit('update:status', backend)
    await store.refresh()
    check(checks === 3, 'About must skip disabled builds and active downloads or installs')

    backend = { ...backend, enabled: true, phase: 'available' }
    await emit('update:status', backend)
    checkFailure = 'Manifest unavailable'
    const error = await store.refresh().then(() => null, error => error)
    check(error === checkFailure && store.status?.error === checkFailure, 'Failed automatic refreshes must remain visible and reject')
    checkFailure = null
    const retry = store.refresh()
    await flush()
    releaseCheck!()
    await retry
    check(checks === 5 && store.status?.error === null, 'Failed checks must allow another refresh')

    const result = await store.install('0.2.0')
    check(installs.length === 1 && installs[0] === '0.2.0' && result === 'refreshed', 'Install must send the clicked version and return without auto-confirming a replacement')
  } finally {
    clearMocks()
    disposePinia(pinia)
    Date.now = originalNow
    if (originalWindow) Object.defineProperty(globalThis, 'window', originalWindow)
    else Reflect.deleteProperty(globalThis, 'window')
  }
}
