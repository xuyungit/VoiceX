import { defineStore, acceptHMRUpdate } from 'pinia'
import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'

export interface AppIconQuery {
  platform?: string | null
  appKey?: string | null
  bundleId?: string | null
  executablePath?: string | null
  appUserModelId?: string | null
  appName?: string | null
}

export interface AppIconLookupRequest {
  requestKey: string
  platform?: string | null
  appKey?: string | null
  bundleId?: string | null
  executablePath?: string | null
  appUserModelId?: string | null
  appName?: string | null
}

function normalizeToken(value?: string | null): string {
  return (value ?? '').trim().toLowerCase()
}

export function buildAppIconRequestKey(query: AppIconQuery): string {
  const platform = normalizeToken(query.platform)
  const appKey = normalizeToken(query.appKey)
  const bundleId = normalizeToken(query.bundleId)
  const executablePath = normalizeToken(query.executablePath)
  const appUserModelId = normalizeToken(query.appUserModelId)
  const appName = normalizeToken(query.appName)

  const primary = appUserModelId || appKey || bundleId || executablePath || appName
  if (!primary) return ''
  return `${platform}:${appKey}|${bundleId}|${executablePath}|${appUserModelId}|${primary === appName ? appName : ''}`
}

export function getAppFallbackLabel(appName: string, mode: 'initials' | 'single' = 'initials'): string {
  const trimmed = (appName ?? '').trim()
  if (!trimmed) return '?'
  if (mode === 'single') {
    const tokens = trimmed.split(/[\s._-]+/).filter(Boolean)
    if (tokens.length >= 2) {
      return `${tokens[0][0] ?? ''}${tokens[1][0] ?? ''}`.toUpperCase()
    }
    return trimmed.slice(0, 2).toUpperCase()
  }
  return trimmed.slice(0, 2).toUpperCase()
}

export const useAppIconStore = defineStore('appIcons', () => {
  // key -> data URL string, or null when backend confirmed no icon is available
  const icons = ref<Record<string, string | null>>({})
  const inFlight = new Set<string>()
  const pendingBatch = new Map<string, AppIconLookupRequest>()
  let flushScheduled = false

  function scheduleFlush() {
    if (flushScheduled) return
    flushScheduled = true
    queueMicrotask(() => {
      flushScheduled = false
      void flushPending()
    })
  }

  async function flushPending() {
    if (pendingBatch.size === 0) return
    const requests = Array.from(pendingBatch.values())
    pendingBatch.clear()

    for (const req of requests) {
      inFlight.add(req.requestKey)
    }

    try {
      const resolved = await invoke<Record<string, string>>('get_app_icons', { requests })
      const next = { ...icons.value }
      for (const req of requests) {
        next[req.requestKey] = resolved?.[req.requestKey] ?? null
      }
      icons.value = next
    } catch (error) {
      console.warn('Failed to load app icons:', error)
      const next = { ...icons.value }
      for (const req of requests) {
        if (!(req.requestKey in next)) {
          next[req.requestKey] = null
        }
      }
      icons.value = next
    } finally {
      for (const req of requests) {
        inFlight.delete(req.requestKey)
      }
    }
  }

  function getIconUrl(query: AppIconQuery): string | null {
    const requestKey = buildAppIconRequestKey(query)
    if (!requestKey) return null

    if (requestKey in icons.value) {
      return icons.value[requestKey] ?? null
    }

    if (!inFlight.has(requestKey) && !pendingBatch.has(requestKey)) {
      pendingBatch.set(requestKey, {
        requestKey,
        platform: query.platform ?? null,
        appKey: query.appKey ?? null,
        bundleId: query.bundleId ?? null,
        executablePath: query.executablePath ?? null,
        appUserModelId: query.appUserModelId ?? null,
        appName: query.appName ?? null,
      })
      scheduleFlush()
    }

    return null
  }

  function markFailed(query: AppIconQuery) {
    const requestKey = buildAppIconRequestKey(query)
    if (!requestKey) return
    icons.value = {
      ...icons.value,
      [requestKey]: null,
    }
  }

  function invalidateMissing() {
    const next: Record<string, string | null> = {}
    for (const [key, value] of Object.entries(icons.value)) {
      if (value !== null) {
        next[key] = value
      }
    }
    icons.value = next
  }

  return {
    icons,
    getIconUrl,
    markFailed,
    invalidateMissing,
    flushPending,
  }
})

if (import.meta.hot) {
  import.meta.hot.accept(acceptHMRUpdate(useAppIconStore, import.meta.hot))
}
