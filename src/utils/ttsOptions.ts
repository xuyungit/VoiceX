// Shared provider identities for persisted settings and reading controls.
export const TTS_PROVIDERS = [
  { value: 'system', labelKey: 'reading.providerSystem' },
  { value: 'volcengine', labelKey: 'reading.providerVolcengine' },
  { value: 'aliyun', labelKey: 'reading.providerAliyun' },
  { value: 'mimo', labelKey: 'reading.providerMimo' },
  { value: 'azure', labelKey: 'reading.providerAzure' },
  { value: 'edge', labelKey: 'reading.providerEdge' }
] as const

export type TtsProviderValue = typeof TTS_PROVIDERS[number]['value']
export function buildTtsProviderOptions(t: (key: string) => string) {
  return TTS_PROVIDERS.map(({ value, labelKey }) => ({ value, label: t(labelKey) }))
}
