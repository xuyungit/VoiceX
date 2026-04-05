type ProviderOption<T extends string> = {
  label: string
  value: T
  disabled?: boolean
}

type Translate = (key: string) => string

export const LLM_PROVIDER_VALUES = ['volcengine', 'openai', 'qwen', 'custom'] as const
export type LlmProviderValue = typeof LLM_PROVIDER_VALUES[number]

export const LLM_API_MODE_VALUES = ['chat_completions', 'responses'] as const
export type LlmApiModeValue = typeof LLM_API_MODE_VALUES[number]

const LLM_PROVIDER_LABEL_KEYS: Array<{ key: string; value: LlmProviderValue }> = [
  { key: 'llm.providerVolcengine', value: 'volcengine' },
  { key: 'llm.providerOpenAI', value: 'openai' },
  { key: 'llm.providerQwen', value: 'qwen' },
  { key: 'llm.providerCustom', value: 'custom' }
]

const LLM_API_MODE_LABEL_KEYS: Array<{ key: string; value: LlmApiModeValue }> = [
  { key: 'llm.apiModeChatCompletions', value: 'chat_completions' },
  { key: 'llm.apiModeResponses', value: 'responses' }
]

export function buildLlmProviderOptions(t: Translate): Array<ProviderOption<LlmProviderValue>> {
  return LLM_PROVIDER_LABEL_KEYS.map(({ key, value }) => ({
    label: t(key),
    value
  }))
}

export function buildLlmApiModeOptions(t: Translate): Array<ProviderOption<LlmApiModeValue>> {
  return LLM_API_MODE_LABEL_KEYS.map(({ key, value }) => ({
    label: t(key),
    value
  }))
}
