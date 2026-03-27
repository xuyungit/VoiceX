<script setup lang="ts">
import { computed, ref } from 'vue'
import { NInput, NSwitch, NButton, NSelect, NTabs, NTabPane } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import type { ResolvedLocale } from '../i18n'
import { useSettingsStore, type AppSettings } from '../stores/settings'
import { getDefaultPrompt } from '../utils/llmPrompts'

const settingsStore = useSettingsStore()
const { t, locale } = useI18n()

const providerOptions = computed(() => [
  { label: t('llm.providerVolcengine'), value: 'volcengine' },
  { label: t('llm.providerOpenAI'), value: 'openai' },
  { label: t('llm.providerQwen'), value: 'qwen' },
  { label: t('llm.providerCustom'), value: 'custom' }
])

const reasoningEffortOptions = computed(() => [
  { label: t('llm.low'), value: 'low' },
  { label: t('llm.minimalDefault'), value: 'minimal' },
  { label: t('llm.medium'), value: 'medium' },
  { label: t('llm.high'), value: 'high' }
])

// Common settings
const enableLlmCorrection = computed({
  get: () => settingsStore.settings.enableLlmCorrection,
  set: (v: boolean) => settingsStore.updateSetting('enableLlmCorrection', v)
})

const llmProviderType = computed({
  get: () => settingsStore.settings.llmProviderType,
  set: (v: AppSettings['llmProviderType']) => settingsStore.updateSetting('llmProviderType', v)
})

const llmPromptTemplate = computed({
  get: () => settingsStore.settings.llmPromptTemplate,
  set: (v: string) => settingsStore.updateSetting('llmPromptTemplate', v)
})

const translationPromptTemplate = computed({
  get: () => settingsStore.settings.translationPromptTemplate,
  set: (v: string) => settingsStore.updateSetting('translationPromptTemplate', v)
})

const enableLlmHistoryContext = computed({
  get: () => settingsStore.settings.enableLlmHistoryContext,
  set: (v: boolean) => settingsStore.updateSetting('enableLlmHistoryContext', v)
})

const translationEnabled = computed({
  get: () => settingsStore.settings.translationEnabled,
  set: (v: boolean) => settingsStore.updateSetting('translationEnabled', v)
})

// Volcengine-specific
const llmVolcengineBaseUrl = computed({
  get: () => settingsStore.settings.llmVolcengineBaseUrl,
  set: (v: string) => settingsStore.updateSetting('llmVolcengineBaseUrl', v)
})
const llmVolcengineApiKey = computed({
  get: () => settingsStore.settings.llmVolcengineApiKey,
  set: (v: string) => settingsStore.updateSetting('llmVolcengineApiKey', v)
})
const llmVolcengineModel = computed({
  get: () => settingsStore.settings.llmVolcengineModel,
  set: (v: string) => settingsStore.updateSetting('llmVolcengineModel', v)
})
const llmVolcengineReasoningEffort = computed({
  get: () => settingsStore.settings.llmVolcengineReasoningEffort ?? 'minimal',
  set: (v: string) => settingsStore.updateSetting('llmVolcengineReasoningEffort', v)
})

// OpenAI-specific
const llmOpenaiBaseUrl = computed({
  get: () => settingsStore.settings.llmOpenaiBaseUrl,
  set: (v: string) => settingsStore.updateSetting('llmOpenaiBaseUrl', v)
})
const llmOpenaiApiKey = computed({
  get: () => settingsStore.settings.llmOpenaiApiKey,
  set: (v: string) => settingsStore.updateSetting('llmOpenaiApiKey', v)
})
const llmOpenaiModel = computed({
  get: () => settingsStore.settings.llmOpenaiModel,
  set: (v: string) => settingsStore.updateSetting('llmOpenaiModel', v)
})

// Qwen-specific
const llmQwenBaseUrl = computed({
  get: () => settingsStore.settings.llmQwenBaseUrl,
  set: (v: string) => settingsStore.updateSetting('llmQwenBaseUrl', v)
})
const llmQwenApiKey = computed({
  get: () => settingsStore.settings.llmQwenApiKey,
  set: (v: string) => settingsStore.updateSetting('llmQwenApiKey', v)
})
const llmQwenModel = computed({
  get: () => settingsStore.settings.llmQwenModel,
  set: (v: string) => settingsStore.updateSetting('llmQwenModel', v)
})

// Custom-specific
const llmCustomBaseUrl = computed({
  get: () => settingsStore.settings.llmCustomBaseUrl,
  set: (v: string) => settingsStore.updateSetting('llmCustomBaseUrl', v)
})
const llmCustomApiKey = computed({
  get: () => settingsStore.settings.llmCustomApiKey,
  set: (v: string) => settingsStore.updateSetting('llmCustomApiKey', v)
})
const llmCustomModel = computed({
  get: () => settingsStore.settings.llmCustomModel,
  set: (v: string) => settingsStore.updateSetting('llmCustomModel', v)
})

const isVolcengine = computed(() => llmProviderType.value === 'volcengine')
const isOpenai = computed(() => llmProviderType.value === 'openai')
const isQwen = computed(() => llmProviderType.value === 'qwen')
const isCustom = computed(() => llmProviderType.value === 'custom')
const activePromptTab = ref<'assistant' | 'translation'>('assistant')

const resolvedLocale = computed<ResolvedLocale>(() => {
  return locale.value === 'zh-CN' ? 'zh-CN' : 'en-US'
})

function resetPrompt() {
  if (activePromptTab.value === 'assistant') {
    llmPromptTemplate.value = getDefaultPrompt('assistant', resolvedLocale.value)
    return
  }
  translationPromptTemplate.value = getDefaultPrompt('translation', resolvedLocale.value)
}
</script>

<template>
  <div class="page settings-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('llm.title') }}</h1>
    </div>

    <div class="surface-card llm-card">
      <div class="card-header">
        <div class="card-title">{{ t('llm.aiCorrection') }}</div>
        <div class="card-sub">{{ t('llm.aiCorrectionSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('llm.enableAiCorrection') }}</div>
          </div>
          <NSwitch v-model:value="enableLlmCorrection" />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('llm.includeRecentInputHistory') }}</div>
            <div class="field-sub">{{ t('llm.includeRecentInputHistorySub') }}</div>
          </div>
          <NSwitch v-model:value="enableLlmHistoryContext" />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('llm.enableTranslationMode') }}</div>
            <div class="field-sub">{{ t('llm.enableTranslationModeSub') }}</div>
          </div>
          <NSwitch v-model:value="translationEnabled" />
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('llm.provider') }}</div>
            <div class="field-sub">{{ t('llm.providerSub') }}</div>
          </div>
          <NSelect
            v-model:value="llmProviderType"
            :options="providerOptions"
            class="field-control short"
          />
        </div>

        <!-- Volcengine Settings -->
        <template v-if="isVolcengine">
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('llm.baseUrl') }}</div>
            </div>
            <NInput v-model:value="llmVolcengineBaseUrl" class="field-control" />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('llm.apiKey') }}</div>
            </div>
            <NInput
              v-model:value="llmVolcengineApiKey"
              type="password"
              show-password-on="click"
              :placeholder="t('llm.enterApiKey')"
              class="field-control"
            />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('llm.modelName') }}</div>
            </div>
            <NInput v-model:value="llmVolcengineModel" class="field-control short" />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('llm.reasoningEffort') }}</div>
              <div class="field-sub">{{ t('llm.reasoningEffortSub') }}</div>
            </div>
            <NSelect
              v-model:value="llmVolcengineReasoningEffort"
              :options="reasoningEffortOptions"
              class="field-control short"
            />
          </div>
        </template>

        <!-- OpenAI Settings -->
        <template v-if="isOpenai">
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('llm.baseUrl') }}</div>
            </div>
            <NInput v-model:value="llmOpenaiBaseUrl" class="field-control" />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('llm.apiKey') }}</div>
            </div>
            <NInput
              v-model:value="llmOpenaiApiKey"
              type="password"
              show-password-on="click"
              placeholder="sk-..."
              class="field-control"
            />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('llm.modelName') }}</div>
            </div>
            <NInput v-model:value="llmOpenaiModel" class="field-control short" />
          </div>
        </template>

        <!-- Qwen Settings -->
        <template v-if="isQwen">
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('llm.baseUrl') }}</div>
            </div>
            <NInput v-model:value="llmQwenBaseUrl" class="field-control" />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('llm.apiKey') }}</div>
            </div>
            <NInput
              v-model:value="llmQwenApiKey"
              type="password"
              show-password-on="click"
              placeholder="sk-..."
              class="field-control"
            />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('llm.modelName') }}</div>
            </div>
            <NInput v-model:value="llmQwenModel" class="field-control short" />
          </div>
        </template>

        <!-- Custom Settings -->
        <template v-if="isCustom">
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('llm.baseUrl') }}</div>
              <div class="field-sub">{{ t('llm.customBaseUrlSub') }}</div>
            </div>
            <NInput v-model:value="llmCustomBaseUrl" placeholder="https://your-api.example.com/v1" class="field-control" />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('llm.apiKey') }}</div>
            </div>
            <NInput
              v-model:value="llmCustomApiKey"
              type="password"
              show-password-on="click"
              :placeholder="t('llm.enterApiKey')"
              class="field-control"
            />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('llm.modelName') }}</div>
            </div>
            <NInput v-model:value="llmCustomModel" placeholder="your-model-id" class="field-control short" />
          </div>
        </template>
      </div>
    </div>

    <div class="surface-card llm-card">
      <div class="card-header prompt-header">
        <div class="card-title">{{ t('llm.prompts') }}</div>
        <NButton size="small" quaternary @click="resetPrompt">{{ t('llm.resetCurrentTab') }}</NButton>
      </div>
      <NTabs v-model:value="activePromptTab" type="line" animated>
        <NTabPane name="assistant" :tab="t('llm.assistantPrompt')">
          <NInput
            v-model:value="llmPromptTemplate"
            type="textarea"
            :rows="12"
            :placeholder="t('llm.assistantPromptPlaceholder')"
          />
        </NTabPane>
        <NTabPane name="translation" :tab="t('llm.translatePrompt')">
          <NInput
            v-model:value="translationPromptTemplate"
            type="textarea"
            :rows="12"
            :placeholder="t('llm.translatePromptPlaceholder')"
          />
        </NTabPane>
      </NTabs>
      <div class="card-sub" style="margin-top: var(--spacing-sm);">
        {{ t('llm.promptHint') }}
      </div>
    </div>
  </div>
</template>

<style scoped>
.settings-page {
  max-width: 1120px;
  width: 100%;
  padding-bottom: var(--spacing-2xl);
}

.llm-card {
  padding: var(--spacing-lg) var(--spacing-xl);
  background: var(--color-bg-secondary);
  border: 1px solid var(--color-border);
}

.card-header {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-bottom: var(--spacing-md);
}

.card-title {
  font-size: var(--font-lg);
  font-weight: 700;
}

.card-sub {
  color: var(--color-text-tertiary);
  font-size: var(--font-xs);
}

.prompt-header {
  flex-direction: row;
  align-items: center;
  justify-content: space-between;
}

.field-list {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-md);
}

.field-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-lg);
}

.field-text {
  display: flex;
  flex-direction: column;
  gap: 4px;
  flex: 1;
}

.field-label {
  font-weight: 600;
  color: var(--color-text-primary);
}

.field-sub {
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
}

.field-control {
  width: 420px;
  max-width: 100%;
}

.field-control.short {
  width: 260px;
}
</style>
