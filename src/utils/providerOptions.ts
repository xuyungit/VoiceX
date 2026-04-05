import type { AppSettings } from '../stores/settings'

export type AsrProviderValue = AppSettings['asrProviderType']
export type BatchCapableRecognitionMode = 'realtime' | 'batch'
export type PostRecordingBatchRefineValue = 'off' | 'batch_refine'
export type ElevenLabsRecognitionMode = AppSettings['elevenlabsRecognitionMode']
export type ElevenLabsPostRecordingRefine = AppSettings['elevenlabsPostRecordingRefine']

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
  { key: 'asr.providerElevenLabs', value: 'elevenlabs' },
  { key: 'asr.providerSoniox', value: 'soniox' }
]

const ELEVENLABS_RECOGNITION_MODE_LABEL_KEYS: Array<{
  key: string
  value: BatchCapableRecognitionMode
}> = [
  { key: 'asr.batchCapableRecognitionModeRealtime', value: 'realtime' },
  { key: 'asr.batchCapableRecognitionModeBatch', value: 'batch' }
]

const ELEVENLABS_POST_RECORDING_REFINE_LABEL_KEYS: Array<{
  key: string
  value: PostRecordingBatchRefineValue
}> = [
  { key: 'asr.postRecordingBatchRefineOff', value: 'off' },
  { key: 'asr.postRecordingBatchRefineBatch', value: 'batch_refine' }
]

export const ELEVENLABS_REALTIME_MODEL_OPTIONS = [
  { label: 'scribe_v2_realtime', value: 'scribe_v2_realtime' }
]

export const ELEVENLABS_BATCH_MODEL_OPTIONS = [
  { label: 'scribe_v2', value: 'scribe_v2' }
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

export function buildElevenLabsRecognitionModeOptions(
  t: Translate
): Array<ProviderOption<ElevenLabsRecognitionMode>> {
  return buildBatchCapableRecognitionModeOptions(t)
}

export function buildBatchCapableRecognitionModeOptions(
  t: Translate
): Array<ProviderOption<BatchCapableRecognitionMode>> {
  return ELEVENLABS_RECOGNITION_MODE_LABEL_KEYS.map(({ key, value }) => ({
    label: t(key),
    value
  }))
}

export function buildElevenLabsPostRecordingRefineOptions(
  t: Translate
): Array<ProviderOption<ElevenLabsPostRecordingRefine>> {
  return buildPostRecordingBatchRefineOptions(t)
}

export function buildPostRecordingBatchRefineOptions(
  t: Translate
): Array<ProviderOption<PostRecordingBatchRefineValue>> {
  return ELEVENLABS_POST_RECORDING_REFINE_LABEL_KEYS.map(({ key, value }) => ({
    label: t(key),
    value
  }))
}
