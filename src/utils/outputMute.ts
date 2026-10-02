export type OutputMuteStatus = (
  | { support: 'supported' | 'unsupported'; device: string }
  | { support: 'no_device' }
  | { support: 'error'; message: string }
) & {
  revision: number
  recording: boolean
  recoveryError: string | null
  pendingRestores: Array<{ device: string; disconnected: boolean; error: string | null }>
}
