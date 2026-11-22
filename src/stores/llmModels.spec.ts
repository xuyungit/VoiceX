import {
  buildModelCacheKey,
  MODEL_LIST_FAILURE_COOLDOWN_MS,
  MODEL_LIST_TTL_MS,
  parseModelCacheJson,
  shouldAutoFetchModels,
  type LlmModelCacheEntry
} from './llmModels'

export function checkLlmModelCache() {
  const now = 1_700_000_000_000

  // Key normalizes trailing slashes and never leaks plaintext API keys.
  const k1 = buildModelCacheKey('gemini', 'https://generativelanguage.googleapis.com/', 'secret-key-1')
  const k2 = buildModelCacheKey('gemini', 'https://generativelanguage.googleapis.com', 'secret-key-1')
  const k3 = buildModelCacheKey('gemini', 'https://generativelanguage.googleapis.com', 'secret-key-2')
  if (k1 !== k2) throw new Error('Trailing slash must normalize to the same cache key')
  if (k1 === k3) throw new Error('Different API keys must produce distinct cache keys')
  if (k1.includes('secret-key-1')) throw new Error('Cache key must not contain plaintext API key')

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

  // JSON round-trip restores models, timestamps, and error safely.
  const parsed = parseModelCacheJson(JSON.stringify({ [k1]: failedRecently }))
  if (
    parsed[k1]?.models?.[0] !== 'gemini-3.5-flash-lite' ||
    parsed[k1]?.lastError !== 'Gemini models HTTP 403'
  ) {
    throw new Error('parseModelCacheJson must restore cached models and lastError')
  }
  if (Object.keys(parseModelCacheJson('not-json')).length !== 0) {
    throw new Error('parseModelCacheJson must ignore malformed JSON')
  }
}
