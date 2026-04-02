import type { AppSettings } from '../stores/settings'

export type AsrProviderValue = AppSettings['asrProviderType']
export type LlmProviderValue = AppSettings['llmProviderType']

type ProviderOption<T extends string> = {
  label: string
  value: T
  disabled?: boolean
}

type Translate = (key: string) => string

const ASR_PROVIDER_LABEL_KEYS: Array<{ key: string; value: Exclude<AsrProviderValue, 'coli'> }> = [
  { key: 'asr.providerVolcengine', value: 'volcengine' },
  { key: 'asr.providerGoogle', value: 'google' },
  { key: 'asr.providerQwen', value: 'qwen' },
  { key: 'asr.providerGemini', value: 'gemini' },
  { key: 'asr.providerGeminiLive', value: 'gemini-live' },
  { key: 'asr.providerCohere', value: 'cohere' },
  { key: 'asr.providerOpenAI', value: 'openai' },
  { key: 'asr.providerSoniox', value: 'soniox' }
]

const LLM_PROVIDER_LABEL_KEYS: Array<{ key: string; value: LlmProviderValue }> = [
  { key: 'llm.providerVolcengine', value: 'volcengine' },
  { key: 'llm.providerOpenAI', value: 'openai' },
  { key: 'llm.providerQwen', value: 'qwen' },
  { key: 'llm.providerCustom', value: 'custom' }
]

export function buildAsrProviderOptions(
  t: Translate,
  options: {
    coliLabel?: string
    coliDisabled?: boolean
  } = {}
): Array<ProviderOption<AsrProviderValue>> {
  return [
    ...ASR_PROVIDER_LABEL_KEYS.map(({ key, value }) => ({
      label: t(key),
      value
    })),
    {
      label: options.coliLabel ?? t('asr.providerColi'),
      value: 'coli',
      disabled: options.coliDisabled
    }
  ]
}

export function buildLlmProviderOptions(t: Translate): Array<ProviderOption<LlmProviderValue>> {
  return LLM_PROVIDER_LABEL_KEYS.map(({ key, value }) => ({
    label: t(key),
    value
  }))
}
