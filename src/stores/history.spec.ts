import { createPinia, disposePinia } from 'pinia'
import { finalText, useHistoryStore, type AppUsageStat, type HistoryRecord } from './history'

function check(condition: boolean, message: string) {
  if (!condition) throw new Error(message)
}

// Let the registration and result IPC promises settle without resolving any
// deliberately held page/count request.
const flushIpc = () => new Promise<void>(resolve => setTimeout(resolve, 0))

export async function checkAppStatsRequests() {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  const pinia = createPinia()
  const requests: { resolve: (rows: AppUsageStat[]) => void }[] = []
  Object.defineProperty(globalThis, 'window', { configurable: true, value: {
    __TAURI_INTERNALS__: { invoke(command: string) {
      check(command === 'get_app_usage_stats', 'Only app stats should be requested')
      return new Promise<AppUsageStat[]>(resolve => requests.push({ resolve }))
    } }
  } })
  const snapshot = (count: number): AppUsageStat[] => [{
    platform: 'macos', appKey: 'test.app', appName: 'Test', usageCount: count,
    totalCharacters: count * 10, totalDurationMs: count * 1000, llmCount: 0,
    lastUsedAt: '2026-11-16T00:00:00Z',
  }]
  try {
    const store = useHistoryStore(pinia)
    const week = store.loadAppStats('week')
    const today = store.loadAppStats('today')
    requests[1].resolve(snapshot(2))
    await today
    requests[0].resolve(snapshot(10))
    await week
    check(store.appUsageRange === 'today' && store.appStats[0].usageCount === 2, 'An old range response must not replace the selected range')

    const oldToday = store.loadAppStats('today')
    const newToday = store.loadAppStats('today')
    requests[3].resolve(snapshot(3))
    await newToday
    requests[2].resolve(snapshot(2))
    await oldToday
    check(store.appStats[0].usageCount === 3, 'An old refresh of the same range must also be discarded')

    const oldTop = store.loadWeeklyTopApps()
    const currentWeek = store.loadAppStats('week')
    const apps = [10, 6, 3, 1].map((count, i) => ({ ...snapshot(count)[0], appKey: `app-${i}` }))
    requests[5].resolve(apps)
    await currentWeek
    requests[4].resolve(snapshot(5))
    await oldTop
    check(store.weeklyTopApps.length === 3 && store.weeklyTotalCount === 20, 'An old teaser response must not replace the current weekly snapshot or its full denominator')

    const oldWeek = store.loadAppStats('week')
    const newTop = store.loadWeeklyTopApps()
    requests[7].resolve(snapshot(30))
    await newTop
    requests[6].resolve(snapshot(20))
    await oldWeek
    check(store.weeklyTotalCount === 30, 'An old app list response must not overwrite a newer weekly teaser snapshot')
  } finally {
    disposePinia(pinia)
    if (originalWindow) Object.defineProperty(globalThis, 'window', originalWindow)
    else Reflect.deleteProperty(globalThis, 'window')
  }
}

export async function checkHistoryDeferredCounts() {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  const pinia = createPinia()
  let searchId = 0
  let backend = Array.from({ length: 25 }, (_, i) => record(i))
  const counts: { resolve: (count: number) => void; reject: (error: Error) => void }[] = []
  Object.defineProperty(globalThis, 'window', { configurable: true, value: {
    __TAURI_INTERNALS__: { invoke(command: string, args: { searchId: number; offset: number; limit: number; query: string; id: string }) {
      if (command === 'begin_history_search') return Promise.resolve(++searchId)
      if (command === 'delete_history_record') {
        backend = backend.filter(row => row.id !== args.id)
        return Promise.resolve()
      }
      if (command === 'get_usage_stats' || command === 'get_local_usage_stats') return Promise.resolve({})
      check(args.searchId === searchId, 'Page/count must use the registered search')
      if (command === 'get_history') {
        return Promise.resolve(args.query === '无结果' ? [] : backend.slice(args.offset, args.offset + args.limit))
      }
      if (command === 'count_history') {
        return new Promise<number>((resolve, reject) => counts.push({ resolve, reject }))
      }
      throw new Error(`Unexpected command: ${command}`)
    } }
  } })
  try {
    const store = useHistoryStore(pinia)
    await store.setSearch('反力', 'all')
    check(store.records.length === 20 && !store.isLoading && !store.searching, 'A slow count must not block the first page')
    check(store.total === null, 'A pending count must not invent a total')
    await store.loadHistory()
    check(store.records.length === 25 && counts.length === 1, 'Pagination must work while counting, without starting another count')

    await store.setSearch('标定', 'all')
    counts[0].resolve(999)
    await flushIpc()
    check(store.total === null, 'An old search count must not replace the current total')
    counts[1].resolve(25)
    await flushIpc()
    check(store.total === 25, 'The current count should update after the list appears')

    await store.loadHistory(true)
    await store.loadHistory(true)
    counts[2].resolve(100)
    await flushIpc()
    check(store.total === null, 'A count from before a history refresh must also be discarded')
    counts[3].reject(new Error('Count failed'))
    await flushIpc()
    check(store.records.length === 20 && store.loadError === null && !store.isLoading, 'Count failures must not discard a successful page')
    check(store.total === null, 'Failed counts leave the loaded-row display available')

    await store.setSearch('删除测试', 'all')
    await store.deleteRecord('0')
    counts[4].resolve(25)
    await flushIpc()
    check(store.total === null, 'A count from before deletion must not restore the old total')
    counts[5].resolve(24)
    await flushIpc()
    check(store.total === 24 && !store.records.some(row => row.id === '0'), 'Deletion refreshes the filtered list and its count')
    await store.setSearch('无结果', 'all')
    check(store.total === 0 && counts.length === 6, 'An exhausted first page must not start another full scan')
  } finally {
    disposePinia(pinia)
    if (originalWindow) Object.defineProperty(globalThis, 'window', originalWindow)
    else Reflect.deleteProperty(globalThis, 'window')
  }
}

async function rejects(action: Promise<unknown>, expected: Error) {
  const error = await action.then(() => null, error => error)
  check(error === expected, 'Operation failures must reach the caller')
}

const record = (index: number): HistoryRecord => ({
  id: String(index), timestamp: '2026-11-06T07:00:00Z', text: '支座返利',
  originalText: null, editedText: null, aiCorrectionApplied: false, llmInvoked: false,
  mode: 'assistant_raw', durationMs: 1000, audioPath: null, isFinal: true,
  errorCode: 0, errorMessage: null, sourceDeviceId: null, sourceDeviceName: null,
  asrModelName: null, llmModelName: null,
  targetAppPlatform: null, targetAppKey: null, targetAppName: null
})

export async function checkHistoryLifecycle() {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, 'navigator')
  const pinia = createPinia()
  const backend = Array.from({ length: 45 }, (_, index) => record(index))
  const reads: { limit: number; offset: number }[] = []
  let gate: Promise<void> | undefined
  let clipboard = ''
  let copyError: Error | undefined
  let deleteError: Error | undefined
  let searchId = 0

  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { __TAURI_INTERNALS__: {
      async invoke(command: string, args: { limit: number; offset: number; id: string }) {
        if (command === 'begin_history_search') return ++searchId
        if (command === 'get_history') {
          reads.push({ limit: args.limit, offset: args.offset })
          const snapshot = backend.slice(args.offset, args.offset + args.limit).map(row => ({ ...row }))
          const pending = gate
          gate = undefined
          if (pending) await pending
          return snapshot
        }
        if (command === 'delete_history_record') {
          if (deleteError) throw deleteError
          backend.splice(backend.findIndex(row => row.id === args.id), 1)
          return
        }
        if (command === 'get_usage_stats' || command === 'get_local_usage_stats') {
          return { totalDurationMs: 0, totalCharacters: 0 }
        }
        throw new Error(`Unexpected command: ${command}`)
      }
    } }
  })
  Object.defineProperty(globalThis, 'navigator', {
    configurable: true,
    value: { clipboard: { async writeText(text: string) {
      if (copyError) throw copyError
      clipboard = text
    } } }
  })

  try {
    const store = useHistoryStore(pinia)
    await store.loadHistory(true)
    await store.loadHistory()
    const openDetails = store.records[25]
    backend[25].editedText = '支座反力'
    await store.loadHistory(true)
    check(reads[2].limit === 40 && reads[2].offset === 0, 'Refresh must preserve loaded depth')
    check(store.records.length === 40, 'Refresh must keep the loaded records')
    check(store.records[25] === openDetails, 'Open details must keep their record reference')
    check(finalText(openDetails) === '支座反力', 'Open details must show the observed edit')
    await store.copyText(finalText(openDetails))
    check(clipboard === '支座反力', 'Copy must use the edited text')

    // An observer update arriving during a read must trigger a fresh read.
    let resume!: () => void
    gate = new Promise<void>(resolve => { resume = resolve })
    const pending = store.loadHistory(true)
    backend[25].editedText = '最终支座反力'
    await store.loadHistory(true)
    resume()
    await pending
    check(finalText(openDetails) === '最终支座反力', 'Updates arriving during a read must not be lost')
    check(store.records.length === 40, 'A queued refresh must preserve loaded depth')
    await store.loadHistory()
    check(store.records.length === 45 && !store.hasMore, 'Pagination must continue from the refreshed depth')

    copyError = new Error('Clipboard unavailable')
    await rejects(store.copyText(finalText(openDetails)), copyError)
    check(clipboard === '支座反力', 'A failed copy must leave the clipboard unchanged')
    deleteError = new Error('Database locked')
    await rejects(store.deleteRecord(openDetails.id), deleteError)
    check(store.records.includes(openDetails), 'A failed deletion must keep the record')
    deleteError = undefined
    await store.deleteRecord(openDetails.id)
    check(!store.records.includes(openDetails), 'A successful deletion must remove the record')
  } finally {
    disposePinia(pinia)
    if (originalWindow) Object.defineProperty(globalThis, 'window', originalWindow)
    else Reflect.deleteProperty(globalThis, 'window')
    if (originalNavigator) Object.defineProperty(globalThis, 'navigator', originalNavigator)
    else Reflect.deleteProperty(globalThis, 'navigator')
  }
}

export async function checkHistorySearchRequests() {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  const pinia = createPinia()
  let searchId = 0
  const reads: {
    args: { query: string; recordType: string; targetAppKey?: string; limit: number; offset: number }
    resolve: (rows: HistoryRecord[]) => void
    reject: (error: Error) => void
  }[] = []
  Object.defineProperty(globalThis, 'window', { configurable: true, value: {
    __TAURI_INTERNALS__: {
      invoke(command: string, args: typeof reads[number]['args']) {
        if (command === 'begin_history_search') return Promise.resolve(++searchId)
        if (command === 'count_history') return Promise.resolve(0)
        if (command !== 'get_history') throw new Error(`Unexpected command: ${command}`)
        return new Promise<HistoryRecord[]>((resolve, reject) => reads.push({ args, resolve, reject }))
      }
    }
  } })
  try {
    const store = useHistoryStore(pinia)
    const old = store.loadHistory(true)
    await flushIpc()
    const current = store.setSearch('  反力\t标定  ', 'dictation', ' com.todesktop.cursor ')
    await flushIpc()
    check(
      reads[1].args.query === '反力 标定' &&
      reads[1].args.recordType === 'dictation' &&
      reads[1].args.targetAppKey === 'com.todesktop.cursor',
      'Send normalized query, type, and targetAppKey to the backend',
    )
    reads[0].resolve([record(99)])
    await old
    check(store.isLoading && store.records.length === 0, 'An old response must not replace results or clear the current loading state')
    reads[1].resolve(Array.from({ length: 20 }, (_, i) => record(i)))
    await current
    const more = store.loadHistory()
    check(
      reads[2].args.offset === 20 &&
      reads[2].args.query === '反力 标定' &&
      reads[2].args.targetAppKey === 'com.todesktop.cursor',
      'Pagination must retain search conditions including targetAppKey',
    )
    const changed = store.setSearch('', 'failed', null)
    await flushIpc()
    check(reads[3].args.targetAppKey === undefined, 'Clearing targetAppKey omits it from backend request')
    reads[3].resolve([record(42)])
    await changed
    reads[2].reject(new Error('Stale page failure'))
    await more
    check(store.records.length === 1 && store.records[0].id === '42' && store.loadError === null, 'Late pagination errors must not affect another filter')
    const clear = store.setSearch('', 'all')
    await flushIpc()
    check(reads[4].args.offset === 0 && reads[4].args.recordType === 'all', 'Clearing filters starts ordinary history from the first page')
    reads[4].reject(new Error('Read failed'))
    await clear
    check(store.loadError === 'Error: Read failed' && !store.isLoading, 'First-page errors remain visible and retryable')
    const retry = store.loadHistory(true)
    await flushIpc()
    reads[5].resolve([record(1)])
    await retry
    check(store.loadError === null && store.records[0].id === '1', 'A successful retry clears the error')
    const stale = store.setSearch('旧查询', 'all')
    await flushIpc()
    const latest = store.setSearch('新查询', 'all')
    await flushIpc()
    reads[7].resolve([record(7)])
    await latest
    reads[6].resolve([record(6)])
    await stale
    check(store.records[0].id === '7', 'A late success must not overwrite the latest search')
  } finally {
    disposePinia(pinia)
    if (originalWindow) Object.defineProperty(globalThis, 'window', originalWindow)
    else Reflect.deleteProperty(globalThis, 'window')
  }
}

export async function checkHistorySearchContinuity() {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  const pinia = createPinia()
  let searchId = 0
  const reads: {
    args: { limit: number; offset: number }
    resolve: (rows: HistoryRecord[]) => void
    reject: (error: Error) => void
  }[] = []
  Object.defineProperty(globalThis, 'window', { configurable: true, value: {
    __TAURI_INTERNALS__: {
      invoke(command: string, args: { limit: number; offset: number; text: string }) {
        if (command === 'begin_history_search') return Promise.resolve(++searchId)
        if (command === 'set_history_final_text') return Promise.resolve({ editedText: args.text })
        if (command === 'count_history') return Promise.resolve(counted)
        if (command !== 'get_history') throw new Error(`Unexpected command: ${command}`)
        return new Promise<HistoryRecord[]>((resolve, reject) => reads.push({ args, resolve, reject }))
      }
    }
  } })
  let counted = 3
  const page = (ids: number[]) => ids.map(record)
  const ids = (rows: HistoryRecord[]) => rows.map(row => row.id).join()
  try {
    const store = useHistoryStore(pinia)
    const first = store.setSearch('反力', 'all')
    await flushIpc()
    reads[0].resolve(page([0, 1, 2]))
    await first
    await flushIpc()
    check(store.total === 3, 'A search reports how many records match in all')
    counted = 35
    const next = store.setSearch('反力 标定', 'all')
    await flushIpc()
    check(ids(store.records) === '0,1,2' && store.searching && store.total === 3, 'Previous results and count stay visible while a new search runs')
    check(reads[1].args.limit === 20, 'A new search starts at one page, not the loaded depth')
    const rows = Array.from({ length: 20 }, (_, i) => 19 - i)
    reads[1].resolve(page(rows))
    await next
    await flushIpc()
    check(!store.searching && store.records.length === 20 && store.total === 35, 'New results and count replace the previous ones when they arrive')

    await store.setFinalText('3', '不再命中')
    // The save's history:updated refresh re-runs the search without row 3.
    const refresh = store.loadHistory(true)
    await flushIpc()
    reads[2].resolve(page([20, ...rows.filter(id => id !== 3)]))
    await refresh
    check(store.records.some(row => row.id === '3' && row.editedText === '不再命中'), 'An edited result stays listed after the refresh')
    const more = store.loadHistory()
    check(reads[3].args.offset === 20, 'Rows kept only for being edited do not shift the next page')
    reads[3].resolve([])
    await more

    const failing = store.setSearch('新条件', 'all')
    await flushIpc()
    check(store.records.length === 21, 'Results stay until the new search answers')
    reads[4].reject(new Error('Read failed'))
    await failing
    check(store.records.length === 0 && store.total === null && store.loadError === 'Error: Read failed', 'A failed new search does not show the previous results as its own')
  } finally {
    disposePinia(pinia)
    if (originalWindow) Object.defineProperty(globalThis, 'window', originalWindow)
    else Reflect.deleteProperty(globalThis, 'window')
  }
}
