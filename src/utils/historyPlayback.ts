import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'

/** One playback owner, including the time spent loading its recording. */
export function useHistoryPlayback(onError: (error: unknown) => void) {
  const playingId = ref<string | null>(null)
  let generation = 0
  let player: HTMLAudioElement | null = null
  let objectUrl: string | null = null

  function stopPlayback() {
    // Also invalidates pending IPC/play promises and already queued events.
    generation++
    playingId.value = null
    if (player) {
      player.onended = null
      player.pause()
      player = null
    }
    if (objectUrl) {
      URL.revokeObjectURL(objectUrl)
      objectUrl = null
    }
  }

  async function togglePlayback(recordId: string) {
    if (playingId.value === recordId) {
      stopPlayback()
      return
    }

    stopPlayback()
    const request = generation
    // A second click can cancel this recording even before its bytes arrive.
    playingId.value = recordId
    try {
      // Tauri returns raw bytes over the custom protocol, or a number array
      // when IPC falls back to postMessage.
      const raw = await invoke<ArrayBuffer | number[]>('load_history_audio', { recordId })
      if (request !== generation) return
      const bytes = raw instanceof ArrayBuffer ? raw : new Uint8Array(raw)
      objectUrl = URL.createObjectURL(new Blob([bytes], { type: 'audio/ogg' }))
      const audio = new Audio(objectUrl)
      player = audio
      audio.onended = () => {
        if (request === generation) stopPlayback()
      }
      await audio.play()
    } catch (error) {
      // Stopping or replacing audio can reject its pending play promise.
      // Only the current request owns the UI and the error message.
      if (request !== generation) return
      stopPlayback()
      onError(error)
    }
  }

  return { playingId, togglePlayback, stopPlayback }
}
