import { ASR_MODEL_DEFAULTS, buildModelOptions, findRetiredAsrModel, modelSelectionError } from './asrModels'

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message)
}

export function checkAsrModelOptions() {
  const retiredId = 'gemini-3.1-flash-lite-preview'
  const t = (key: string) => key
  const choices = buildModelOptions('gemini', 'batch', 'gemini-3.5-flash-lite', t)
  assert(!choices.some(option => option.value === retiredId), 'Retired model must not be offered as a preset')

  const saved = buildModelOptions('gemini', 'batch', retiredId, t).find(option => option.value === retiredId)
  assert(saved?.disabled && saved.label.includes('asr.modelStatus_retired'), 'Saved retired value must remain visible and disabled')
  assert(modelSelectionError('gemini', retiredId, 'batch'), 'Saved retired value must fail validation')
  const retired = findRetiredAsrModel('gemini', ` ${retiredId} `)
  assert(retired?.shutdownDate === '2026-05-25' && retired.replacement === 'gemini-3.1-flash-lite', 'Warning must use shared retirement evidence')

  for (const [provider, id, mode] of [
    ['gemini-live', 'gemini-3.1-flash-live-preview', 'realtime'],
    ['openai', 'whisper-1', 'batch'],
    ['openai', 'gpt-4o-transcribe', 'batch'],
    ['openai', 'gpt-4o-mini-transcribe', 'batch'],
    ['soniox', 'stt-rt-v4', 'realtime']
  ] as const) {
    const option = buildModelOptions(provider, mode, id, t).find(candidate => candidate.value === id)
    assert(option && !option.disabled, `Still-served option must remain selectable: ${id}`)
  }
  assert(!modelSelectionError('custom', retiredId, 'batch'), 'Retirement must not claim another provider’s model IDs')
  assert(!modelSelectionError('gemini', `${retiredId}-my-deployment`, 'batch'), 'Retirement must match the exact ID')
  assert(ASR_MODEL_DEFAULTS.geminiLive === 'gemini-3.5-transcribe-live', 'Gemini Live must default to the supported transcription model')
  const retiredSnapshots = [
    ['qwen', 'qwen3-asr-flash-2025-09-08', 'batch'],
    ['qwen', 'qwen3-asr-flash-2026-02-10', 'batch'],
    ['qwen', 'qwen3-asr-flash-filetrans-2025-11-17', 'batch'],
    ['qwen', 'qwen3-asr-flash-realtime-2025-10-27', 'realtime'],
    ['qwen', 'qwen3-asr-flash-realtime-2026-02-10', 'realtime'],
    ['funasr', 'fun-asr-realtime-2025-11-07', 'realtime']
  ] as const
  for (const [provider, id, mode] of retiredSnapshots) {
    assert(!buildModelOptions(provider, mode, '', t).some(option => option.value === id), 'Retired snapshots must not be presets')
    const savedSnapshot = buildModelOptions(provider, mode, id, t).find(option => option.value === id)
    assert(savedSnapshot?.disabled && savedSnapshot.label.includes('asr.modelStatus_retired'), 'Saved retired snapshot must be disabled and labeled')
    assert(modelSelectionError(provider, id, mode), 'Retired snapshots must fail validation')
    assert(findRetiredAsrModel(provider, id)?.shutdownDate === '2026-10-10', 'Retirement must show the exact official date')
    assert(!modelSelectionError('custom', id, mode), 'Custom provider models must be preserved')
    if (!id.includes('filetrans')) {
      assert(!modelSelectionError(provider, `${id}-my-deployment`, mode), 'Unknown deployment suffixes must not be retired')
    }
  }
  assert(findRetiredAsrModel('qwen', 'qwen3-asr-flash-filetrans-2025-11-17')?.note === 'modelRetiredFiletransNote', 'Unsupported asynchronous migration must explain its limitation')
  return 48
}
