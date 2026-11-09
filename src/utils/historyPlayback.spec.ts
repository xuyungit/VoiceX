import { useHistoryPlayback } from './historyPlayback'

function check(condition: boolean, message: string) {
  if (!condition) throw new Error(message)
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

export async function checkHistoryPlayback() {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  const originalAudio = Object.getOwnPropertyDescriptor(globalThis, 'Audio')
  const originalCreate = URL.createObjectURL
  const originalRevoke = URL.revokeObjectURL
  const reads: ReturnType<typeof deferred<ArrayBuffer | number[]>>[] = []
  const players: FakeAudio[] = []
  const blobs: Blob[] = []
  const liveUrls = new Set<string>()
  const errors: unknown[] = []
  let nextPlay: {
    result: ReturnType<typeof deferred<void>>
    started: ReturnType<typeof deferred<void>>
  } | undefined

  class FakeAudio {
    onended: (() => void) | null = null
    active = false
    constructor(readonly src: string) { players.push(this) }
    play() {
      this.active = true
      const pending = nextPlay
      nextPlay = undefined
      pending?.started.resolve()
      return pending?.result.promise ?? Promise.resolve()
    }
    pause() { this.active = false }
  }

  Object.defineProperty(globalThis, 'window', { configurable: true, value: {
    __TAURI_INTERNALS__: { invoke(command: string) {
      check(command === 'load_history_audio', 'Playback must load by record ID')
      const read = deferred<ArrayBuffer | number[]>()
      reads.push(read)
      return read.promise
    } }
  } })
  Object.defineProperty(globalThis, 'Audio', { configurable: true, value: FakeAudio })
  URL.createObjectURL = blob => {
    blobs.push(blob as Blob)
    const url = `blob:${blobs.length}`
    liveUrls.add(url)
    return url
  }
  URL.revokeObjectURL = url => { liveUrls.delete(url) }
  const playback = useHistoryPlayback(error => errors.push(error))
  const toggle = (id: string) => {
    const done = playback.togglePlayback(id)
    return { done, read: reads[reads.length - 1] }
  }
  const activeCount = () => players.filter(player => player.active).length

  try {
    const first = toggle('A')
    const second = toggle('B')
    second.read.resolve([1, 2, 3])
    await second.done
    first.read.resolve(new ArrayBuffer(1))
    await first.done
    check(players.length === 1 && playback.playingId.value === 'B', 'Late loads must not replace the latest recording')
    check(blobs[0].size === 3, 'postMessage number arrays must remain playable bytes')
    playback.stopPlayback()
    check(activeCount() === 0 && liveUrls.size === 0, 'Stopping must pause audio and release its URL')

    const cancelled = toggle('A')
    await playback.togglePlayback('A')
    cancelled.read.resolve(new ArrayBuffer(1))
    await cancelled.done
    check(players.length === 1 && playback.playingId.value === null, 'Clicking a loading row again must cancel it')

    const leaving = toggle('A')
    // History calls this on unmount, including while a file is loading.
    playback.stopPlayback()
    leaving.read.resolve(new ArrayBuffer(1))
    await leaving.done
    check(players.length === 1 && activeCount() === 0, 'A load finishing after leaving must not create a player')

    const stale = toggle('A')
    const current = toggle('B')
    current.read.resolve(new ArrayBuffer(4))
    await current.done
    stale.read.reject(new Error('Old load failed'))
    await stale.done
    check(errors.length === 0 && playback.playingId.value === 'B', 'Stale load errors must not clear current playback or show a dialog')
    check(blobs[1].size === 4, 'Custom protocol ArrayBuffers must remain playable bytes')

    const pendingPlay = deferred<void>()
    const slowStarted = deferred<void>()
    nextPlay = { result: pendingPlay, started: slowStarted }
    const slow = toggle('C')
    slow.read.resolve([5])
    await slowStarted.promise
    const oldPlayer = players[players.length - 1]
    const queuedEnd = oldPlayer.onended!
    const replacement = toggle('D')
    replacement.read.resolve([6])
    await replacement.done
    queuedEnd()
    pendingPlay.reject(new Error('Playback interrupted'))
    await slow.done
    check(!oldPlayer.active && activeCount() === 1, 'Replacing audio must pause the previous player')
    check(errors.length === 0 && playback.playingId.value === 'D', 'Late play failures and ended events must not affect a replacement')
    players[players.length - 1].onended!()
    check(playback.playingId.value === null && liveUrls.size === 0, 'Natural completion must release the current playback')

    const failedPlay = deferred<void>()
    const failedStarted = deferred<void>()
    nextPlay = { result: failedPlay, started: failedStarted }
    const failed = toggle('E')
    failed.read.resolve([7])
    await failedStarted.promise
    const failure = new Error('Cannot decode recording')
    failedPlay.reject(failure)
    await failed.done
    check(errors[0] === failure && errors.length === 1, 'Current playback failures must remain visible')
    check(playback.playingId.value === null && activeCount() === 0 && liveUrls.size === 0, 'Playback failures must clean up audio and URLs')
  } finally {
    playback.stopPlayback()
    URL.createObjectURL = originalCreate
    URL.revokeObjectURL = originalRevoke
    if (originalWindow) Object.defineProperty(globalThis, 'window', originalWindow)
    else Reflect.deleteProperty(globalThis, 'window')
    if (originalAudio) Object.defineProperty(globalThis, 'Audio', originalAudio)
    else Reflect.deleteProperty(globalThis, 'Audio')
  }
}
