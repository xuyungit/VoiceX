<script setup lang="ts">
import { computed } from 'vue'
import { NInput } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'
const settings = useSettingsStore()
const { t } = useI18n()
const key = computed({
  get: () => settings.settings.llmDeepseekApiKey,
  set: (value: string) => settings.updateSetting('llmDeepseekApiKey', value)
})
</script>
<template>
  <SettingsSection :title="t('asr.deepseekConfiguration')" :help="t('asr.deepseekConfigurationSub')">
    <SettingRow :label="t('asr.apiCredentials')" :help="t('asr.deepseekApiKeyNote')">
      <NInput v-model:value="key" type="password" show-password-on="click" placeholder="sk-..." />
    </SettingRow>
  </SettingsSection>
</template>
<style scoped>
@import '../../styles/asr-settings.css';
</style>
