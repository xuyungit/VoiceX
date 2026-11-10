import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

export type UpdatePhase =
  | 'idle'
  | 'checking'
  | 'upToDate'
  | 'available'
  | 'downloading'
  | 'installing'
  | 'error'

export interface AvailableUpdate {
  version: string
  notes: string | null
  publishedAt: number | null
}

export interface UpdateStatus {
  enabled: boolean
  phase: UpdatePhase
  available: AvailableUpdate | null
  downloaded: number
  total: number | null
  error: string | null
  lastCheckedAt: number | null
}

export type InstallOutcome = 'installed' | 'refreshed' | 'upToDate'
const ABOUT_REFRESH_INTERVAL_SECONDS = 60

// Error codes the backend returns for its own refusals; anything else is the
// updater's message and is shown as is.
export const UPDATE_ERROR_CODES = ['disabled', 'inProgress', 'busy', 'noUpdate'] as const

export const useUpdatesStore = defineStore('updates', () => {
  const status = ref<UpdateStatus | null>(null)
  let unlisten: UnlistenFn | null = null
  let initializing: Promise<void> | null = null
  let checking: Promise<void> | null = null

  const working = computed(() => {
    const phase = status.value?.phase
    return phase === 'checking' || phase === 'downloading' || phase === 'installing'
  })

  const hasUpdate = computed(() => Boolean(status.value?.available))

  function init(): Promise<void> {
    if (!initializing) {
      initializing = (async () => {
        if (!unlisten) {
          unlisten = await listen<UpdateStatus>('update:status', (event) => {
            status.value = event.payload
          })
        }
        status.value = await invoke<UpdateStatus>('get_update_status')
      })().catch(error => {
        initializing = null
        throw error
      })
    }
    return initializing
  }

  // Both reject with an error code or message; the status event carries the
  // resulting state, so callers only need to show the rejection.
  function check(): Promise<void> {
    if (!checking) {
      checking = (async () => {
        status.value = await invoke<UpdateStatus>('check_for_update')
      })().finally(() => { checking = null })
    }
    return checking
  }

  // About can mount before App's initialization completes. Share that work,
  // then avoid duplicate requests on rapid navigation or during an install.
  async function refresh() {
    await init()
    const current = status.value
    if (!current?.enabled || working.value) return
    if (current.lastCheckedAt !== null && !current.error &&
        Date.now() / 1000 - current.lastCheckedAt < ABOUT_REFRESH_INTERVAL_SECONDS) return
    await check()
  }

  async function install(expectedVersion: string) {
    return await invoke<InstallOutcome>('install_update', { expectedVersion })
  }

  return { status, working, hasUpdate, init, check, refresh, install }
})
