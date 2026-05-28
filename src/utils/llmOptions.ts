import type { CustomLlmEndpoint } from '../stores/settings'

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

export const LLM_PROVIDER_VALUES = ['volcengine', 'openai', 'qwen', 'custom'] as const
export type LlmProviderValue = typeof LLM_PROVIDER_VALUES[number]

export const LLM_API_MODE_VALUES = ['chat_completions', 'responses'] as const
export type LlmApiModeValue = typeof LLM_API_MODE_VALUES[number]

// Dropdown value prefix for a specific named custom endpoint, e.g. `custom:<id>`.
export const CUSTOM_ENDPOINT_PREFIX = 'custom:'
// Sentinel dropdown value for the "add a new custom endpoint" action.
export const ADD_CUSTOM_ENDPOINT_VALUE = '__add_custom_endpoint__'

const BUILTIN_PROVIDER_LABEL_KEYS: Array<{ key: string; value: Exclude<LlmProviderValue, 'custom'> }> = [
  { key: 'llm.providerVolcengine', value: 'volcengine' },
  { key: 'llm.providerOpenAI', value: 'openai' },
  { key: 'llm.providerQwen', value: 'qwen' }
]

const LLM_API_MODE_LABEL_KEYS: Array<{ key: string; value: LlmApiModeValue }> = [
  { key: 'llm.apiModeChatCompletions', value: 'chat_completions' },
  { key: 'llm.apiModeResponses', value: 'responses' }
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
