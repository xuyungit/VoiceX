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

// Error codes the backend returns for its own refusals; anything else is the
// updater's message and is shown as is.
export const UPDATE_ERROR_CODES = ['disabled', 'inProgress', 'busy', 'noUpdate'] as const

export const useUpdatesStore = defineStore('updates', () => {
  const status = ref<UpdateStatus | null>(null)
  let unlisten: UnlistenFn | null = null

  const working = computed(() => {
    const phase = status.value?.phase
    return phase === 'checking' || phase === 'downloading' || phase === 'installing'
  })

  const hasUpdate = computed(() => Boolean(status.value?.available))

  async function init() {
    if (!unlisten) {
      unlisten = await listen<UpdateStatus>('update:status', (event) => {
        status.value = event.payload
      })
    }
    status.value = await invoke<UpdateStatus>('get_update_status')
  }

  // Both reject with an error code or message; the status event carries the
  // resulting state, so callers only need to show the rejection.
  async function check() {
    status.value = await invoke<UpdateStatus>('check_for_update')
  }

  // 'refreshed' means a newer version replaced the clicked one; nothing was
  // downloaded and the user reviews its notes before clicking again.
  async function install(expectedVersion: string) {
    return await invoke<InstallOutcome>('install_update', { expectedVersion })
  }

  return { status, working, hasUpdate, init, check, install }
})
