import { ASR_DICTIONARY_POLICIES, isAsrDictionaryEnabled } from './asrDictionary'
import type { AppSettings } from '../stores/settings'

export function checkAsrDictionarySettings(settings: AppSettings) {
  const fixture = { ...settings, asrDictionaryEnabled: {} }
  for (const provider of Object.keys(ASR_DICTIONARY_POLICIES) as AppSettings['asrProviderType'][]) {
    const expected = ASR_DICTIONARY_POLICIES[provider].defaultEnabled
    if (isAsrDictionaryEnabled(fixture, provider) !== expected) throw new Error(`Wrong default: ${provider}`)
  }
  fixture.asrDictionaryEnabled = { gemini: true, soniox: false }
  if (!isAsrDictionaryEnabled(fixture, 'gemini') || isAsrDictionaryEnabled(fixture, 'soniox')) {
    throw new Error('Provider dictionary preferences must be independent')
  }
  fixture.qwenLocalUseDictionary = false
  fixture.elevenlabsEnableKeyterms = false
  if (isAsrDictionaryEnabled(fixture, 'qwen-local') || isAsrDictionaryEnabled(fixture, 'elevenlabs')) {
    throw new Error('Legacy disabled preferences must survive')
  }
  fixture.asrDictionaryEnabled = { 'qwen-local': true, elevenlabs: true }
  if (!isAsrDictionaryEnabled(fixture, 'qwen-local') || !isAsrDictionaryEnabled(fixture, 'elevenlabs')) {
    throw new Error('Explicit preferences must override legacy flags')
  }
}
