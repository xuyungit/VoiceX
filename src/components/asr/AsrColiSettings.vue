<script setup lang="ts">
import { computed } from 'vue'
import { NButton, NCheckbox, NInput, NInputNumber, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import type { LocalAsrStatus } from '../../types/asr'
import {
  buildColiLanguageOptions,
  buildColiPostRecordingRefineOptions,
  buildColiRecognitionModeOptions,
  normalizeColiPostRecordingRefine,
  type ColiLanguage,
} from '../../utils/providerOptions'
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'

defineProps<{
  coliStatus: LocalAsrStatus | null
  coliStatusLoading: boolean
  coliStatusError: string
}>()

const emit = defineEmits<{
  refresh: []
}>()

const settingsStore = useSettingsStore()
const { t } = useI18n()

const coliCommandPath = computed({
  get: () => settingsStore.settings.coliCommandPath,
  set: (v: string) => settingsStore.updateSetting('coliCommandPath', v)
})

const coliSensevoiceModelPath = computed({
  get: () => settingsStore.settings.coliSensevoiceModelPath,
  set: (v: string) => settingsStore.updateSetting('coliSensevoiceModelPath', v)
})

const coliLanguage = computed({
  get: () => settingsStore.settings.coliLanguage,
  set: (v: ColiLanguage) => settingsStore.updateSetting('coliLanguage', v)
})

const coliUseVad = computed({
  get: () => settingsStore.settings.coliUseVad,
  set: (v: boolean) => settingsStore.updateSetting('coliUseVad', v)
})

const coliAsrIntervalMs = computed({
  get: () => settingsStore.settings.coliAsrIntervalMs,
  set: (v: number | null) => settingsStore.updateSetting('coliAsrIntervalMs', v ?? 1000)
})

const coliFinalRefinementMode = computed({
  get: () => settingsStore.settings.coliFinalRefinementMode,
  set: (v: 'off' | 'sensevoice' | 'whisper') => settingsStore.updateSetting(
    'coliFinalRefinementMode',
    normalizeColiPostRecordingRefine(coliRecognitionMode.value, v)
  )
})

const coliRecognitionMode = computed({
  get: (): 'realtime' | 'batch' => settingsStore.settings.coliRealtime ? 'realtime' : 'batch',
  set: (value: 'realtime' | 'batch') => {
    settingsStore.updateSetting('coliRealtime', value === 'realtime')
    settingsStore.updateSetting(
      'coliFinalRefinementMode',
      normalizeColiPostRecordingRefine(value, settingsStore.settings.coliFinalRefinementMode)
    )
  }
})

const coliRecognitionModeOptions = computed(() => buildColiRecognitionModeOptions(t))
const coliLanguageOptions = computed(() => buildColiLanguageOptions(t))
const coliRefinementOptions = computed(() => buildColiPostRecordingRefineOptions(t))
const batchRefineDisabled = computed(() => coliRecognitionMode.value === 'batch')
</script>

<template>
  <SettingsSection :title="t('asr.localColiConfiguration')" :help="t('asr.localColiConfigurationSub')">
    <SettingRow :label="t('asr.detectionStatus')">
      <template #status>
        <div>
          {{ coliStatusError || coliStatus?.message || t('asr.checkingLocalColi') }}
        </div>
      </template>
      <div
        class="status-pill"
        :class="{
          online: coliStatus?.available,
          offline: !coliStatusLoading && !coliStatus?.available
        }"
      >
        {{ coliStatusLoading ? t('asr.checking') : coliStatus?.available ? t('asr.detected') : t('asr.notFound') }}
      </div>
    </SettingRow>
    <SettingRow :label="t('asr.commandPath')" :help="t('asr.commandPathNote')">
      <div class="control-group">
        <NInput
          v-model:value="coliCommandPath"
          :placeholder="t('asr.leaveEmptyToAuto')"
        />
        <NButton secondary size="small" @click="emit('refresh')">
          {{ t('asr.refresh') }}
        </NButton>
      </div>
    </SettingRow>
    <SettingRow :label="t('asr.resolvedPath')" :help="t('asr.resolvedPathNote')">
      <div class="field-value mono">
        {{ coliStatus?.resolvedPath || '—' }}
      </div>
    </SettingRow>
    <SettingRow :label="t('asr.modelCache')" :help="t('asr.modelCacheNote')">
      <div class="field-value mono">
        {{ coliStatus?.modelsDir || '—' }}
      </div>
    </SettingRow>
    <SettingRow :label="t('asr.coliSensevoiceModelPath')" :help="t('asr.coliSensevoiceModelPathNote')">
      <NInput
        v-model:value="coliSensevoiceModelPath"
        :placeholder="t('asr.coliSensevoiceModelPathPlaceholder')"
      />
    </SettingRow>
    <SettingRow :label="t('asr.language')" :help="t('asr.coliLanguageNote')">
      <NSelect v-model:value="coliLanguage" :options="coliLanguageOptions" size="small" />
    </SettingRow>
    <SettingRow :label="t('asr.recognitionMode')" :help="t('asr.coliRecognitionModeNote')">
      <NSelect
        v-model:value="coliRecognitionMode"
        :options="coliRecognitionModeOptions"
        size="small"
      />
    </SettingRow>
    <SettingRow v-if="coliRecognitionMode === 'realtime'" :label="t('asr.enableVad')" :help="t('asr.enableVadNote')">
      <NCheckbox v-model:checked="coliUseVad" />
    </SettingRow>
    <div v-if="coliRecognitionMode === 'realtime' && !coliUseVad" class="settings-callout">
      {{ t('asr.warningVadOff') }}
    </div>
    <SettingRow v-if="coliRecognitionMode === 'realtime' && !coliUseVad" :label="t('asr.streamingInterval')" :help="t('asr.streamingIntervalNote')">
      <NInputNumber
        v-model:value="coliAsrIntervalMs"
        :min="200"
        :max="5000"
        :step="100"
        class="compact"
      />
    </SettingRow>
    <SettingRow :label="t('asr.postRecordingRefine')" :help="t('asr.coliPostRecordingRefineNote')">
      <NSelect
        v-model:value="coliFinalRefinementMode"
        :options="coliRefinementOptions"
        :disabled="batchRefineDisabled"
        size="small"
      />
    </SettingRow>
    <SettingRow :label="t('asr.installedModels')" :help="t('asr.installedModelsNote')">
      <div class="pill-group">
        <span class="mini-pill" :class="{ ready: coliStatus?.sensevoiceInstalled }">
          {{ coliStatus?.sensevoiceInstalled ? t('asr.senseVoiceReady') : t('asr.senseVoicePending') }}
        </span>
        <span class="mini-pill" :class="{ ready: coliStatus?.whisperInstalled }">
          {{ coliStatus?.whisperInstalled ? t('asr.whisperReady') : t('asr.whisperPending') }}
        </span>
        <span class="mini-pill" :class="{ ready: coliStatus?.vadInstalled }">
          {{ coliStatus?.vadInstalled ? t('asr.sileroReady') : t('asr.sileroPending') }}
        </span>
        <span class="mini-pill" :class="{ ready: coliStatus?.ffmpegAvailable }">
          {{ coliStatus?.ffmpegAvailable ? t('asr.ffmpegReady') : t('asr.ffmpegMissing') }}
        </span>
      </div>
    </SettingRow>
  </SettingsSection>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
