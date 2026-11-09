export type ModelDownloadSource = 'modelscope' | 'huggingface'
export type LocalModelPhase = 'missing' | 'ready' | 'downloading' | 'pausing' | 'paused' | 'verifying' | 'waitingForIdle' | 'error'
export interface ManagedModelStatus {
  id: string
  label: string
  phase: LocalModelPhase
  installed: boolean
  downloadBytes: number
  downloadedBytes: number
  diskBytes: number
  speedBytesPerSecond: number
  inUse: boolean
  error: string | null
}
export interface LocalModelsStatus {
  supported: boolean
  platform: string
  architecture: string
  engineAvailable: boolean
  engineVersion: string
  source: ModelDownloadSource
  models: ManagedModelStatus[]
}
