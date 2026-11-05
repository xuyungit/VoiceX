<script setup lang="ts">
import { computed } from 'vue'
import AsrModelSelect from './AsrModelSelect.vue'
import { NInput, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import { ASR_MODEL_DEFAULTS } from '../../utils/asrModels'
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const stepaudioApiKey = computed({
  get: () => settingsStore.settings.stepaudioApiKey,
  set: (v: string) => settingsStore.updateSetting('stepaudioApiKey', v)
})

const stepaudioModel = computed({
  get: () => settingsStore.settings.stepaudioModel,
  set: (v: string) => settingsStore.updateSetting('stepaudioModel', v)
})

const stepaudioBaseUrl = computed({
  get: () => settingsStore.settings.stepaudioBaseUrl,
  set: (v: string) => settingsStore.updateSetting('stepaudioBaseUrl', v)
})

const stepaudioLanguage = computed({
  get: () => settingsStore.settings.stepaudioLanguage,
  set: (v: 'auto' | 'zh' | 'en' | '') => settingsStore.updateSetting('stepaudioLanguage', v)
})

const stepaudioLanguageOptions = computed(() => [
  { label: t('asr.stepaudioLanguageAuto'), value: 'auto' },
  { label: t('asr.stepaudioLanguageZh'), value: 'zh' },
  { label: t('asr.stepaudioLanguageEn'), value: 'en' },
  { label: t('asr.stepaudioLanguageBlank'), value: '' }
])
</script>

<template>
  <SettingsSection :title="t('asr.stepaudioConfiguration')" :help="t('asr.stepaudioConfigurationSub')">
    <SettingRow :label="t('asr.apiCredentials')" :help="t('asr.stepaudioApiKeyNote')">
      <NInput
        v-model:value="stepaudioApiKey"
        type="password"
        show-password-on="click"
        :placeholder="t('asr.enterProviderApiKey', { provider: 'StepFun' })"
      />
    </SettingRow>
    <SettingRow :label="t('asr.model')" :help="t('asr.stepaudioModelNote', { model: ASR_MODEL_DEFAULTS.stepaudio })">
      <AsrModelSelect v-model:value="stepaudioModel" provider="stepaudio" mode="batch" />
    </SettingRow>
    <SettingRow :label="t('asr.baseUrl')" :help="t('asr.stepaudioBaseUrlNote')">
      <NInput
        v-model:value="stepaudioBaseUrl"
        placeholder="https://api.stepfun.com/v1"
      />
    </SettingRow>
    <SettingRow :label="t('asr.languageHint')" :help="t('asr.stepaudioLanguageNote')">
      <NSelect
        v-model:value="stepaudioLanguage"
        :options="stepaudioLanguageOptions"
        size="small"
      />
    </SettingRow>
  </SettingsSection>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
