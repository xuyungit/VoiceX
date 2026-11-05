<script setup lang="ts">
import { computed } from 'vue'
import { isValidSonioxEndpointDelay } from '../../utils/sonioxSettings'
import AsrModelSelect from './AsrModelSelect.vue'
import { NInput, NInputNumber } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const sonioxApiKey = computed({
  get: () => settingsStore.settings.sonioxApiKey,
  set: (v: string) => settingsStore.updateSetting('sonioxApiKey', v)
})

const sonioxModel = computed({
  get: () => settingsStore.settings.sonioxModel,
  set: (v: string) => settingsStore.updateSetting('sonioxModel', v)
})

const sonioxLanguage = computed({
  get: () => settingsStore.settings.sonioxLanguage,
  set: (v: string) => settingsStore.updateSetting('sonioxLanguage', v)
})

const sonioxMaxEndpointDelayMs = computed({
  get: () => settingsStore.settings.sonioxMaxEndpointDelayMs,
  set: (v: number | null) => settingsStore.updateSetting('sonioxMaxEndpointDelayMs', v)
})
</script>

<template>
  <SettingsSection :title="t('asr.sonioxConfiguration')" :help="t('asr.sonioxConfigurationSub')">
    <SettingRow :label="t('asr.apiCredentials')" :help="t('asr.sonioxApiKeyNote')">
      <NInput
        v-model:value="sonioxApiKey"
        type="password"
        show-password-on="click"
        :placeholder="t('asr.enterProviderApiKey', { provider: 'Soniox' })"
      />
    </SettingRow>
    <SettingRow :label="t('asr.model')" :help="t('asr.sonioxModelNote')">
      <AsrModelSelect v-model:value="sonioxModel" provider="soniox" mode="realtime" />
    </SettingRow>
    <SettingRow :label="t('asr.languageHint')" :help="t('asr.sonioxLanguageNote')">
      <NInput v-model:value="sonioxLanguage" placeholder="zh, en" />
    </SettingRow>
    <SettingRow :label="t('asr.sonioxMaxEndpointDelay')" :help="t('asr.sonioxMaxEndpointDelayNote')">
      <template #status>
        <div v-if="!isValidSonioxEndpointDelay(sonioxMaxEndpointDelayMs)" class="is-warning" role="alert">
          {{ t('asr.sonioxMaxEndpointDelayInvalid', { value: sonioxMaxEndpointDelayMs }) }}
        </div>
      </template>
      <NInputNumber
        v-model:value="sonioxMaxEndpointDelayMs"
        :validator="isValidSonioxEndpointDelay"
        :status="isValidSonioxEndpointDelay(sonioxMaxEndpointDelayMs) ? undefined : 'error'"
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
