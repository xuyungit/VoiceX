import { defineStore } from 'pinia'
import { invoke } from '@tauri-apps/api/core'
import { reactive, ref } from 'vue'

export const MODEL_LIST_TTL_MS = 24 * 60 * 60 * 1000
export const MODEL_LIST_FAILURE_COOLDOWN_MS = 60 * 60 * 1000
const MAX_CACHED_ENTRIES = 32
const MODEL_CACHE_KEY_PATTERN = /^(?:openai|gemini|custom-openai|custom-gemini)\|[0-9a-f]{16}$/

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
  return `${scope}|${fingerprintSecret(`${normalizedUrl}\0${apiKey.trim()}`)}`
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
  const lastFailedAt =
    typeof record.lastFailedAt === 'number' && Number.isFinite(record.lastFailedAt)
      ? record.lastFailedAt
      : null
  const updatedAt =
    typeof record.updatedAt === 'number' && Number.isFinite(record.updatedAt)
      ? record.updatedAt
      : fetchedAt ?? lastFailedAt ?? 0
  return { models, fetchedAt, lastError: '', lastFailedAt, updatedAt }
}

function parseModelCacheJson(rawJson: string | null) {
  const entries: Record<string, LlmModelCacheEntry> = {}
  if (!rawJson) return { entries, needsRewrite: false }
  try {
    const parsed: unknown = JSON.parse(rawJson)
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return { entries, needsRewrite: true }
    }
    let needsRewrite = false
    for (const [key, value] of Object.entries(parsed)) {
      // Older cache keys embedded the full endpoint URL.
      if (!MODEL_CACHE_KEY_PATTERN.test(key)) {
        needsRewrite = true
        continue
      }
      const normalized = normalizeEntry(value)
      if (normalized) {
        entries[key] = normalized
        if ('lastError' in value) needsRewrite = true
      } else {
        needsRewrite = true
      }
    }
    return { entries, needsRewrite }
  } catch {
    return { entries, needsRewrite: true }
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
        const { entries: savedEntries, needsRewrite } = parseModelCacheJson(raw)
        Object.assign(entries, savedEntries)
        if (needsRewrite) await persist()
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
    // Keep detailed failures in memory. Timestamps alone drive the retry cooldown.
    const savedEntries = Object.fromEntries(
      Object.entries(entries).map(([key, { models, fetchedAt, lastFailedAt, updatedAt }]) => [
        key,
        { models, fetchedAt, lastFailedAt, updatedAt }
      ])
    )
    try {
      await invoke('save_llm_model_cache', { json: JSON.stringify(savedEntries) })
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
        const rawMessage = err instanceof Error ? err.message : String(err)
        entries[key] = {
          models: previous?.models ?? null,
          fetchedAt: previous?.fetchedAt ?? null,
          lastError: rawMessage,
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
