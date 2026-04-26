import type { AppSettings } from '../stores/settings'

export type AsrProviderValue = AppSettings['asrProviderType']
export type BatchCapableRecognitionMode = 'realtime' | 'batch'
export type PostRecordingBatchRefineValue = 'off' | 'batch_refine'
export type UnifiedAsrPipelineMode = 'realtime' | 'realtime_with_final_pass' | 'batch'
export type ElevenLabsRecognitionMode = AppSettings['elevenlabsRecognitionMode']
export type ElevenLabsPostRecordingRefine = AppSettings['elevenlabsPostRecordingRefine']
export type OpenAiPostRecordingRefine = AppSettings['openaiAsrPostRecordingRefine']
export type ColiRecognitionMode = BatchCapableRecognitionMode
export type ColiPostRecordingRefine = AppSettings['coliFinalRefinementMode']
export const QWEN_BATCH_RECORDING_LIMIT_MINUTES = 5
export const STEPAUDIO_RECORDING_LIMIT_MINUTES = 30

type ProviderOption<T extends string> = {
  label: string
  value: T
  disabled?: boolean
}

type Translate = (key: string) => string

const ASR_PROVIDER_LABEL_KEYS: Array<{ key: string; value: Exclude<AsrProviderValue, 'coli'> }> = [
  { key: 'asr.providerVolcengine', value: 'volcengine' },
  { key: 'asr.providerGoogle', value: 'google' },
  { key: 'asr.providerFunAsr', value: 'funasr' },
  { key: 'asr.providerQwen', value: 'qwen' },
  { key: 'asr.providerGemini', value: 'gemini' },
  { key: 'asr.providerGeminiLive', value: 'gemini-live' },
  { key: 'asr.providerCohere', value: 'cohere' },
  { key: 'asr.providerOpenAI', value: 'openai' },
  { key: 'asr.providerElevenLabs', value: 'elevenlabs' },
  { key: 'asr.providerSoniox', value: 'soniox' },
  { key: 'asr.providerStepAudio', value: 'stepaudio' }
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

const COLI_POST_RECORDING_REFINE_LABEL_KEYS: Array<{
  key: string
  value: ColiPostRecordingRefine
}> = [
  { key: 'asr.refinementOff', value: 'off' },
  { key: 'asr.refinementSenseVoice', value: 'sensevoice' },
  { key: 'asr.refinementWhisper', value: 'whisper' }
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

export function buildOpenAiRecognitionModeOptions(
  t: Translate
): Array<ProviderOption<AppSettings['openaiAsrMode']>> {
  return buildBatchCapableRecognitionModeOptions(t)
}

export function buildColiRecognitionModeOptions(
  t: Translate
): Array<ProviderOption<ColiRecognitionMode>> {
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

export function buildOpenAiPostRecordingRefineOptions(
  t: Translate
): Array<ProviderOption<OpenAiPostRecordingRefine>> {
  return buildPostRecordingBatchRefineOptions(t)
}

export function buildColiPostRecordingRefineOptions(
  t: Translate
): Array<ProviderOption<ColiPostRecordingRefine>> {
  return COLI_POST_RECORDING_REFINE_LABEL_KEYS.map(({ key, value }) => ({
    label: t(key),
    value
  }))
}

export function buildPostRecordingBatchRefineOptions(
  t: Translate
): Array<ProviderOption<PostRecordingBatchRefineValue>> {
  return ELEVENLABS_POST_RECORDING_REFINE_LABEL_KEYS.map(({ key, value }) => ({
    label: t(key),
    value
  }))
}

export function normalizeBatchCapablePostRecordingRefine(
  recognitionMode: BatchCapableRecognitionMode,
  postRecordingRefine: PostRecordingBatchRefineValue
): PostRecordingBatchRefineValue {
  return recognitionMode === 'batch' ? 'off' : postRecordingRefine
}

export function postRecordingBatchRefineEnabled(
  value: PostRecordingBatchRefineValue
): boolean {
  return value === 'batch_refine'
}

export function postRecordingBatchRefineValueFromBoolean(
  enabled: boolean
): PostRecordingBatchRefineValue {
  return enabled ? 'batch_refine' : 'off'
}

export function resolveBatchCapablePipelineMode(
  recognitionMode: BatchCapableRecognitionMode,
  postRecordingRefine: PostRecordingBatchRefineValue
): UnifiedAsrPipelineMode {
  if (recognitionMode === 'batch') {
    return 'batch'
  }
  if (normalizeBatchCapablePostRecordingRefine(recognitionMode, postRecordingRefine) === 'batch_refine') {
    return 'realtime_with_final_pass'
  }
  return 'realtime'
}

export function resolveQwenRecordingHardLimitMinutes(
  recognitionMode: BatchCapableRecognitionMode,
  postRecordingRefineEnabled: boolean
): number | null {
  const pipelineMode = resolveBatchCapablePipelineMode(
    recognitionMode,
    postRecordingBatchRefineValueFromBoolean(postRecordingRefineEnabled)
  )
  return pipelineMode === 'realtime' ? null : QWEN_BATCH_RECORDING_LIMIT_MINUTES
}

export function resolveAsrRecordingHardLimitMinutes(
  settings: Pick<AppSettings, 'asrProviderType' | 'qwenAsrRecognitionMode' | 'qwenAsrPostRecordingRefine'>
): number | null {
  if (settings.asrProviderType === 'stepaudio') {
    return STEPAUDIO_RECORDING_LIMIT_MINUTES
  }

  if (settings.asrProviderType !== 'qwen') {
    return null
  }

  return resolveQwenRecordingHardLimitMinutes(
    settings.qwenAsrRecognitionMode,
    settings.qwenAsrPostRecordingRefine
  )
}

export function exceedsRecordingHardLimit(
  configuredMaxRecordingMinutes: number,
  hardLimitMinutes: number | null
): boolean {
  if (hardLimitMinutes == null) {
    return false
  }

  return configuredMaxRecordingMinutes === 0 || configuredMaxRecordingMinutes > hardLimitMinutes
}

export function normalizeColiPostRecordingRefine(
  recognitionMode: ColiRecognitionMode,
  postRecordingRefine: ColiPostRecordingRefine
): ColiPostRecordingRefine {
  return recognitionMode === 'batch' ? 'off' : postRecordingRefine
}
