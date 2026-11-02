import catalog from '../shared/asrModels.json'

export type ModelMode = 'realtime' | 'batch'
export const ASR_MODEL_DEFAULTS = catalog.defaults
export const ASR_MODELS = catalog.models

export function findRetiredAsrModel(provider: string, id: string) {
  // Match only confirmed IDs, not future snapshots or user deployment names.
  return catalog.retired.find(model =>
    (model.provider === provider || (provider.startsWith('gemini') && model.provider.startsWith('gemini'))) &&
    model.id === id.trim()
  )
}

export function findAsrModel(provider: string, id: string) {
  return [...ASR_MODELS].sort((a, b) => b.id.length - a.id.length).find(model =>
    (model.provider === provider || (provider.startsWith('gemini') && model.provider.startsWith('gemini'))) &&
    (model.id === id.trim() || id.trim().startsWith(model.id + '-'))
  )
}

export function modelSelectionError(provider: string, id: string, mode: ModelMode): boolean {
  if (findRetiredAsrModel(provider, id)) return true
  const model = findAsrModel(provider, id)
  return !!model && (model.provider !== provider || !model.modes.includes(mode))
}

// Switching from the standard batch model should select the live model, which
// actually emits partial text. Keep explicit compatible and custom selections.
export function openAiModelForMode(id: string, mode: ModelMode): string {
  if (mode === 'realtime' && id === ASR_MODEL_DEFAULTS.openai) {
    return ASR_MODEL_DEFAULTS.openaiRealtime
  }
  if (modelSelectionError('openai', id, mode) || !id.trim()) {
    return mode === 'realtime' ? ASR_MODEL_DEFAULTS.openaiRealtime : ASR_MODEL_DEFAULTS.openai
  }
  return id
}

export function buildModelOptions(
  provider: string, mode: ModelMode, current: string, t: (key: string) => string
) {
  const models = ASR_MODELS.filter(model =>
    model.provider === provider && (model.modes.includes(mode) || model.id === current)
  )
  const options = models.map(model => ({
    value: model.id,
    label: [model.id, model.note && t('asr.' + model.note),
      model.status && t('asr.modelStatus_' + model.status)].filter(Boolean).join(' · '),
    disabled: !model.modes.includes(mode)
  }))
  if (current && !models.some(model => model.id === current)) {
    const retired = findRetiredAsrModel(provider, current)
    options.push({ value: current,
      label: current + ' · ' + t(retired ? 'asr.modelStatus_retired' : 'asr.modelCustom'),
      disabled: modelSelectionError(provider, current, mode) })
  }
  return options
}
