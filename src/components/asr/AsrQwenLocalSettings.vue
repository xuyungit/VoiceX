<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NInput, NSelect, NSwitch } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const qwenLocalCommandPath = computed({
  get: () => settingsStore.settings.qwenLocalCommandPath,
  set: (v: string) => settingsStore.updateSetting('qwenLocalCommandPath', v)
})

const qwenLocalModelDir = computed({
  get: () => settingsStore.settings.qwenLocalModelDir,
  set: (v: string) => settingsStore.updateSetting('qwenLocalModelDir', v)
})

const qwenLocalLanguage = computed({
  get: () => settingsStore.settings.qwenLocalLanguage,
  set: (v: string) => settingsStore.updateSetting('qwenLocalLanguage', v)
})

const qwenLocalUseDictionary = computed({
  get: () => settingsStore.settings.qwenLocalUseDictionary,
  set: (v: boolean) => settingsStore.updateSetting('qwenLocalUseDictionary', v)
})

interface LocalModel { path: string; label: string; filesReady: boolean; message: string; configured: boolean }
const localModels = ref<LocalModel[]>([])
const modelError = ref('')
const loadingModels = ref(false)
let refreshGeneration = 0
async function refreshModels() {
  const generation = ++refreshGeneration
  loadingModels.value = true
  try {
    const result = await invoke<LocalModel[]>('list_local_qwen_models', { configured: qwenLocalModelDir.value })
    if (generation !== refreshGeneration) return
    localModels.value = result
    modelError.value = ''
  } catch (error) {
    if (generation === refreshGeneration) modelError.value = String(error)
  } finally {
    if (generation === refreshGeneration) loadingModels.value = false
  }
}
const modelOptions = computed(() => localModels.value.map(model => ({
  label: model.label + ' · ' + (model.filesReady ? t('asr.localModelFilesReady') : t('asr.localModelIncomplete')),
  value: model.path,
  disabled: !model.filesReady
})))
const selectedModel = computed(() => localModels.value.find(model => model.configured || model.path === qwenLocalModelDir.value))
async function chooseDirectory() {
  try {
    const path = await invoke<string | null>('choose_local_model_directory')
    if (path) qwenLocalModelDir.value = path
  } catch (error) { modelError.value = String(error) }
}
onMounted(refreshModels)
watch(qwenLocalModelDir, refreshModels)

// Leaving this on auto lets the model drift into English mid-utterance, so the
// blank option is deliberately labelled as not recommended rather than hidden.
const languageOptions = computed(() => [
  { label: t('asr.qwenLocalLanguageChinese'), value: 'Chinese' },
  { label: t('asr.qwenLocalLanguageEnglish'), value: 'English' },
  { label: t('asr.qwenLocalLanguageAuto'), value: '' }
])
</script>

<template>
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.qwenLocalConfiguration') }}</div>
      <div class="card-sub">{{ t('asr.qwenLocalConfigurationSub') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.qwenLocalModelDir') }}</div>
          <div class="field-note">{{ t('asr.qwenLocalModelDirNote') }}</div>
        </div>
        <div class="field-control local-model-control">
          <NSelect v-model:value="qwenLocalModelDir" :options="modelOptions" :loading="loadingModels"
            filterable tag size="small" :placeholder="t('asr.localModelSelect')" />
          <div class="local-model-actions">
            <NButton size="small" @click="chooseDirectory">{{ t('asr.localModelChoose') }}</NButton>
            <NButton size="small" :loading="loadingModels" @click="refreshModels">{{ t('asr.localModelRefresh') }}</NButton>
          </div>
          <div class="field-note">{{ qwenLocalModelDir }}</div>
          <div class="field-note">{{ t('asr.localModelVerifyNote') }}</div>
          <div v-if="selectedModel?.message || modelError" role="alert">{{ selectedModel?.message || modelError }}</div>
        </div>
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.qwenLocalCommandPath') }}</div>
          <div class="field-note">{{ t('asr.qwenLocalCommandPathNote') }}</div>
        </div>
        <NInput
          v-model:value="qwenLocalCommandPath"
          placeholder="qwen-asr"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.qwenLocalLanguage') }}</div>
          <div class="field-note">{{ t('asr.qwenLocalLanguageNote') }}</div>
        </div>
        <NSelect
          v-model:value="qwenLocalLanguage"
          :options="languageOptions"
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.qwenLocalUseDictionary') }}</div>
          <div class="field-note">{{ t('asr.qwenLocalUseDictionaryNote') }}</div>
        </div>
        <NSwitch v-model:value="qwenLocalUseDictionary" />
      </div>
    </div>
  </div>
</template>

<style scoped>
.local-model-control { min-width: 0; display: grid; gap: 6px; }
.local-model-actions { display: flex; gap: 6px; }
.field-note { overflow-wrap: anywhere; }
</style>
