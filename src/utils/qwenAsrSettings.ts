import { ASR_MODELS, type ModelMode } from './asrModels'

export const QWEN_ASR_REGIONS = [
  { value: 'beijing', labelKey: 'asr.qwenEndpointBeijing', host: 'dashscope.aliyuncs.com' },
  { value: 'singapore', labelKey: 'asr.qwenEndpointSingapore', host: 'dashscope-intl.aliyuncs.com' },
] as const

export type QwenAsrRegion = typeof QWEN_ASR_REGIONS[number]['value']

// Match the protocol predicates in funasr_client.rs and qwen_transcription_client.rs,
// using the shared catalogue for model IDs (including custom snapshot suffixes).
function isQwenAudioModel(model: string, mode: ModelMode) {
  const id = model.trim()
  return ASR_MODELS.some(entry => entry.provider === 'qwen'
    && entry.id.startsWith('qwen-audio-') && entry.modes.includes(mode)
    && id.startsWith(entry.id))
}

export const isQwenAudioStreamingModel = (model: string) => isQwenAudioModel(model, 'realtime')
export const isQwenAudioBatchModel = (model: string) =>
  isQwenAudioModel(model, 'batch') && !/streaming|filetrans|message/.test(model.trim())

export function qwenAsrRegionFromEndpoint(endpoint: string): QwenAsrRegion | 'custom' {
  return QWEN_ASR_REGIONS.find(region => ['realtime', 'inference'].some(path =>
    endpoint.trim() === `wss://${region.host}/api-ws/v1/${path}`
  ))?.value ?? 'custom'
}

export function qwenAsrPresetEndpoint(region: QwenAsrRegion, model: string) {
  const host = QWEN_ASR_REGIONS.find(entry => entry.value === region)!.host
  const path = isQwenAudioStreamingModel(model) ? 'inference' : 'realtime'
  return `wss://${host}/api-ws/v1/${path}`
}

interface QwenAsrConnectionSettings {
  recognitionMode: ModelMode
  model: string
  batchModel: string
  postRecordingRefine: boolean
  endpoint: string
  workspaceId: string
}

export function resolveQwenAsrConnection(settings: QwenAsrConnectionSettings) {
  const realtime = settings.recognitionMode === 'realtime'
  const batch = settings.recognitionMode === 'batch' || (realtime && settings.postRecordingRefine)
  const audioStreaming = realtime && isQwenAudioStreamingModel(settings.model)
  const audioBatch = batch && isQwenAudioBatchModel(settings.batchModel)
  const needsWorkspace = audioStreaming || audioBatch
  const endpoints: { mode: ModelMode; url: string }[] = []
  let parsed: URL
  try {
    parsed = new URL(settings.endpoint.trim())
  } catch {
    return { needsWorkspace, workspaceEmbedded: false, endpoints, error: 'endpoint' as const }
  }
  const workspaceEmbedded = parsed.host.endsWith('.maas.aliyuncs.com')
  if (!(realtime ? ['ws:', 'wss:'] : ['ws:', 'wss:', 'http:', 'https:']).includes(parsed.protocol)) {
    return { needsWorkspace, workspaceEmbedded, endpoints, error: 'endpoint' as const }
  }
  const workspaceId = settings.workspaceId.trim()
  const error = needsWorkspace && !workspaceEmbedded
    ? !workspaceId ? 'workspaceRequired' as const
      : !/^[a-zA-Z0-9-]+$/.test(workspaceId) ? 'workspaceInvalid' as const : null
    : null
  // The backend resolves Qwen-Audio workspace hosts from the selected region.
  // A full workspace endpoint takes precedence over a separately stored ID.
  const workspaceHost = workspaceEmbedded ? parsed.host : error ? null
    : `${workspaceId}.${parsed.host === 'dashscope-intl.aliyuncs.com' || parsed.host.includes('ap-southeast-1')
      ? 'ap-southeast-1' : 'cn-beijing'}.maas.aliyuncs.com`
  if (realtime && (!audioStreaming || workspaceHost)) {
    const base = settings.endpoint.trim().replace('/api-ws/v1/inference', '/api-ws/v1/realtime')
    endpoints.push({ mode: 'realtime', url: audioStreaming
      ? `${parsed.protocol}//${workspaceHost}/api-ws/v1/inference`
      : `${base}${base.includes('?') ? '&' : '?'}model=${encodeURIComponent(settings.model)}` })
  }
  if (batch && (!audioBatch || workspaceHost)) {
    const scheme = ['http:', 'ws:'].includes(parsed.protocol) ? 'http:' : 'https:'
    endpoints.push({ mode: 'batch', url: audioBatch
      ? `${scheme}//${workspaceHost}/api/v1/services/aigc/multimodal-generation/generation`
      : `${scheme}//${parsed.host}/compatible-mode/v1/chat/completions` })
  }
  return { needsWorkspace, workspaceEmbedded, endpoints, error }
}
