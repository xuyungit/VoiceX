import { ASR_MODEL_DEFAULTS, buildModelOptions, findAsrModel, findRetiredAsrModel, modelSelectionError, openAiModelForMode } from './asrModels'

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message)
}

const t = (key: string) => key

export function checkOpenAiModelModes() {
  const live = ASR_MODEL_DEFAULTS.openaiRealtime
  const batch = ASR_MODEL_DEFAULTS.openai
  assert(openAiModelForMode(batch, 'realtime') === live, 'Switching to realtime should select live partials')
  assert(openAiModelForMode(live, 'batch') === batch, 'Switching to batch must replace a realtime-only model')
  assert(openAiModelForMode('gpt-4o-transcribe', 'batch') === 'gpt-4o-transcribe', 'Keep compatible saved models')
  assert(openAiModelForMode('my-deployment', 'realtime') === 'my-deployment', 'Keep custom deployment IDs')
  assert(!buildModelOptions('openai', 'batch', '', t).some(option => option.value === live), 'Batch choices exclude realtime-only models')
  assert(buildModelOptions('openai', 'realtime', '', t).some(option => option.value === batch && !option.disabled), 'Committed-turn model remains available in realtime mode')
  assert(findAsrModel('openai', live)?.delay === true && !findAsrModel('openai', batch)?.delay, 'Delay only applies to live model')
}

export function checkAsrModelPresets() {
  for (const [provider, id, mode] of [
    ['gemini-live', ASR_MODEL_DEFAULTS.geminiLive, 'realtime'],
    ['gemini-live', 'gemini-3.8-live', 'realtime'],
    ['openai', 'whisper-1', 'batch'],
    ['soniox', ASR_MODEL_DEFAULTS.soniox, 'realtime'],
    ['soniox', 'stt-rt-v5', 'realtime'],
    ['elevenlabs', ASR_MODEL_DEFAULTS.elevenlabsRealtime, 'realtime'],
    ['elevenlabs', 'scribe_v2_realtime', 'realtime'],
    ['elevenlabs', ASR_MODEL_DEFAULTS.elevenlabsBatch, 'batch'],
    ['elevenlabs', 'scribe_v2', 'batch']
  ] as const) {
    const option = buildModelOptions(provider, mode, '', t).find(candidate => candidate.value === id)
    assert(option && !option.disabled, `Missing selectable preset: ${provider}/${id} (${mode})`)
  }
  const gemini31 = buildModelOptions('gemini', 'batch', '', t).find(option => option.value === 'gemini-3.1-flash-lite')
  assert(gemini31 && !gemini31.disabled && gemini31.label.includes('asr.modelGemini31EarliestShutdown'), 'Gemini 3.1 Flash-Lite remains usable with its future shutdown notice')
}

export function checkSavedAsrModels() {
  const retired = 'gemini-3.1-flash-lite-preview'
  const deployment = `${retired}-my-deployment`
  assert(!buildModelOptions('gemini', 'batch', '', t).some(option => option.value === retired), 'Retired models must be left out of presets')
  const saved = buildModelOptions('gemini', 'batch', retired, t).find(option => option.value === retired)
  assert(saved?.disabled && saved.label.includes('asr.modelStatus_retired'), 'Saved retired value must remain visible and disabled')
  assert(modelSelectionError('gemini', ` ${retired} `, 'batch'), 'Retired models must fail validation even with whitespace')
  assert(!modelSelectionError('custom', retired, 'batch'), 'Retirement must be scoped to the provider')
  const custom = buildModelOptions('gemini', 'batch', deployment, t).find(option => option.value === deployment)
  assert(custom && !custom.disabled && custom.label.includes('asr.modelCustom'), 'A saved custom deployment with a similar ID must remain selectable')

  const filetrans = 'qwen3-asr-flash-filetrans-2025-11-17'
  assert(findRetiredAsrModel('qwen', filetrans)?.note === 'modelRetiredFiletransNote', 'Unsupported asynchronous migration must explain its limitation')
}
