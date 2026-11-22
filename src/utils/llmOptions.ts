import type { CustomLlmEndpoint } from '../stores/settings'
import llmModels from '../shared/llmModels.json'

type ProviderOption<T extends string> = {
  label: string
  value: T
  disabled?: boolean
}

type ProviderGroupOption = {
  type: 'group'
  label: string
  key: string
  children: Array<ProviderOption<string>>
}

export type ProviderSelectOption = ProviderOption<string> | ProviderGroupOption

type Translate = (key: string) => string

export const LLM_PROVIDER_VALUES = ['volcengine', 'openai', 'qwen', 'deepseek', 'gemini', 'anthropic', 'custom'] as const
export type LlmProviderValue = typeof LLM_PROVIDER_VALUES[number]

export const LLM_API_MODE_VALUES = ['chat_completions', 'responses', 'gemini'] as const
export type LlmApiModeValue = typeof LLM_API_MODE_VALUES[number]

export type OpenAiServiceTier = 'default' | 'fast'

export const REASONING_EFFORT_LABEL_KEYS: Record<string, string> = {
  none: 'llm.reasoningNone',
  minimal: 'llm.reasoningMinimal',
  low: 'llm.low',
  medium: 'llm.medium',
  high: 'llm.high',
  xhigh: 'llm.xhigh'
}

export function buildReasoningEffortOptions(
  t: Translate,
  supportedEfforts: readonly string[]
): Array<ProviderOption<string>> {
  const options: Array<ProviderOption<string>> = [
    { label: t('llm.reasoningLowest'), value: '' },
    { label: t('llm.reasoningServerDefault'), value: 'server_default' }
  ]
  for (const effort of supportedEfforts) {
    const key = REASONING_EFFORT_LABEL_KEYS[effort]
    options.push({
      label: key ? t(key) : effort,
      value: effort
    })
  }
  return options
}

export function isOfficialOpenAiBaseUrl(baseUrl: string): boolean {
  const url = baseUrl.trim().replace(/\/+$/, '')
  return url === llmModels.openaiBaseUrl || llmModels.openaiRegionalBaseUrls.includes(url)
}

// Dropdown value prefix for a specific named custom endpoint, e.g. `custom:<id>`.
export const CUSTOM_ENDPOINT_PREFIX = 'custom:'
// Sentinel dropdown value for the "add a new custom endpoint" action.
export const ADD_CUSTOM_ENDPOINT_VALUE = '__add_custom_endpoint__'

const BUILTIN_PROVIDER_LABEL_KEYS: Array<{ key: string; value: Exclude<LlmProviderValue, 'custom'> }> = [
  { key: 'llm.providerVolcengine', value: 'volcengine' },
  { key: 'llm.providerOpenAI', value: 'openai' },
  { key: 'llm.providerQwen', value: 'qwen' },
  { key: 'llm.providerDeepSeek', value: 'deepseek' },
  { key: 'llm.providerGemini', value: 'gemini' },
  { key: 'llm.providerAnthropic', value: 'anthropic' }
]

const LLM_API_MODE_LABEL_KEYS: Array<{ key: string; value: LlmApiModeValue }> = [
  { key: 'llm.apiModeChatCompletions', value: 'chat_completions' },
  { key: 'llm.apiModeResponses', value: 'responses' },
  { key: 'llm.apiModeGemini', value: 'gemini' }
]

/**
 * Build the provider dropdown options: the built-in providers, then a group of
 * named custom OpenAI-compatible endpoints. When `includeAddAction` is true an
 * "add custom endpoint" item is appended to the custom group.
 */
export function buildLlmProviderOptions(
  t: Translate,
  endpoints: CustomLlmEndpoint[] = [],
  options: { includeAddAction?: boolean } = {}
): ProviderSelectOption[] {
  const result: ProviderSelectOption[] = BUILTIN_PROVIDER_LABEL_KEYS.map(({ key, value }) => ({
    label: t(key),
    value
  }))

  const children: Array<ProviderOption<string>> = endpoints.map((endpoint) => ({
    label: endpoint.name.trim() || t('llm.customEndpointUnnamed'),
    value: `${CUSTOM_ENDPOINT_PREFIX}${endpoint.id}`
  }))

  if (options.includeAddAction) {
    children.push({ label: t('llm.addCustomEndpoint'), value: ADD_CUSTOM_ENDPOINT_VALUE })
  }

  if (children.length > 0) {
    result.push({
      type: 'group',
      label: t('llm.providerCustom'),
      key: 'custom-endpoints',
      children
    })
  }

  return result
}

/** Resolve the dropdown selection key from the persisted provider type + active endpoint id. */
export function providerSelectionKey(
  providerType: LlmProviderValue,
  endpoints: CustomLlmEndpoint[],
  activeEndpointId: string
): string {
  if (providerType !== 'custom') {
    return providerType
  }
  const active = endpoints.find((endpoint) => endpoint.id === activeEndpointId)
  const id = active?.id ?? endpoints[0]?.id ?? ''
  return `${CUSTOM_ENDPOINT_PREFIX}${id}`
}

export function buildLlmApiModeOptions(t: Translate): Array<ProviderOption<LlmApiModeValue>> {
  return LLM_API_MODE_LABEL_KEYS.map(({ key, value }) => ({
    label: t(key),
    value
  }))
}
