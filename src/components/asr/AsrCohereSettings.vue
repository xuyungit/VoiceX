<script setup lang="ts">
import { computed } from 'vue'
import AsrModelSelect from './AsrModelSelect.vue'
import { NInput } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const cohereApiKey = computed({
  get: () => settingsStore.settings.cohereApiKey,
  set: (v: string) => settingsStore.updateSetting('cohereApiKey', v)
})

const cohereModel = computed({
  get: () => settingsStore.settings.cohereModel,
  set: (v: string) => settingsStore.updateSetting('cohereModel', v)
})

const cohereLanguage = computed({
  get: () => settingsStore.settings.cohereLanguage,
  set: (v: string) => settingsStore.updateSetting('cohereLanguage', v)
})
</script>

<template>
  <SettingsSection :title="t('asr.cohereConfiguration')" :help="t('asr.cohereConfigurationSub')">
    <SettingRow :label="t('asr.apiCredentials')" :help="t('asr.cohereApiKeyNote')">
      <NInput
        v-model:value="cohereApiKey"
        type="password"
        show-password-on="click"
        placeholder="Enter Cohere API key"
      />
    </SettingRow>
    <SettingRow :label="t('asr.model')" :help="t('asr.cohereModelNote')">
      <AsrModelSelect v-model:value="cohereModel" provider="cohere" mode="batch" />
    </SettingRow>
    <SettingRow :label="t('asr.languageHint')" :help="t('asr.cohereLanguageNote')">
      <NInput v-model:value="cohereLanguage" placeholder="zh" />
    </SettingRow>
  </SettingsSection>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
