<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { NButton, NSelect, NProgress, NDropdown, useDialog } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import type { LocalModelsStatus, ModelDownloadSource } from '../../types/localModels'
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'

const settingsStore = useSettingsStore()
const { t } = useI18n()
const dialog = useDialog()
const status = ref<LocalModelsStatus | null>(null)
const error = ref('')
const loading = ref(true)
const busy = ref(false)
const source = ref<ModelDownloadSource>('modelscope')
const selectedId = ref(settingsStore.settings.qwenLocalModel)
let disposed = false
let generation = 0
let unlisten: UnlistenFn | undefined

const model = computed(() => status.value?.models.find(item => item.id === selectedId.value))
const activePhases = ['downloading', 'verifying', 'pausing', 'waitingForIdle']
const downloading = computed(() => !!model.value && activePhases.includes(model.value.phase))
const anyDownload = computed(() => status.value?.models.some(item => activePhases.includes(item.phase)) ?? false)
const current = computed(() => settingsStore.settings.qwenLocalModel === selectedId.value)
const engineMissingMessage = computed(() => t(import.meta.env.DEV ? 'asr.managedDevEngineMissing' : 'asr.managedEngineMissing'))
const progress = computed(() => model.value ? Math.min(100, model.value.downloadedBytes / model.value.downloadBytes * 100) : 0)
const options = computed(() => status.value?.models.map((item, index) => ({
  label: item.label + (index === 0 ? ` · ${t('asr.managedRecommended')}` : ''), value: item.id
})) ?? [])
const sourceOptions = computed(() => [
  { label: 'ModelScope（魔搭）', value: 'modelscope' },
  { label: 'Hugging Face', value: 'huggingface' }
])
const moreOptions = computed(() => [
  { key: 'redownload', label: t('asr.managedRedownload'), disabled: anyDownload.value || busy.value },
  { key: 'clear', label: t('asr.managedClearDownload'), disabled: anyDownload.value || busy.value || !model.value?.downloadedBytes },
  { key: 'delete', label: t('asr.managedDelete'), disabled: anyDownload.value || busy.value || model.value?.inUse || !model.value?.diskBytes }
])
const qwenLocalLanguage = computed({
  get: () => settingsStore.settings.qwenLocalLanguage,
  set: (v: string) => settingsStore.updateSetting('qwenLocalLanguage', v)
})
const languageOptions = computed(() => [
  { label: t('asr.qwenLocalLanguageChinese'), value: 'Chinese' },
  { label: t('asr.qwenLocalLanguageEnglish'), value: 'English' },
  { label: t('asr.qwenLocalLanguageAuto'), value: '' }
])
function bytes(value: number) {
  if (value >= 1e9) return `${(value / 1e9).toFixed(2)} GB`
  return `${(value / 1e6).toFixed(1)} MB`
}
function message(reason: unknown) {
  const code = String(reason)
  if (code === 'bundled_engine_missing') return engineMissingMessage.value
  const known: Record<string, string> = {
    model_in_use: 'asr.managedInUse', download_in_progress: 'asr.managedOtherDownload',
    unsupported_platform: 'asr.managedUnsupported',
    model_not_installed: 'asr.managedNotInstalled', insufficient_space: 'asr.managedInsufficientSpace'
  }
  return known[code] ? t(known[code]) : code
}
async function refresh() {
  const request = ++generation
  try {
    const next = await invoke<LocalModelsStatus>('get_local_model_status')
    if (disposed || request !== generation) return
    status.value = next
    error.value = ''
  } catch (reason) {
    if (!disposed && request === generation) error.value = message(reason)
  } finally {
    if (!disposed) loading.value = false
  }
}
async function run(command: string, args: Record<string, unknown>) {
  busy.value = true
  error.value = ''
  try {
    await invoke(command, args)
    await refresh()
    return true
  } catch (reason) {
    error.value = message(reason)
    return false
  } finally { busy.value = false }
}
function download(restart = false) {
  return run('download_local_model', { id: selectedId.value, source: source.value, restart })
}
function useModel() { void settingsStore.updateSetting('qwenLocalModel', selectedId.value) }
function onMore(key: string | number) {
  if (key === 'redownload') void download(true)
  if (key === 'clear') void run('delete_local_model', { id: selectedId.value, partialOnly: true })
  if (key === 'delete') {
    const id = selectedId.value
    dialog.warning({
      title: t('asr.managedDelete'),
      content: t('asr.managedDeleteConfirm', { model: model.value?.label, size: bytes(model.value?.diskBytes ?? 0) }),
      positiveText: t('common.delete'), negativeText: t('common.cancel'),
      onPositiveClick: () => run('delete_local_model', { id, partialOnly: false })
    })
  }
}
watch(() => settingsStore.settings.qwenLocalModel, value => { selectedId.value = value })
onMounted(async () => {
  try {
    const stop = await listen<LocalModelsStatus>('local-models:updated', event => {
      if (disposed) return
      generation++
      status.value = event.payload
      loading.value = false
    })
    if (disposed) { stop(); return }
    unlisten = stop
    await refresh()
    source.value = status.value?.source ?? 'modelscope'
  } catch (reason) {
    if (!disposed) { error.value = message(reason); loading.value = false }
  }
})
onBeforeUnmount(() => { disposed = true; generation++; unlisten?.() })
</script>

<template>
  <SettingsSection :title="t('asr.qwenLocalConfiguration')" :help="t('asr.qwenLocalConfigurationSub')">
    <p v-if="status && !status.supported" class="model-notice">{{ t('asr.managedUnsupported') }}</p>
    <template v-if="status?.supported">
      <SettingRow :label="t('asr.managedModel')" :help="t('asr.managedModelHelp')">
        <NSelect v-model:value="selectedId" :options="options" :loading="loading" size="small" />
      </SettingRow>
      <SettingRow :label="t('asr.managedSource')" :help="t('asr.managedSourceHelp')">
        <NSelect v-model:value="source" :options="sourceOptions" :disabled="anyDownload" size="small" />
      </SettingRow>
      <SettingRow :label="t('asr.managedStatus')" :help="t('asr.managedStatusHelp')" :stacked="downloading">
        <template #status>
          <span v-if="model" aria-live="polite">{{ t(`asr.managedPhase.${model.phase}`) }}</span>
          <span v-if="model">{{ t('asr.managedDownloadSize', { size: bytes(model.downloadBytes) }) }}<template v-if="model.diskBytes"> · {{ t('asr.managedDiskSize', { size: bytes(model.diskBytes) }) }}</template></span>
          <span v-if="!status.engineAvailable" class="is-warning">{{ engineMissingMessage }}</span>
          <span v-if="model?.error" class="is-error" role="alert">{{ message(model.error) }}</span>
        </template>
        <div v-if="downloading && model" class="download-progress">
          <NProgress type="line" :percentage="progress" :show-indicator="false" :processing="model.phase === 'downloading'" />
          <div class="download-details"><span>{{ bytes(model.downloadedBytes) }} / {{ bytes(model.downloadBytes) }}</span><span v-if="model.speedBytesPerSecond">{{ bytes(model.speedBytesPerSecond) }}/s</span></div>
        </div>
        <NButton v-if="downloading" size="small" :loading="busy || model?.phase === 'pausing'" @click="run('pause_local_model_download', { id: selectedId })">{{ t('asr.managedPause') }}</NButton>
        <template v-else>
          <NButton v-if="model?.installed" size="small" :type="current ? 'default' : 'primary'" :disabled="current || busy" @click="useModel">{{ current ? t('asr.managedCurrent') : t('asr.managedUse') }}</NButton>
          <NButton v-else size="small" type="primary" :loading="busy" :disabled="!status.engineAvailable || anyDownload" @click="download()">{{ model?.downloadedBytes || model?.phase === 'paused' ? t('asr.managedResume') : t('asr.managedDownload') }}</NButton>
          <NButton v-if="model?.installed && model.downloadedBytes" size="small" :disabled="busy || anyDownload" @click="download()">{{ t('asr.managedResume') }}</NButton>
          <NDropdown :options="moreOptions" @select="onMore"><NButton size="small" :disabled="busy">{{ t('asr.managedMore') }}</NButton></NDropdown>
        </template>
      </SettingRow>
    </template>
    <SettingRow v-if="status?.supported" :label="t('asr.qwenLocalLanguage')" :help="t('asr.qwenLocalLanguageNote')">
      <NSelect v-model:value="qwenLocalLanguage" :options="languageOptions" size="small" />
    </SettingRow>
    <SettingRow v-if="status?.supported" :label="t('asr.managedEngine')" :help="t('asr.managedEngineHelp')">
      <span class="engine-version">{{ status.engineVersion }}</span>
    </SettingRow>
    <p v-if="error" class="model-notice is-error" role="alert">{{ error }} <NButton text size="tiny" @click="refresh">{{ t('asr.localModelRefresh') }}</NButton></p>
  </SettingsSection>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
.model-notice { padding: 10px 0; color: var(--color-text-secondary); font-size: var(--font-sm); }
.is-error { color: var(--color-error); }
.download-progress { flex: 1; min-width: 180px; }
.download-details { display: flex; justify-content: space-between; margin-top: 4px; font-size: var(--font-xs); color: var(--color-text-tertiary); }
.engine-version { font-size: var(--font-sm); color: var(--color-text-secondary); }
</style>
