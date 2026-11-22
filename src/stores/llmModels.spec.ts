import { createPinia, disposePinia } from 'pinia'
import {
  buildModelCacheKey,
  MODEL_LIST_FAILURE_COOLDOWN_MS,
  MODEL_LIST_TTL_MS,
  shouldAutoFetchModels,
  useLlmModelCacheStore,
  type LlmModelCacheEntry
} from './llmModels'

export function checkLlmModelCacheKey() {
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
}

export function checkLlmModelRefreshBoundaries() {
  const now = 1_700_000_000_000
  const expired: LlmModelCacheEntry = {
    models: ['cached-model'],
    fetchedAt: now - MODEL_LIST_TTL_MS,
    lastError: '',
    lastFailedAt: null,
    updatedAt: now - MODEL_LIST_TTL_MS
  }
  const cooldownElapsed = { ...expired, lastFailedAt: now - MODEL_LIST_FAILURE_COOLDOWN_MS }

  check(shouldAutoFetchModels(undefined, now), 'The first visit must fetch models')
  check(shouldAutoFetchModels(expired, now), 'A refresh is due at the TTL boundary')
  check(shouldAutoFetchModels(cooldownElapsed, now), 'A failed entry can retry at the cooldown boundary')
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
