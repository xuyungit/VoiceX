<script setup lang="ts">
import { computed } from 'vue'
import AsrModelSelect from './AsrModelSelect.vue'
import { NInput, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'

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
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.mimoConfiguration') }}</div>
      <div class="card-sub">{{ t('asr.mimoConfigurationSub') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.apiCredentials') }}</div>
          <div class="field-note">{{ t('asr.mimoApiKeyNote') }}</div>
        </div>
        <NInput
          v-model:value="mimoApiKey"
          type="password"
          show-password-on="click"
          placeholder="MiMo API key"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.model') }}</div>
          <div class="field-note">{{ t('asr.mimoModelNote') }}</div>
        </div>
        <AsrModelSelect v-model:value="mimoModel" provider="mimo" mode="batch" class="field-control" />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.baseUrl') }}</div>
          <div class="field-note">{{ t('asr.mimoBaseUrlNote') }}</div>
        </div>
        <NInput
          v-model:value="mimoBaseUrl"
          placeholder="https://api.xiaomimimo.com/v1"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.languageHint') }}</div>
          <div class="field-note">{{ t('asr.mimoLanguageNote') }}</div>
        </div>
        <NSelect
          v-model:value="mimoLanguage"
          :options="mimoLanguageOptions"
          size="small"
          class="field-control"
        />
      </div>
    </div>
  </div>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
