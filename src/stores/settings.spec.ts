import { createPinia, disposePinia } from 'pinia'
import { createServer, type ViteDevServer } from 'vite'
import llmModels from '../shared/llmModels.json'
import asrModels from '../shared/asrModels.json'
import { useSettingsStore } from './settings'
import {
  buildLlmApiModeOptions,
  buildLlmProviderOptions,
  buildReasoningEffortOptions,
  providerSelectionKey
} from '../utils/llmOptions'
import { buildAsrProviderOptions } from '../utils/providerOptions'

export function checkSettingsDefaults() {
  const pinia = createPinia()
  try {
    const { settings } = useSettingsStore(pinia)
    if (settings.ttsProviderType !== 'system') {
      throw new Error('Read aloud must default to the system voice on macOS')
    }
    if (settings.llmVolcengineModel !== llmModels.volcengineDefaultModel) {
      throw new Error('The settings UI must use the shared Doubao default')
    }
    if (settings.llmOpenaiModel !== llmModels.openaiDefaultModel) {
      throw new Error('The settings UI must use the shared OpenAI default')
    }
    if (settings.llmDeepseekBaseUrl !== llmModels.deepseekBaseUrl || settings.llmDeepseekModel !== llmModels.deepseekDefaultModel || settings.llmDeepseekApiKey !== '') {
      throw new Error('DeepSeek must have independent credentials and the shared official defaults')
    }
    if (settings.llmGeminiBaseUrl !== llmModels.geminiBaseUrl || settings.llmGeminiModel !== llmModels.geminiDefaultModel || settings.llmGeminiReasoningEffort !== null) {
      throw new Error('Gemini must use the shared official defaults and auto-lowest reasoning by default')
    }
    if (settings.geminiLiveModel !== asrModels.defaults.geminiLive) {
      throw new Error('The settings UI must use the shared Gemini Live default')
    }
    if (
      settings.elevenlabsRealtimeModel !== asrModels.defaults.elevenlabsRealtime ||
      settings.elevenlabsBatchModel !== asrModels.defaults.elevenlabsBatch
    ) {
      throw new Error('The settings UI must use the shared ElevenLabs defaults')
    }
    if (settings.geminiRecognitionMode !== 'batch' || settings.geminiPostRecordingRefine !== 'off') {
      throw new Error('Gemini defaults must preserve batch mode and disable refinement')
    }
  } finally {
    disposePinia(pinia)
  }
}

export function checkProviderOptions() {
  const t = (key: string) => key
  if (!buildLlmApiModeOptions(t).some(option => option.value === 'gemini')) {
    throw new Error('Custom endpoints must offer Gemini native API mode')
  }
  const effortOptions = buildReasoningEffortOptions(t, ['low', 'medium', 'high'])
  if (effortOptions[0].value !== '' || effortOptions[1].value !== 'server_default' || effortOptions.length !== 5) {
    throw new Error('Reasoning effort options must begin with auto-lowest and server_default')
  }
  if (!buildLlmProviderOptions(t).some(option => !('type' in option) && option.value === 'deepseek')) {
    throw new Error('DeepSeek must be a built-in provider without any custom endpoints')
  }
  if (providerSelectionKey('deepseek', [], '') !== 'deepseek') {
    throw new Error('DeepSeek must resolve as a built-in selection')
  }
  const asrOptions = buildAsrProviderOptions(t)
  if (asrOptions.filter(option => option.value === 'gemini').length !== 1 || asrOptions.some(option => String(option.value) === 'gemini-live')) {
    throw new Error('Gemini must appear once in the shared ASR provider list')
  }
}

export async function checkWindowsTtsDefault(server: ViteDevServer) {
  // The platform is detected at module load, so give Windows a fresh module graph.
  const windows = await createServer(server.config.inlineConfig)
  const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, 'navigator')!
  const pinia = createPinia()
  try {
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: { platform: 'Win32', userAgent: 'Win32' }
    })
    const { useSettingsStore: useWindowsSettings } =
      (await windows.ssrLoadModule('/src/stores/settings.ts')) as typeof import('./settings')
    if (useWindowsSettings(pinia).settings.ttsProviderType !== 'edge') {
      throw new Error('Read aloud must default to Edge on Windows')
    }
  } finally {
    disposePinia(pinia)
    Object.defineProperty(globalThis, 'navigator', originalNavigator)
    await windows.close()
  }
}
