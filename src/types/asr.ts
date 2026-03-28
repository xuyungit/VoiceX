export type LocalAsrStatus = {
  available: boolean
  configuredCommand: string
  resolvedPath: string | null
  ffmpegAvailable: boolean
  modelsDir: string | null
  sensevoiceInstalled: boolean
  whisperInstalled: boolean
  vadInstalled: boolean
  message: string
}
