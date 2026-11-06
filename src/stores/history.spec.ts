import { createPinia, disposePinia } from 'pinia'
import { finalText, useHistoryStore, type HistoryRecord } from './history'

function check(condition: boolean, message: string) {
  if (!condition) throw new Error(message)
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
  asrModelName: null, llmModelName: null
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

  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { __TAURI_INTERNALS__: {
      async invoke(command: string, args: { limit: number; offset: number; id: string }) {
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
