import { createPinia, disposePinia } from 'pinia'
import {
  buildModelCacheKey,
  MODEL_LIST_FAILURE_COOLDOWN_MS,
  MODEL_LIST_TTL_MS,
  shouldAutoFetchModels,
  useLlmModelCacheStore,
  type LlmModelCacheEntry
} from './llmModels'

export function checkLlmModelCache() {
  const now = 1_700_000_000_000

  // Key normalizes trailing slashes and never leaks plaintext URLs or API keys.
  const secretUrl = 'https://proxy.example.com/token/secret-path-token/v1?key=query-secret'
  const k1 = buildModelCacheKey('custom-gemini', `${secretUrl}/`, 'secret-key-1')
  const k2 = buildModelCacheKey('custom-gemini', secretUrl, 'secret-key-1')
  const k3 = buildModelCacheKey('custom-gemini', secretUrl, 'secret-key-2')
  const k4 = buildModelCacheKey('custom-gemini', 'https://other.example.com/v1', 'secret-key-1')
  if (k1 !== k2) throw new Error('Trailing slash must normalize to the same cache key')
  if (k1 === k3) throw new Error('Different API keys must produce distinct cache keys')
  if (k1 === k4) throw new Error('Different base URLs must produce distinct cache keys')
  if (k1.includes('secret-key-1') || k1.includes('proxy.example.com') || k1.includes('query-secret')) {
    throw new Error('Cache key must not contain plaintext URL or API key')
  }

  // Uncached entry auto-fetches.
  if (!shouldAutoFetchModels(undefined, now)) {
    throw new Error('Missing cache entry should auto-fetch')
  }

  // Fresh entry (< 24h) skips auto-fetch even if a manual retry failed earlier or not.
  const fresh: LlmModelCacheEntry = {
    models: ['gemini-3.5-flash-lite'],
    fetchedAt: now - MODEL_LIST_TTL_MS + 60_000,
    lastError: '',
    lastFailedAt: null,
    updatedAt: now - 1000
  }
  if (shouldAutoFetchModels(fresh, now)) {
    throw new Error('Fresh cache (< 24h) must not auto-fetch')
  }

  // Stale entry (>= 24h) triggers auto-fetch when outside failure cooldown.
  const stale: LlmModelCacheEntry = {
    ...fresh,
    fetchedAt: now - MODEL_LIST_TTL_MS - 1
  }
  if (!shouldAutoFetchModels(stale, now)) {
    throw new Error('Stale cache (>= 24h) should auto-fetch')
  }

  // Failed entry within 1h cooldown skips auto-fetch while preserving lastError.
  const failedRecently: LlmModelCacheEntry = {
    models: ['gemini-3.5-flash-lite'],
    fetchedAt: now - MODEL_LIST_TTL_MS - 10_000,
    lastError: 'Gemini models HTTP 403',
    lastFailedAt: now - MODEL_LIST_FAILURE_COOLDOWN_MS + 1000,
    updatedAt: now - 1000
  }
  if (shouldAutoFetchModels(failedRecently, now)) {
    throw new Error('Failed entry within 1h cooldown must not auto-fetch')
  }

  // After 1h cooldown, auto-fetch runs again.
  const cooldownElapsed: LlmModelCacheEntry = {
    ...failedRecently,
    lastFailedAt: now - MODEL_LIST_FAILURE_COOLDOWN_MS - 1
  }
  if (!shouldAutoFetchModels(cooldownElapsed, now)) {
    throw new Error('Failed entry past 1h cooldown should retry auto-fetch')
  }

}

function check(condition: boolean, message: string) {
  if (!condition) throw new Error(message)
}

export async function checkLlmModelCachePersistence() {
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
  const firstPinia = createPinia()
  const restartedPinia = createPinia()
  const openaiKey = buildModelCacheKey('openai', 'https://api.openai.com/v1', 'fixture')
  const geminiKey = buildModelCacheKey('gemini', 'https://generativelanguage.googleapis.com', 'fixture')
  const legacyUrl = 'https://relay.example.com/token/legacy-secret'
  const legacyKey = `custom-openai|${legacyUrl}|0000000000000000`
  const now = Date.now()
  const cached = {
    models: ['last-good-model'],
    fetchedAt: now,
    lastError: 'Invalid API key old-secret',
    lastFailedAt: null,
    updatedAt: now
  }
  let saved = JSON.stringify({
    [openaiKey]: cached,
    [geminiKey]: { ...cached, fetchedAt: now - MODEL_LIST_TTL_MS - 60_000 },
    [legacyKey]: cached
  })
  let writes = 0
  let requests = 0
  const fetchModels = async () => {
    requests++
    return ['updated-model']
  }
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { __TAURI_INTERNALS__: { async invoke(command: string, args: { json: string }) {
      if (command === 'get_llm_model_cache') return saved
      if (command === 'save_llm_model_cache') {
        saved = args.json
        writes++
        return
      }
      throw new Error(`Unexpected command: ${command}`)
    } } }
  })
  try {
    const store = useLlmModelCacheStore(firstPinia)
    await store.ensureLoaded()
    check(writes === 1 && !saved.includes('legacy-secret') && !saved.includes('old-secret'),
      'Loading an older cache should remove plaintext URLs and past error details from disk')
    check(store.getEntry(openaiKey)?.models?.[0] === 'last-good-model',
      'Cleanup should preserve existing models under a hashed cache key')

    await store.refresh(openaiKey, fetchModels)
    await store.refresh(openaiKey, fetchModels)
    check(requests === 0 && writes === 1, 'Revisiting a fresh cache should skip network and disk writes')

    await store.refresh(geminiKey, async () => {
      requests++
      throw new Error('Invalid API key new-secret')
    })
    check(!saved.includes('new-secret'), 'Refreshing should never persist remote error details')
    check(store.getEntry(geminiKey)?.models?.[0] === 'last-good-model',
      'A failed refresh should retain the previous model list')

    const restarted = useLlmModelCacheStore(restartedPinia)
    await restarted.refresh(openaiKey, fetchModels)
    await restarted.refresh(geminiKey, fetchModels)
    check(requests === 1 && writes === 2, 'A restart should honor both the TTL and failure cooldown')
    check(restarted.getEntry(geminiKey)?.lastFailedAt != null &&
      restarted.getEntry(geminiKey)?.lastError === '',
      'The failure timestamp should survive restart without the error details')

    await restarted.refresh(geminiKey, fetchModels, { force: true })
    check(requests === 2 && restarted.getEntry(geminiKey)?.models?.[0] === 'updated-model',
      'Manual refresh should bypass the cooldown')
  } finally {
    disposePinia(firstPinia)
    disposePinia(restartedPinia)
    if (originalWindow) Object.defineProperty(globalThis, 'window', originalWindow)
    else Reflect.deleteProperty(globalThis, 'window')
  }
}
