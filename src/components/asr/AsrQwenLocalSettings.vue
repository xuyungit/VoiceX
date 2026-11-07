<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NInput, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'

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
  <SettingsSection :title="t('asr.qwenLocalConfiguration')" :help="t('asr.qwenLocalConfigurationSub')">
    <SettingRow
      stacked
      :label="t('asr.qwenLocalModelDir')"
      :help="`${t('asr.qwenLocalModelDirNote')} ${t('asr.localModelVerifyNote')}`"
    >
      <template #status>
        <span v-if="localModels.some((model) => model.path === qwenLocalModelDir)" class="model-path">{{ qwenLocalModelDir }}</span>
        <span v-if="selectedModel?.message || modelError" class="is-warning" role="alert">{{ selectedModel?.message || modelError }}</span>
      </template>
      <NSelect v-model:value="qwenLocalModelDir" :options="modelOptions" :loading="loadingModels"
        filterable tag size="small" :placeholder="t('asr.localModelSelect')" />
      <NButton size="small" @click="chooseDirectory">{{ t('asr.localModelChoose') }}</NButton>
      <NButton size="small" :loading="loadingModels" @click="refreshModels">{{ t('asr.localModelRefresh') }}</NButton>
    </SettingRow>
    <SettingRow :label="t('asr.qwenLocalCommandPath')" :help="t('asr.qwenLocalCommandPathNote')">
      <NInput
        v-model:value="qwenLocalCommandPath"
        placeholder="qwen-asr"
      />
    </SettingRow>
    <SettingRow :label="t('asr.qwenLocalLanguage')" :help="t('asr.qwenLocalLanguageNote')">
      <NSelect
        v-model:value="qwenLocalLanguage"
        :options="languageOptions"
        size="small"
      />
    </SettingRow>
  </SettingsSection>
</template>

<style scoped>
@import '../../styles/asr-settings.css';

.model-path { overflow-wrap: anywhere; }
</style>
