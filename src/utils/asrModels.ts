import catalog from '../shared/asrModels.json'

export type ModelMode = 'realtime' | 'batch'
export const ASR_MODEL_DEFAULTS = catalog.defaults
export const ASR_MODELS = catalog.models

export function findAsrModel(provider: string, id: string) {
  return [...ASR_MODELS].sort((a, b) => b.id.length - a.id.length).find(model =>
    (model.provider === provider || (provider.startsWith('gemini') && model.provider.startsWith('gemini'))) &&
    (model.id === id.trim() || id.trim().startsWith(model.id + '-'))
  )
}

export function modelSelectionError(provider: string, id: string, mode: ModelMode): boolean {
  const model = findAsrModel(provider, id)
  return !!model && (model.provider !== provider || !model.modes.includes(mode))
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
    options.push({ value: current, label: current + ' · ' + t('asr.modelCustom'), disabled: modelSelectionError(provider, current, mode) })
  }
  return options
}
