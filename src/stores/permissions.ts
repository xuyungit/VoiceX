import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'

export type PermissionKind = 'microphone' | 'accessibility' | 'inputMonitoring'
export type PermissionState = 'granted' | 'notDetermined' | 'denied' | 'notApplicable'

export interface PermissionsSnapshot {
  platform: string
  microphone: PermissionState
  accessibility: PermissionState
  inputMonitoring: PermissionState
  hotkeyListenerActive: boolean
}

export const PERMISSION_KINDS: PermissionKind[] = ['microphone', 'accessibility', 'inputMonitoring']

// The listener retries the keyboard hook every second after a grant lands, so
// a down listener only means "restart needed" once it has stayed down a while.
const LISTENER_GRACE_MS = 4000
const POLL_MS = 1500

export function isMissing(state: PermissionState) {
  return state === 'notDetermined' || state === 'denied'
}

export const usePermissionsStore = defineStore('permissions', () => {
  const snapshot = ref<PermissionsSnapshot | null>(null)
  const listenerDownSince = ref<number | null>(null)
  const now = ref(Date.now())

  const missing = computed<PermissionKind[]>(() => {
    const current = snapshot.value
    if (!current) return []
    return PERMISSION_KINDS.filter((kind) => isMissing(current[kind]))
  })

  const needsRestart = computed(() =>
    missing.value.length === 0 &&
    listenerDownSince.value !== null &&
    now.value - listenerDownSince.value >= LISTENER_GRACE_MS
  )

  const needsAttention = computed(() => missing.value.length > 0 || needsRestart.value)

  async function refresh() {
    const next = await invoke<PermissionsSnapshot>('permissions_status')
    snapshot.value = next
    now.value = Date.now()
    if (next.hotkeyListenerActive) {
      listenerDownSince.value = null
    } else if (listenerDownSince.value === null) {
      listenerDownSince.value = now.value
    }
  }

  // Microphone: the system prompt while it is still available, Settings after
  // a refusal. Accessibility / Input Monitoring: the drag helper (macOS only).
  async function grant(kind: PermissionKind) {
    const state = snapshot.value?.[kind]
    if (kind === 'microphone') {
      if (state === 'notDetermined' && snapshot.value?.platform === 'macos') {
        await invoke('permissions_request_microphone')
      } else {
        await invoke('permissions_open_settings', { kind })
      }
    } else {
      await invoke('permissions_start_guide', { kind })
    }
    await check()
  }

  async function restartApp() {
    await invoke('restart_app')
  }

  // Poll only while something is missing or the hotkey hook is down: that is
  // when a change made in System Settings has to show up here by itself.
  // Otherwise App refreshes on window focus and on `permissions:changed`.
  let timer: number | null = null

  function shouldPoll() {
    const current = snapshot.value
    return !current || missing.value.length > 0 || !current.hotkeyListenerActive
  }

  function ensurePolling() {
    if (timer !== null || !shouldPoll()) return
    timer = window.setTimeout(async () => {
      timer = null
      try {
        await refresh()
      } catch (err) {
        console.error('[permissions] status check failed:', err)
      }
      ensurePolling()
    }, POLL_MS)
  }

  async function check() {
    try {
      await refresh()
    } finally {
      ensurePolling()
    }
  }

  return { snapshot, missing, needsRestart, needsAttention, refresh, check, grant, restartApp }
})
