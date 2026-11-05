<script setup lang="ts">
import { computed } from 'vue'
import AsrModelSelect from './AsrModelSelect.vue'
import { NInput, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const mimoApiKey = computed({
  get: () => settingsStore.settings.mimoApiKey,
  set: (v: string) => settingsStore.updateSetting('mimoApiKey', v)
})

const mimoModel = computed({
  get: () => settingsStore.settings.mimoModel,
  set: (v: string) => settingsStore.updateSetting('mimoModel', v)
})

const mimoBaseUrl = computed({
  get: () => settingsStore.settings.mimoBaseUrl,
  set: (v: string) => settingsStore.updateSetting('mimoBaseUrl', v)
})

const mimoLanguage = computed({
  get: () => settingsStore.settings.mimoLanguage,
  set: (v: 'auto' | 'zh' | 'en' | '') => settingsStore.updateSetting('mimoLanguage', v)
})

const mimoLanguageOptions = computed(() => [
  { label: t('asr.mimoLanguageAuto'), value: 'auto' },
  { label: t('asr.mimoLanguageZh'), value: 'zh' },
  { label: t('asr.mimoLanguageEn'), value: 'en' }
])
</script>

<template>
  <SettingsSection :title="t('asr.mimoConfiguration')" :help="t('asr.mimoConfigurationSub')">
    <SettingRow :label="t('asr.apiCredentials')" :help="t('asr.mimoApiKeyNote')">
      <NInput
        v-model:value="mimoApiKey"
        type="password"
        show-password-on="click"
        placeholder="MiMo API key"
      />
    </SettingRow>
    <SettingRow :label="t('asr.model')" :help="t('asr.mimoModelNote')">
      <AsrModelSelect v-model:value="mimoModel" provider="mimo" mode="batch" />
    </SettingRow>
    <SettingRow :label="t('asr.baseUrl')" :help="t('asr.mimoBaseUrlNote')">
      <NInput
        v-model:value="mimoBaseUrl"
        placeholder="https://api.xiaomimimo.com/v1"
      />
    </SettingRow>
    <SettingRow :label="t('asr.languageHint')" :help="t('asr.mimoLanguageNote')">
      <NSelect
        v-model:value="mimoLanguage"
        :options="mimoLanguageOptions"
        size="small"
      />
    </SettingRow>
  </SettingsSection>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
