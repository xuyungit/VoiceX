import { ASR_MODEL_DEFAULTS, buildModelOptions, findRetiredAsrModel, modelSelectionError } from './asrModels'

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message)
}

export function checkAsrModelOptions() {
  const t = (key: string) => key
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
  return 37
}
