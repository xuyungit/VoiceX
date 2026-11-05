<script setup lang="ts">
import { computed } from 'vue'
import { NInput, NInputNumber, NSelect, NSwitch, NButton } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'
import SettingsSection from '../components/settings/SettingsSection.vue'
import SettingRow from '../components/settings/SettingRow.vue'

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
  <div class="page settings-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('postProcessing.title') }}</h1>
    </div>

    <SettingsSection :title="t('postProcessing.smartPunctuation')" :help="t('postProcessing.smartPunctuationSub')">
      <SettingRow :label="t('postProcessing.removeTrailingPunctuation')" :help="t('postProcessing.removeTrailingPunctuationNote')">
        <NSwitch v-model:value="removeTrailingPunctuation" />
      </SettingRow>
      <SettingRow v-if="removeTrailingPunctuation" :label="t('postProcessing.shortSentenceThreshold')" :help="t('postProcessing.shortSentenceThresholdNote')">
        <NInputNumber
          v-model:value="shortSentenceThreshold"
          :min="1"
          :max="100"
          class="compact"
        />
      </SettingRow>
    </SettingsSection>

    <SettingsSection :title="t('postProcessing.keywordSubstitution')" :help="t('postProcessing.keywordSubstitutionSub')">
      <template #extra>
        <span class="pill">{{ t('postProcessing.activeRules', { count: settingsStore.settings.replacementRules.filter((rule) => rule.enabled).length }) }}</span>
        <NButton size="small" type="primary" secondary @click="addRule">
          <template #icon>
            <svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
              <path d="M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z"/>
            </svg>
          </template>
          {{ t('postProcessing.addRule') }}
        </NButton>
      </template>

      <div v-if="settingsStore.settings.replacementRules.length === 0" class="empty-state">
        {{ t('postProcessing.noRules') }}
      </div>
      <div v-else class="rules">
        <div class="rule-columns" aria-hidden="true">
          <span>{{ t('postProcessing.ifOutputIs') }}</span>
          <span>{{ t('postProcessing.replaceWith') }}</span>
        </div>
        <ul class="rules-list">
          <li
            v-for="rule in settingsStore.settings.replacementRules"
            :key="rule.id"
            class="rule-item"
            :class="{ disabled: !rule.enabled }"
          >
            <NInput
              v-model:value="rule.keyword"
              :placeholder="t('postProcessing.keywordPlaceholder')"
              :aria-label="t('postProcessing.ifOutputIs')"
              size="small"
            />
            <span class="rule-arrow" aria-hidden="true">→</span>
            <NInput
              v-model:value="rule.replacement"
              :placeholder="t('postProcessing.replacementPlaceholder')"
              :aria-label="t('postProcessing.replaceWith')"
              size="small"
            />
            <NSelect
              v-model:value="rule.matchMode"
              :options="matchModeOptions"
              size="small"
              class="mode-select"
            />
            <NSwitch v-model:value="rule.enabled" size="small" />
            <NButton size="small" quaternary circle class="rule-remove" @click="removeRule(rule.id)">
              <template #icon>
                <svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
                  <path d="M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM19 4h-3.5l-1-1h-5l-1 1H5v2h14V4z"/>
                </svg>
              </template>
            </NButton>
          </li>
        </ul>
      </div>
    </SettingsSection>
  </div>
</template>

<style scoped>
.rules {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding-bottom: var(--spacing-sm);
}

.rule-columns,
.rule-item {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 16px minmax(0, 1fr) 120px 40px 28px;
  align-items: center;
  gap: var(--spacing-sm);
}

.rule-columns {
  padding: 0;
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
}

.rule-columns span:nth-child(2) {
  grid-column: 3;
}

.rules-list {
  list-style: none;
}

.rule-item {
  padding: 8px 0;
  transition: opacity var(--transition-fast);
}

.rule-item + .rule-item {
  border-top: 1px solid var(--color-divider);
}

.rule-item.disabled > :not(.n-switch):not(.rule-remove) {
  opacity: 0.5;
}

.rule-arrow {
  text-align: center;
  color: var(--color-text-tertiary);
}

.rule-remove {
  color: var(--color-text-tertiary);
}

.rule-remove:hover {
  color: var(--color-error);
}

.empty-state {
  padding: var(--spacing-lg);
  text-align: center;
  font-size: var(--font-sm);
  color: var(--color-text-tertiary);
}
</style>
