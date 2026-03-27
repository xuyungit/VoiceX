<script setup lang="ts">
import { computed } from 'vue'
import { NInput, NInputNumber, NSelect, NSwitch, NButton } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const removeTrailingPunctuation = computed({
  get: () => settingsStore.settings.removeTrailingPunctuation,
  set: (v) => settingsStore.updateSetting('removeTrailingPunctuation', v)
})

const shortSentenceThreshold = computed({
  get: () => settingsStore.settings.shortSentenceThreshold,
  set: (v) => settingsStore.updateSetting('shortSentenceThreshold', v)
})

const matchModeOptions = computed(() => [
  { label: t('postProcessing.exactMatch'), value: 'exact' },
  { label: t('postProcessing.contains'), value: 'contains' },
  { label: t('postProcessing.regex'), value: 'regex' }
])

function addRule() {
  const newRule = {
    id: crypto.randomUUID(),
    keyword: '',
    replacement: '',
    matchMode: 'exact' as const,
    enabled: true
  }
  settingsStore.settings.replacementRules.push(newRule)
}

function removeRule(id: string) {
  const index = settingsStore.settings.replacementRules.findIndex(r => r.id === id)
  if (index !== -1) {
    settingsStore.settings.replacementRules.splice(index, 1)
  }
}
</script>

<template>
  <div class="page settings-page post-processing-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('postProcessing.title') }}</h1>
      <div class="page-subtitle">
        <span class="subtitle-item">
          <span class="subtitle-label">{{ t('postProcessing.rules') }}</span>
          <span class="pill">{{ t('postProcessing.activeRules', { count: settingsStore.settings.replacementRules.length }) }}</span>
        </span>
      </div>
    </div>

    <!-- Smart Punctuation -->
    <div class="surface-card">
      <div class="card-header">
        <div class="card-title">{{ t('postProcessing.smartPunctuation') }}</div>
        <div class="card-sub">{{ t('postProcessing.smartPunctuationSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('postProcessing.removeTrailingPunctuation') }}</div>
            <div class="field-note">{{ t('postProcessing.removeTrailingPunctuationNote') }}</div>
          </div>
          <NSwitch v-model:value="removeTrailingPunctuation" />
        </div>
        <div class="field-row" v-if="removeTrailingPunctuation">
          <div class="field-text">
            <div class="field-label">{{ t('postProcessing.shortSentenceThreshold') }}</div>
            <div class="field-note">{{ t('postProcessing.shortSentenceThresholdNote') }}</div>
          </div>
          <NInputNumber
            v-model:value="shortSentenceThreshold"
            :min="1"
            :max="100"
            class="field-control short"
          />
        </div>
      </div>
    </div>

    <!-- Text Replacement Rules -->
    <div class="surface-card rules-card">
      <div class="card-header section-header">
        <div>
          <div class="card-title">{{ t('postProcessing.keywordSubstitution') }}</div>
          <div class="card-sub">{{ t('postProcessing.keywordSubstitutionSub') }}</div>
        </div>
        <NButton size="small" type="primary" secondary @click="addRule">
          <template #icon>
            <svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
              <path d="M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z"/>
            </svg>
          </template>
          {{ t('postProcessing.addRule') }}
        </NButton>
      </div>

      <div class="rules-list">
        <div v-if="settingsStore.settings.replacementRules.length === 0" class="empty-state">
          {{ t('postProcessing.noRules') }}
        </div>
        <div
          v-for="rule in settingsStore.settings.replacementRules"
          :key="rule.id"
          class="rule-item"
        >
          <div class="rule-main">
            <div class="rule-inputs">
              <div class="input-group">
                <span class="input-label">{{ t('postProcessing.ifOutputIs') }}</span>
                <NInput
                  v-model:value="rule.keyword"
                  :placeholder="t('postProcessing.keywordPlaceholder')"
                  size="small"
                  class="keyword-input"
                />
              </div>
              <div class="input-group">
                <span class="input-label">{{ t('postProcessing.replaceWith') }}</span>
                <NInput
                  v-model:value="rule.replacement"
                  :placeholder="t('postProcessing.replacementPlaceholder')"
                  size="small"
                  class="replacement-input"
                />
              </div>
            </div>
            <div class="rule-settings">
              <NSelect
                v-model:value="rule.matchMode"
                :options="matchModeOptions"
                size="small"
                class="mode-select"
              />
              <NSwitch v-model:value="rule.enabled" size="small" />
              <NButton size="small" quaternary circle type="error" @click="removeRule(rule.id)">
                <template #icon>
                  <svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
                    <path d="M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM19 4h-3.5l-1-1h-5l-1 1H5v2h14V4z"/>
                  </svg>
                </template>
              </NButton>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.settings-page {
  width: 100%;
  max-width: 1120px;
  padding-bottom: var(--spacing-2xl);
}

.surface-card {
  padding: var(--spacing-lg) var(--spacing-xl);
  background: var(--color-bg-secondary);
  border: 1px solid var(--color-border);
  margin-bottom: var(--spacing-xl);
}

.card-header {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-bottom: var(--spacing-md);
}

.section-header {
  flex-direction: row;
  justify-content: space-between;
  align-items: center;
}

.card-title {
  font-size: var(--font-lg);
  font-weight: 700;
}

.card-sub {
  color: var(--color-text-tertiary);
  font-size: var(--font-xs);
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

.field-note {
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
  max-width: 520px;
}

.field-control.short {
  width: 120px;
}

.rules-list {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-sm);
}

.rule-item {
  padding: var(--spacing-md);
  background: var(--color-bg-primary);
  border-radius: var(--radius-md);
  border: 1px solid var(--color-border);
}

.rule-main {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-xl);
}

.rule-inputs {
  display: flex;
  gap: var(--spacing-lg);
  flex: 1;
}

.input-group {
  display: flex;
  flex-direction: column;
  gap: 4px;
  flex: 1;
}

.input-label {
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
  font-weight: 500;
}

.rule-settings {
  display: flex;
  align-items: center;
  gap: var(--spacing-md);
  align-self: flex-end;
  padding-bottom: 2px;
}

.mode-select {
  width: 120px;
}

.empty-state {
  text-align: center;
  padding: var(--spacing-xl);
  color: var(--color-text-tertiary);
  font-style: italic;
  background: var(--color-bg-primary);
  border: 1px dashed var(--color-border);
  border-radius: var(--radius-md);
}
</style>
