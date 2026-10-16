import { createPinia, disposePinia } from 'pinia'
import llmModels from '../shared/llmModels.json'
import asrModels from '../shared/asrModels.json'
import { isMacOS } from '../utils/platform'
import { useSettingsStore } from './settings'

export function checkLlmDefault(expectedMacOS: boolean) {
  if (isMacOS !== expectedMacOS) throw new Error('Platform fixture was not applied')
  const pinia = createPinia()
  try {
    // Inspect initial state only: no backend load, save, API call, or timer.
    const store = useSettingsStore(pinia)
    const model = store.settings.llmVolcengineModel
    if (model !== llmModels.volcengineDefaultModel || model !== 'doubao-seed-2-0-mini-260428') {
      throw new Error('The settings UI must use the shared current Doubao default')
    }
    if (store.settings.llmOpenaiModel !== llmModels.openaiDefaultModel || store.settings.llmOpenaiModel !== 'gpt-6-luna') {
      throw new Error('The settings UI must use the shared current OpenAI default')
    }
    if (store.settings.geminiLiveModel !== asrModels.defaults.geminiLive || store.settings.geminiLiveModel !== 'gemini-3.5-transcribe-live') {
      throw new Error('The settings UI must use the shared current Gemini Live default')
    }
  } finally {
    disposePinia(pinia)
  }
}
