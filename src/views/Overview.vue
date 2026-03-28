<script setup lang="ts">
import { onMounted, computed } from 'vue'
import { NSpin } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useHistoryStore } from '../stores/history'
import { useSettingsStore } from '../stores/settings'
import { formatHotkey } from '../utils/hotkey'

const historyStore = useHistoryStore()
const settingsStore = useSettingsStore()
const { t, locale } = useI18n()

const recentRecords = computed(() => historyStore.records.slice(0, 5))

const hotkeyDisplay = computed(() => {
  const config = settingsStore.settings.hotkeyConfig
  return formatHotkey(config) ?? t('overview.unset')
})

const formatDuration = (durationMs: number) => {
  const minutes = Math.floor(durationMs / 60000)
  const hours = Math.floor(minutes / 60)
  const remaining = minutes % 60
  if (hours > 0) {
    return t('overview.hoursMinutes', { hours, minutes: remaining })
  }
  return t('overview.minutesOnly', { minutes: remaining })
}

const formatCharacters = (count: number) => {
  if (count >= 1000) return `${(count / 1000).toFixed(1)}K`
  return count.toString()
}

const totalDuration = computed(() => formatDuration(historyStore.stats.totalDurationMs || 0))
const localDuration = computed(() => formatDuration(historyStore.localStats.totalDurationMs || 0))

const totalCharacters = computed(() => formatCharacters(historyStore.stats.totalCharacters || 0))
const localCharacters = computed(() => formatCharacters(historyStore.localStats.totalCharacters || 0))

const averageSpeed = computed(() => historyStore.formattedStats.averageSpeed || 0)

const aiCalls = computed(() => historyStore.stats.llmCorrectionCount || 0)
const localAiCalls = computed(() => historyStore.localStats.llmCorrectionCount || 0)
const localAverageSpeed = computed(() => historyStore.formattedLocalStats.averageSpeed || 0)

onMounted(async () => {
  await Promise.all([
    historyStore.loadHistory(true),
    historyStore.loadStats()
  ])
})

function formatTime(timestamp: string): string {
  const date = new Date(timestamp)
  return date.toLocaleString(locale.value, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit'
  })
}
</script>

<template>
  <div class="page overview-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('overview.title') }}</h1>
      <div class="status-lines">
        <div class="meta-line">
          <span class="meta-label">{{ t('overview.hotkey') }}:</span>
          <span class="meta-value">{{ hotkeyDisplay }}</span>
        </div>
      </div>
    </div>

    <div class="stats-grid">
      <div class="surface-card stat-card">
        <div class="stat-icon">
          <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
            <path d="M12 2a10 10 0 1 0 10 10A10 10 0 0 0 12 2zm0 18a8 8 0 1 1 8-8 8 8 0 0 1-8 8zm.5-13h-1v6l5 3 .5-.84-4.5-2.66Z" />
          </svg>
        </div>
        <div class="stat-content">
          <div class="stat-value">{{ totalDuration }}</div>
          <div class="stat-subvalue">{{ t('overview.thisDevice') }}: {{ localDuration }}</div>
          <div class="stat-label">{{ t('overview.totalDictationTime') }}</div>
        </div>
      </div>

      <div class="surface-card stat-card">
        <div class="stat-icon">
          <span class="glyph">Aa</span>
        </div>
        <div class="stat-content">
          <div class="stat-value">{{ totalCharacters }}</div>
          <div class="stat-subvalue">{{ t('overview.thisDevice') }}: {{ localCharacters }}</div>
          <div class="stat-label">{{ t('overview.totalCharacters') }}</div>
        </div>
      </div>

      <div class="surface-card stat-card">
        <div class="stat-icon">
          <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
            <path d="M12 2a1 1 0 0 0-.92.6l-3 7a1 1 0 0 0 1.84.8L10.38 9h3.24l.46 1.4a1 1 0 0 0 1.92-.6l-3-7A1 1 0 0 0 12 2Zm7.66 11.11-2.78-.93a1 1 0 0 0-.64 1.9l1.38.46-1.88 2.64-1.37-.46a1 1 0 1 0-.64 1.9l2.78.92a1 1 0 0 0 1.1-.38l2.5-3.5a1 1 0 0 0-.45-1.55ZM8.1 14.2l-1.38.46-1.88-2.64 1.37-.46a1 1 0 1 0-.64-1.9l-2.78.92a1 1 0 0 0-.45 1.56l2.5 3.5a1 1 0 0 0 1.1.38l2.78-.93a1 1 0 0 0-.64-1.9Z" />
          </svg>
        </div>
        <div class="stat-content">
          <div class="stat-value">{{ aiCalls }}</div>
          <div class="stat-subvalue">{{ t('overview.thisDevice') }}: {{ localAiCalls }}</div>
          <div class="stat-label">{{ t('overview.aiCalls') }}</div>
        </div>
      </div>

      <div class="surface-card stat-card">
        <div class="stat-icon">
          <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
            <path d="M11 21h-1l2-7H7l8-10h1l-2 8h5Z" />
          </svg>
        </div>
        <div class="stat-content">
          <div class="stat-value">{{ t('overview.charactersPerMinute', { value: averageSpeed }) }}</div>
          <div class="stat-subvalue">{{ t('overview.thisDevice') }}: {{ t('overview.charactersPerMinute', { value: localAverageSpeed }) }}</div>
          <div class="stat-label">{{ t('overview.averageDictationSpeed') }}</div>
        </div>
      </div>
    </div>

    <div class="surface-card recent-card">
      <div class="recent-header">{{ t('overview.recentHistory') }}</div>

      <div v-if="historyStore.isLoading" class="loading-container">
        <NSpin size="small" />
      </div>

      <div v-else-if="recentRecords.length === 0" class="empty-state">
        <p class="muted">{{ t('overview.noRecords') }}</p>
      </div>

      <div v-else class="history-list">
        <div
          v-for="record in recentRecords"
          :key="record.id"
          class="history-row"
        >
          <div class="history-time">{{ formatTime(record.timestamp) }}</div>
          <div class="history-text">{{ record.text }}</div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.page-header {
  gap: 4px;
}

.status-lines {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: var(--font-sm);
  color: var(--color-text-secondary);
}

.meta-line {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.meta-label {
  color: var(--color-text-tertiary);
}

.stats-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
  gap: var(--spacing-lg);
}

.stat-card {
  display: flex;
  align-items: center;
  gap: var(--spacing-md);
  padding: var(--spacing-lg) var(--spacing-xl);
  background: var(--color-bg-secondary);
  border: 1px solid var(--color-border);
}

.stat-icon {
  width: 38px;
  height: 38px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 12px;
  background: rgba(255, 255, 255, 0.04);
  color: var(--color-text-secondary);
  flex-shrink: 0;
}

.stat-icon svg {
  width: 22px;
  height: 22px;
}

.glyph {
  font-weight: 700;
  letter-spacing: 0.02em;
}

.stat-content {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.stat-value {
  font-size: 22px;
  font-weight: 700;
  letter-spacing: 0.01em;
}

.stat-subvalue {
  font-size: var(--font-xs);
  color: var(--color-text-secondary);
}

.stat-label {
  color: var(--color-text-tertiary);
  font-size: var(--font-xs);
}

.recent-card {
  padding: var(--spacing-lg) var(--spacing-xl);
  background: var(--color-bg-secondary);
  border: 1px solid var(--color-border);
}

.recent-header {
  font-size: var(--font-lg);
  font-weight: 700;
  margin-bottom: var(--spacing-md);
}

.history-list {
  display: flex;
  flex-direction: column;
}

.history-row {
  display: grid;
  grid-template-columns: 130px 1fr;
  gap: var(--spacing-lg);
  padding: 12px 0;
  border-bottom: 1px solid var(--color-divider);
}

.history-row:last-child {
  border-bottom: none;
}

.history-time {
  font-size: var(--font-sm);
  color: var(--color-text-tertiary);
}

.history-text {
  color: var(--color-text-primary);
  line-height: 1.55;
}

.loading-container {
  display: flex;
  justify-content: center;
  padding: var(--spacing-lg);
}

.empty-state {
  text-align: center;
  padding: var(--spacing-lg);
}
</style>
