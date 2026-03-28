<script setup lang="ts">
import { computed } from 'vue'
import { NInput } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'

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
</script>

<template>
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.sonioxConfiguration') }}</div>
      <div class="card-sub">{{ t('asr.sonioxConfigurationSub') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.apiCredentials') }}</div>
          <div class="field-note">{{ t('asr.sonioxApiKeyNote') }}</div>
        </div>
        <NInput
          v-model:value="sonioxApiKey"
          type="password"
          show-password-on="click"
          placeholder="Enter Soniox API key"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.model') }}</div>
          <div class="field-note">{{ t('asr.sonioxModelNote') }}</div>
        </div>
        <NInput v-model:value="sonioxModel" placeholder="stt-rt-v4" class="field-control" />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.languageHint') }}</div>
          <div class="field-note">{{ t('asr.sonioxLanguageNote') }}</div>
        </div>
        <NInput v-model:value="sonioxLanguage" placeholder="en" class="field-control" />
      </div>
    </div>
  </div>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
