<script setup lang="ts">
import { computed } from 'vue'
import { NCheckbox, NInput, NInputNumber, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const asrAppKey = computed({
  get: () => settingsStore.settings.asrAppKey,
  set: (v: string) => settingsStore.updateSetting('asrAppKey', v)
})

const asrAccessKey = computed({
  get: () => settingsStore.settings.asrAccessKey,
  set: (v: string) => settingsStore.updateSetting('asrAccessKey', v)
})

const asrResourceId = computed({
  get: () => settingsStore.settings.asrResourceId,
  set: (v: string) => settingsStore.updateSetting('asrResourceId', v)
})

const asrWsUrl = computed({
  get: () => settingsStore.settings.asrWsUrl,
  set: (v: string) => settingsStore.updateSetting('asrWsUrl', v)
})

const recognitionModeOptions = computed(() => [
  { label: t('asr.recognitionModeRealtime'), value: 'realtime_async' },
  { label: t('asr.recognitionModeRealtimeTwoPass'), value: 'realtime_two_pass' },
  { label: t('asr.recognitionModeNostream'), value: 'nostream' }
])

const recognitionMode = computed({
  get: () => {
    if (settingsStore.settings.asrWsUrl.includes('nostream')) {
      return 'nostream'
    }
    if (settingsStore.settings.enableNonstream) {
      return 'realtime_two_pass'
    }
    return 'realtime_async'
  },
  set: (v: string) => {
    const isNostream = v === 'nostream'
    const url = isNostream
      ? 'wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_nostream'
      : 'wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async'
    settingsStore.updateSetting('asrWsUrl', url)
    settingsStore.updateSetting('enableNonstream', v === 'realtime_two_pass')
  }
})

const enableDdc = computed({
  get: () => settingsStore.settings.enableDdc,
  set: (v: boolean) => settingsStore.updateSetting('enableDdc', v)
})

const endWindowSize = computed({
  get: () => settingsStore.settings.endWindowSize,
  set: (v: number | null) => settingsStore.updateSetting('endWindowSize', v)
})

const forceToSpeechTime = computed({
  get: () => settingsStore.settings.forceToSpeechTime,
  set: (v: number | null) => settingsStore.updateSetting('forceToSpeechTime', v)
})
</script>

<template>
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.apiCredentials') }}</div>
      <div class="card-sub">Volcengine 豆包 ASR 服务的访问凭证</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">App Key</div>
        </div>
        <NInput v-model:value="asrAppKey" placeholder="Enter App Key" class="field-control" />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">Access Key</div>
        </div>
        <NInput
          v-model:value="asrAccessKey"
          type="password"
          show-password-on="click"
          placeholder="Enter Access Key"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">Resource ID</div>
        </div>
        <NInput v-model:value="asrResourceId" placeholder="Enter Resource ID" class="field-control" />
      </div>
    </div>
  </div>

  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.recognition') }}</div>
      <div class="card-sub">{{ t('asr.recognitionSub') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.recognitionMode') }}</div>
          <div class="field-note">{{ t('asr.volcengineRecognitionModeNote') }}</div>
        </div>
        <NSelect
          v-model:value="recognitionMode"
          :options="recognitionModeOptions"
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.enableSemanticSmoothing') }}</div>
          <div class="field-note">{{ t('asr.enableSemanticSmoothingNote') }}</div>
        </div>
        <NCheckbox v-model:checked="enableDdc" />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.webSocketUrl') }}</div>
          <div class="field-note">{{ t('asr.webSocketUrlNote') }}</div>
        </div>
        <NInput
          v-model:value="asrWsUrl"
          placeholder="wss://openspeech.bytedance.com/api/v3/..."
          class="field-control"
        />
      </div>
    </div>
  </div>

  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.endpointSettings') }}</div>
      <div class="card-sub">{{ t('asr.endpointSettingsSub') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.endWindowSize') }}</div>
          <div class="field-note">{{ t('asr.useServiceDefault') }}</div>
        </div>
        <NInputNumber
          v-model:value="endWindowSize"
          :min="0"
          :max="5000"
          :placeholder="t('asr.serviceDefault')"
          class="field-control short"
          clearable
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.forceToSpeechTime') }}</div>
          <div class="field-note">{{ t('asr.useServiceDefault') }}</div>
        </div>
        <NInputNumber
          v-model:value="forceToSpeechTime"
          :min="0"
          :max="60000"
          :placeholder="t('asr.serviceDefault')"
          class="field-control short"
          clearable
        />
      </div>
    </div>
  </div>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
