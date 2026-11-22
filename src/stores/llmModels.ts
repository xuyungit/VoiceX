import { defineStore } from 'pinia'
import { invoke } from '@tauri-apps/api/core'
import { reactive, ref } from 'vue'

export const MODEL_LIST_TTL_MS = 24 * 60 * 60 * 1000
export const MODEL_LIST_FAILURE_COOLDOWN_MS = 60 * 60 * 1000
const MAX_CACHED_ENTRIES = 32

export type ModelCacheScope = 'openai' | 'gemini' | 'custom-openai' | 'custom-gemini'

export interface LlmModelCacheEntry {
  models: string[] | null
  fetchedAt: number | null
  lastError: string
  lastFailedAt: number | null
  updatedAt: number
}

/**
 * Two independent 32-bit hashes so cached keys never store plaintext API keys
 * while still invalidating immediately when the user edits credentials.
 */
function fingerprintSecret(secret: string): string {
  let fnv = 0x811c9dc5
  let djb = 5381
  for (let i = 0; i < secret.length; i++) {
    const code = secret.charCodeAt(i)
    fnv ^= code
    fnv = Math.imul(fnv, 0x01000193)
    djb = Math.imul(djb, 33) ^ code
  }
  return `${(fnv >>> 0).toString(16).padStart(8, '0')}${(djb >>> 0).toString(16).padStart(8, '0')}`
}

export function buildModelCacheKey(
  scope: ModelCacheScope,
  baseUrl: string,
  apiKey: string
): string {
  const normalizedUrl = baseUrl.trim().replace(/\/+$/, '')
  return `${scope}|${normalizedUrl}|${fingerprintSecret(apiKey.trim())}`
}

export function shouldAutoFetchModels(
  entry: LlmModelCacheEntry | undefined,
  now: number = Date.now()
): boolean {
  if (entry?.fetchedAt != null && now - entry.fetchedAt < MODEL_LIST_TTL_MS) {
    return false
  }
  if (entry?.lastFailedAt != null && now - entry.lastFailedAt < MODEL_LIST_FAILURE_COOLDOWN_MS) {
    return false
  }
  return true
}

function normalizeEntry(raw: unknown): LlmModelCacheEntry | null {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return null
  const record = raw as Record<string, unknown>
  const models = Array.isArray(record.models)
    ? record.models.filter((item): item is string => typeof item === 'string')
    : null
  const fetchedAt = typeof record.fetchedAt === 'number' && Number.isFinite(record.fetchedAt)
    ? record.fetchedAt
    : null
  const lastError = typeof record.lastError === 'string' ? record.lastError : ''
  const lastFailedAt =
    typeof record.lastFailedAt === 'number' && Number.isFinite(record.lastFailedAt)
      ? record.lastFailedAt
      : null
  const updatedAt =
    typeof record.updatedAt === 'number' && Number.isFinite(record.updatedAt)
      ? record.updatedAt
      : fetchedAt ?? lastFailedAt ?? 0
  return { models, fetchedAt, lastError, lastFailedAt, updatedAt }
}

export function parseModelCacheJson(rawJson: string | null | undefined): Record<string, LlmModelCacheEntry> {
  if (!rawJson) return {}
  try {
    const parsed: unknown = JSON.parse(rawJson)
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {}
    const result: Record<string, LlmModelCacheEntry> = {}
    for (const [key, value] of Object.entries(parsed)) {
      const normalized = normalizeEntry(value)
      if (normalized) result[key] = normalized
    }
    return result
  } catch {
    return {}
  }
}

function pruneEntries(entries: Record<string, LlmModelCacheEntry>) {
  const keys = Object.keys(entries)
  if (keys.length <= MAX_CACHED_ENTRIES) return
  keys
    .sort((a, b) => (entries[a]?.updatedAt ?? 0) - (entries[b]?.updatedAt ?? 0))
    .slice(0, keys.length - MAX_CACHED_ENTRIES)
    .forEach((key) => {
      delete entries[key]
    })
}

export const useLlmModelCacheStore = defineStore('llmModelCache', () => {
  const entries = reactive<Record<string, LlmModelCacheEntry>>({})
  const isLoaded = ref(false)
  const loadingKeys = reactive<Record<string, boolean>>({})
  const inFlight = new Map<string, Promise<void>>()
  let loadPromise: Promise<void> | null = null

  async function ensureLoaded(): Promise<void> {
    if (isLoaded.value) return
    if (loadPromise) return loadPromise
    loadPromise = (async () => {
      try {
        const raw = await invoke<string | null>('get_llm_model_cache')
        const parsed = parseModelCacheJson(raw)
        for (const [key, entry] of Object.entries(parsed)) {
          if (!entries[key]) {
            entries[key] = entry
          }
        }
      } catch (err) {
        console.warn('Failed to load LLM model list cache:', err)
      } finally {
        isLoaded.value = true
        loadPromise = null
      }
    })()
    return loadPromise
  }

  async function persist(): Promise<void> {
    pruneEntries(entries)
    try {
      await invoke('save_llm_model_cache', { json: JSON.stringify(entries) })
    } catch (err) {
      console.warn('Failed to persist LLM model list cache:', err)
    }
  }

  function getEntry(key: string): LlmModelCacheEntry | undefined {
    return entries[key]
  }

  function isLoading(key: string): boolean {
    return Boolean(loadingKeys[key])
  }

  async function refresh(
    key: string,
    fetcher: () => Promise<string[]>,
    options: { force?: boolean } = {}
  ): Promise<void> {
    await ensureLoaded()
    const force = Boolean(options.force)
    if (!force && !shouldAutoFetchModels(entries[key], Date.now())) {
      return
    }
    const existingPromise = inFlight.get(key)
    if (existingPromise) {
      return existingPromise
    }

    const task = (async () => {
      loadingKeys[key] = true
      try {
        const models = await fetcher()
        const now = Date.now()
        entries[key] = {
          models,
          fetchedAt: now,
          lastError: '',
          lastFailedAt: null,
          updatedAt: now
        }
      } catch (err) {
        const now = Date.now()
        const previous = entries[key]
        entries[key] = {
          models: previous?.models ?? null,
          fetchedAt: previous?.fetchedAt ?? null,
          lastError: err instanceof Error ? err.message : String(err),
          lastFailedAt: now,
          updatedAt: now
        }
      } finally {
        loadingKeys[key] = false
        inFlight.delete(key)
        await persist()
      }
    })()

    inFlight.set(key, task)
    return task
  }

  return {
    entries,
    isLoaded,
    ensureLoaded,
    getEntry,
    isLoading,
    refresh
  }
})
