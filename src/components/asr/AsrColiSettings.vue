<script setup lang="ts">
import { computed } from 'vue'
import { NButton, NCheckbox, NInput, NInputNumber, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import type { LocalAsrStatus } from '../../types/asr'

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
  set: (v: 'off' | 'sensevoice' | 'whisper') => settingsStore.updateSetting('coliFinalRefinementMode', v)
})

const coliRealtime = computed({
  get: () => settingsStore.settings.coliRealtime,
  set: (v: boolean) => settingsStore.updateSetting('coliRealtime', v)
})

const coliRefinementOptions = computed(() => [
  { label: t('asr.refinementOff'), value: 'off' },
  { label: t('asr.refinementSenseVoice'), value: 'sensevoice' },
  { label: t('asr.refinementWhisper'), value: 'whisper' },
])
</script>

<template>
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.localColiConfiguration') }}</div>
      <div class="card-sub">{{ t('asr.localColiConfigurationSub') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.detectionStatus') }}</div>
          <div class="field-note">
            {{ coliStatusError || coliStatus?.message || t('asr.checkingLocalColi') }}
          </div>
        </div>
        <div
          class="status-pill"
          :class="{
            online: coliStatus?.available,
            offline: !coliStatusLoading && !coliStatus?.available
          }"
        >
          {{ coliStatusLoading ? t('asr.checking') : coliStatus?.available ? t('asr.detected') : t('asr.notFound') }}
        </div>
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.commandPath') }}</div>
          <div class="field-note">{{ t('asr.commandPathNote') }}</div>
        </div>
        <div class="field-control action-control">
          <NInput
            v-model:value="coliCommandPath"
            :placeholder="t('asr.leaveEmptyToAuto')"
          />
          <NButton secondary size="small" @click="emit('refresh')">
            {{ t('asr.refresh') }}
          </NButton>
        </div>
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.resolvedPath') }}</div>
          <div class="field-note">{{ t('asr.resolvedPathNote') }}</div>
        </div>
        <div class="field-value mono">
          {{ coliStatus?.resolvedPath || '—' }}
        </div>
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.modelCache') }}</div>
          <div class="field-note">{{ t('asr.modelCacheNote') }}</div>
        </div>
        <div class="field-value mono">
          {{ coliStatus?.modelsDir || '—' }}
        </div>
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.realtimeStreaming') }}</div>
          <div class="field-note">{{ t('asr.realtimeStreamingNote') }}</div>
        </div>
        <NCheckbox v-model:checked="coliRealtime" />
      </div>
      <div v-if="coliRealtime" class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.enableVad') }}</div>
          <div class="field-note">{{ t('asr.enableVadNote') }}</div>
        </div>
        <NCheckbox v-model:checked="coliUseVad" />
      </div>
      <div v-if="coliRealtime && !coliUseVad" class="warning-box">
        {{ t('asr.warningVadOff') }}
      </div>
      <div v-if="coliRealtime && !coliUseVad" class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.streamingInterval') }}</div>
          <div class="field-note">{{ t('asr.streamingIntervalNote') }}</div>
        </div>
        <NInputNumber
          v-model:value="coliAsrIntervalMs"
          :min="200"
          :max="5000"
          :step="100"
          class="field-control short"
        />
      </div>
      <div v-if="coliRealtime" class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.finalRefinement') }}</div>
          <div class="field-note">{{ t('asr.finalRefinementNote') }}</div>
        </div>
        <NSelect
          v-model:value="coliFinalRefinementMode"
          :options="coliRefinementOptions"
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.installedModels') }}</div>
          <div class="field-note">{{ t('asr.installedModelsNote') }}</div>
        </div>
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
      </div>
    </div>
  </div>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
