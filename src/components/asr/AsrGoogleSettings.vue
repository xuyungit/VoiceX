<script setup lang="ts">
import { computed } from 'vue'
import { NInput, NInputNumber, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const googleSttApiKey = computed({
  get: () => settingsStore.settings.googleSttApiKey,
  set: (v: string) => settingsStore.updateSetting('googleSttApiKey', v)
})

const googleSttProjectId = computed({
  get: () => settingsStore.settings.googleSttProjectId,
  set: (v: string) => settingsStore.updateSetting('googleSttProjectId', v)
})

const googleSttLocation = computed({
  get: () => settingsStore.settings.googleSttLocation,
  set: (v: string) => settingsStore.updateSetting('googleSttLocation', v)
})

const googleSttLanguageCode = computed({
  get: () => settingsStore.settings.googleSttLanguageCode,
  set: (v: string) => settingsStore.updateSetting('googleSttLanguageCode', v)
})

const googleSttEndpointing = computed({
  get: () => settingsStore.settings.googleSttEndpointing,
  set: (v: 'supershort' | 'short' | 'standard') => settingsStore.updateSetting('googleSttEndpointing', v)
})

const googleSttPhraseBoost = computed({
  get: () => settingsStore.settings.googleSttPhraseBoost,
  set: (v: number | null) => settingsStore.updateSetting('googleSttPhraseBoost', v ?? 0)
})

const googleEndpointingOptions = computed(() => [
  { label: t('asr.endpointingSupershort'), value: 'supershort' },
  { label: t('asr.endpointingShort'), value: 'short' },
  { label: t('asr.endpointingStandard'), value: 'standard' },
])

const googleLocationOptions = [
  { label: 'us (Multi-region)', value: 'us' },
  { label: 'eu (Multi-region)', value: 'eu' },
  { label: 'asia-southeast1 (Singapore)', value: 'asia-southeast1' },
  { label: 'asia-northeast1 (Tokyo)', value: 'asia-northeast1' },
  { label: 'asia-south1 (Mumbai) [Preview]', value: 'asia-south1' },
  { label: 'europe-west2 (London) [Preview]', value: 'europe-west2' },
  { label: 'europe-west3 (Frankfurt) [Preview]', value: 'europe-west3' },
  { label: 'northamerica-northeast1 (Montreal) [Preview]', value: 'northamerica-northeast1' },
]
</script>

<template>
  <SettingsSection :title="t('asr.googleCloudConfiguration')" :help="t('asr.googleCloudConfigurationSub')">
    <SettingRow stacked :label="t('asr.serviceAccountKey')" :help="t('asr.serviceAccountNote')">
      <NInput
        v-model:value="googleSttApiKey"
        type="textarea"
        placeholder='{"type":"service_account","project_id":"...","private_key":"...",...}'
        :autosize="{ minRows: 3, maxRows: 6 }"
      />
    </SettingRow>
    <SettingRow :label="t('asr.projectId')" :help="t('asr.projectIdNote')">
      <NInput v-model:value="googleSttProjectId" placeholder="e.g. my-project-123456" />
    </SettingRow>
    <SettingRow :label="t('asr.location')" :help="t('asr.locationNote')">
      <NSelect
        v-model:value="googleSttLocation"
        :options="googleLocationOptions"
        filterable
        tag
        size="small"
      />
    </SettingRow>
    <SettingRow :label="t('asr.language')" :help="t('asr.languageNote')">
      <NInput
        v-model:value="googleSttLanguageCode"
        placeholder="cmn-Hans-CN, en-US"
      />
    </SettingRow>
    <SettingRow :label="t('asr.endpointing')" :help="t('asr.endpointingNote')">
      <NSelect
        v-model:value="googleSttEndpointing"
        :options="googleEndpointingOptions"
        size="small"
      />
    </SettingRow>
    <SettingRow :label="t('asr.phraseBoost')" :help="t('asr.phraseBoostNote')">
      <NInputNumber
        v-model:value="googleSttPhraseBoost"
        :min="0"
        :max="20"
        :step="1"
        class="compact"
      />
    </SettingRow>
  </SettingsSection>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
