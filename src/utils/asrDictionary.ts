import policies from '../shared/asrDictionary.json'
import type { AppSettings } from '../stores/settings'

export const ASR_DICTIONARY_POLICIES = policies

export function isAsrDictionaryEnabled(
  settings: Pick<AppSettings, 'asrProviderType' | 'asrDictionaryEnabled' | 'qwenLocalUseDictionary' | 'elevenlabsEnableKeyterms'>,
  provider = settings.asrProviderType
): boolean {
  const policy = policies[provider]
  if (policy.channel === 'unsupported') return false
  const explicit = settings.asrDictionaryEnabled[provider]
  if (explicit !== undefined) return explicit
  // Preserve the two controls that predate per-provider dictionary settings.
  if (provider === 'qwen-local') return settings.qwenLocalUseDictionary
  if (provider === 'elevenlabs') return settings.elevenlabsEnableKeyterms
  return policy.defaultEnabled
}
