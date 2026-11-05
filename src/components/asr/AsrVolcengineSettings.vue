<script setup lang="ts">
import { computed } from 'vue'
import { NCheckbox, NInput, NInputNumber, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'

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
  <SettingsSection :title="t('asr.apiCredentials')">
    <SettingRow label="App key">
      <NInput v-model:value="asrAppKey" :placeholder="t('asr.enterAppKey')" />
    </SettingRow>
    <SettingRow label="Access key">
      <NInput
        v-model:value="asrAccessKey"
        type="password"
        show-password-on="click"
        :placeholder="t('asr.enterAccessKey')"
      />
    </SettingRow>
    <SettingRow label="Resource ID">
      <NInput v-model:value="asrResourceId" :placeholder="t('asr.enterResourceId')" />
    </SettingRow>
  </SettingsSection>

  <SettingsSection :title="t('asr.recognition')">
    <SettingRow :label="t('asr.recognitionMode')" :help="t('asr.volcengineRecognitionModeNote')">
      <NSelect
        v-model:value="recognitionMode"
        :options="recognitionModeOptions"
        size="small"
      />
    </SettingRow>
    <SettingRow :label="t('asr.enableSemanticSmoothing')" :help="t('asr.enableSemanticSmoothingNote')">
      <NCheckbox v-model:checked="enableDdc" />
    </SettingRow>
    <SettingRow :label="t('asr.webSocketUrl')" :help="t('asr.webSocketUrlNote')">
      <NInput
        v-model:value="asrWsUrl"
        placeholder="wss://openspeech.bytedance.com/api/v3/..."
      />
    </SettingRow>
  </SettingsSection>

  <SettingsSection :title="t('asr.endpointSettings')" :help="t('asr.endpointSettingsSub')">
    <SettingRow :label="t('asr.endWindowSize')">
      <NInputNumber
        v-model:value="endWindowSize"
        :min="0"
        :max="5000"
        :placeholder="t('asr.serviceDefault')"
        class="compact"
        clearable
      />
    </SettingRow>
    <SettingRow :label="t('asr.forceToSpeechTime')">
      <NInputNumber
        v-model:value="forceToSpeechTime"
        :min="0"
        :max="60000"
        :placeholder="t('asr.serviceDefault')"
        class="compact"
        clearable
      />
    </SettingRow>
  </SettingsSection>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
