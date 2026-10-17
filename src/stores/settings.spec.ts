import { createPinia, disposePinia } from 'pinia'
import llmModels from '../shared/llmModels.json'
import asrModels from '../shared/asrModels.json'
import { isMacOS } from '../utils/platform'
import { useSettingsStore } from './settings'
import { buildLlmProviderOptions, providerSelectionKey } from '../utils/llmOptions'

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
    if (store.settings.llmDeepseekBaseUrl !== llmModels.deepseekBaseUrl || store.settings.llmDeepseekModel !== llmModels.deepseekDefaultModel || store.settings.llmDeepseekApiKey !== '') {
      throw new Error('DeepSeek must have independent credentials and the shared official defaults')
    }
    const options = buildLlmProviderOptions(key => key)
    if (!options.some(option => !('type' in option) && option.value === 'deepseek')) {
      throw new Error('DeepSeek must be a built-in provider without any custom endpoints')
    }
    if (providerSelectionKey('deepseek', [], '') !== 'deepseek') {
      throw new Error('DeepSeek must resolve as a built-in selection')
    }
    if (store.settings.geminiLiveModel !== asrModels.defaults.geminiLive || store.settings.geminiLiveModel !== 'gemini-3.5-transcribe-live') {
      throw new Error('The settings UI must use the shared current Gemini Live default')
    }
  } finally {
    disposePinia(pinia)
  }
}
