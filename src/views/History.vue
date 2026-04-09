<script setup lang="ts">
import { computed, h, onBeforeUnmount, onMounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NSpin, NEmpty, NSelect, NModal, NDropdown, useDialog, type DropdownOption } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useHistoryStore, type HistoryRecord } from '../stores/history'
import { useSyncStore } from '../stores/sync'
import { useSettingsStore } from '../stores/settings'
import { trimTrailingPunctuation } from '../utils/text'
import ReTranscribeDialog from '../components/ReTranscribeDialog.vue'

const historyStore = useHistoryStore()
const settingsStore = useSettingsStore()
const syncStore = useSyncStore()
const dialog = useDialog()
const { t, locale } = useI18n()

const retentionOptions = computed(() => [
  { label: t('common.forever'), value: 0 },
  { label: t('common.days', { count: 365 }), value: 365 },
  { label: t('common.days', { count: 180 }), value: 180 },
  { label: t('common.days', { count: 30 }), value: 30 },
  { label: t('common.days', { count: 7 }), value: 7 }
])

const textRetention = computed({
  get: () => settingsStore.settings.textRetentionDays,
  set: (v: number) => settingsStore.updateSetting('textRetentionDays', v)
})

const audioRetention = computed({
  get: () => settingsStore.settings.audioRetentionDays,
  set: (v: number) => settingsStore.updateSetting('audioRetentionDays', v)
})

const groupedRecords = computed(() => {
  const buckets: { label: string; records: HistoryRecord[] }[] = []
  const map = new Map<string, HistoryRecord[]>()

  for (const record of historyStore.records) {
    const date = new Date(record.timestamp)
    const today = new Date()
    const isToday =
      date.getFullYear() === today.getFullYear() &&
      date.getMonth() === today.getMonth() &&
      date.getDate() === today.getDate()
    const label = isToday
      ? t('history.today')
      : date.toLocaleDateString(locale.value, { month: 'long', day: 'numeric' })

    if (!map.has(label)) {
      const list: HistoryRecord[] = []
      map.set(label, list)
      buckets.push({ label, records: list })
    }
    map.get(label)!.push(record)
  }

  return buckets
})

const detailVisible = ref(false)
const detailRecord = ref<HistoryRecord | null>(null)
const reTranscribeVisible = ref(false)
const reTranscribeRecord = ref<HistoryRecord | null>(null)
const audioPlayer = ref<HTMLAudioElement | null>(null)
const playingId = ref<string | null>(null)
const objectUrl = ref<string | null>(null)
onMounted(async () => {
  await Promise.all([
    settingsStore.loadSettings(),
    historyStore.loadHistory(true)
  ])
})

onBeforeUnmount(() => {
  stopPlayback()
})

function loadMore() {
  historyStore.loadHistory(false)
}

async function handleCopy(record: HistoryRecord) {
  if (!hasRecordText(record)) return
  await historyStore.copyText(record.text)
}

async function handleDelete(record: HistoryRecord) {
  if (playingId.value === record.id) {
    stopPlayback()
  }
  await historyStore.deleteRecord(record.id)
}

function formatTime(timestamp: string): string {
  const date = new Date(timestamp)
  return date.toLocaleTimeString(locale.value, {
    hour: '2-digit',
    minute: '2-digit'
  })
}

function formatDuration(ms: number): string {
  if (!ms || ms <= 0) return '--'
  const seconds = Math.floor(ms / 1000)
  const minutes = Math.floor(seconds / 60)
  const remainingSeconds = seconds % 60
  return `${minutes}:${remainingSeconds.toString().padStart(2, '0')}`
}

function formatDevice(record: HistoryRecord): string {
  if (record.sourceDeviceName && record.sourceDeviceName.trim()) {
    return record.sourceDeviceName
  }
  if (!settingsStore.settings.syncEnabled) {
    return t('common.thisDevice')
  }
  if (record.sourceDeviceId && record.sourceDeviceId === syncStore.deviceId) {
    return t('common.thisDevice')
  }
  if (record.sourceDeviceId) {
    return t('history.devicePrefix', { id: record.sourceDeviceId.slice(0, 6) })
  }
  return t('common.thisDevice')
}

async function togglePlayback(record: HistoryRecord) {
  if (!record.audioPath) {
    dialog.warning({
      title: '无法播放',
      content: '缺少录音文件路径，可能未成功保存录音。',
      positiveText: '知道了'
    })
    return
  }

  if (playingId.value === record.id) {
    stopPlayback()
    return
  }

  try {
    stopPlayback()
    const bytes = await invoke<number[]>('load_audio_file', { path: record.audioPath })
    const buffer = new Uint8Array(bytes)
    const url = URL.createObjectURL(new Blob([buffer], { type: 'audio/ogg' }))
    objectUrl.value = url
    const audio = new Audio(url)
    audioPlayer.value = audio
    playingId.value = record.id
    audio.onended = () => {
      playingId.value = null
      audioPlayer.value = null
      if (objectUrl.value) {
        URL.revokeObjectURL(objectUrl.value)
        objectUrl.value = null
      }
    }
    await audio.play()
  } catch (error) {
    console.error('Failed to play audio:', error)
    dialog.error({
      title: '播放失败',
      content: '录音文件无法播放，请检查文件是否存在或格式是否受支持。'
    })
    playingId.value = null
    audioPlayer.value = null
    if (objectUrl.value) {
      URL.revokeObjectURL(objectUrl.value)
      objectUrl.value = null
    }
  }
}

function stopPlayback() {
  if (audioPlayer.value) {
    audioPlayer.value.pause()
    audioPlayer.value.currentTime = 0
    audioPlayer.value = null
  }
  playingId.value = null
  if (objectUrl.value) {
    URL.revokeObjectURL(objectUrl.value)
    objectUrl.value = null
  }
}

function openDetails(record: HistoryRecord) {
  detailRecord.value = record
  detailVisible.value = true
}

function isTranslateMode(record: HistoryRecord): boolean {
  return record.mode.startsWith('translate_en')
}

function isFailedRecord(record: HistoryRecord): boolean {
  return record.errorCode !== 0
}

function hasRecordText(record: HistoryRecord): boolean {
  return record.text.trim().length > 0
}

function displayText(record: HistoryRecord): string {
  if (hasRecordText(record)) {
    return record.text
  }
  if (isFailedRecord(record)) {
    return t('history.failedPlaceholder')
  }
  return t('common.notRecorded')
}

function failureMessage(record: HistoryRecord): string {
  const message = record.errorMessage?.trim()
  return message && message.length > 0 ? message : t('history.failedNoReason')
}

function canCompare(record: HistoryRecord) {
  if (isFailedRecord(record)) return false
  if (!record.originalText) return false
  if (!isTranslateMode(record) && !record.aiCorrectionApplied) return false

  const originalProcessed = trimTrailingPunctuation(record.originalText)
  const textProcessed = trimTrailingPunctuation(record.text)

  return originalProcessed !== textProcessed
}

function modeBadge(record: HistoryRecord): string | null {
  if (isFailedRecord(record)) {
    return null
  }
  if (record.mode.startsWith('translate_en')) {
    return t('history.englishTranslation')
  }
  if (record.mode.startsWith('assistant')) {
    return 'Assistant'
  }
  return null
}

function compareTagLabel(record: HistoryRecord): string {
  return isTranslateMode(record) ? t('history.compareTranslation') : t('history.aiCorrection')
}

function showCompareTag(record: HistoryRecord): boolean {
  return canCompare(record) && !isTranslateMode(record)
}

function closeDetails() {
  detailVisible.value = false
  detailRecord.value = null
}

async function copyOriginal() {
  if (!detailRecord.value) return
  const original = resolvedOriginalText(detailRecord.value)
  if (!original) return
  await historyStore.copyText(original)
}

async function openRecordingsFolder() {
  try {
    await invoke('open_recordings_dir')
  } catch (error) {
    console.error('Failed to open recordings directory:', error)
    dialog.error({
      title: '打开失败',
      content: '无法打开录音目录，请稍后重试。'
    })
  }
}

function confirmDelete(record: HistoryRecord) {
  dialog.warning({
    title: '删除历史',
    content: '确定删除这条记录吗？',
    positiveText: '删除',
    negativeText: '取消',
    onPositiveClick: () => handleDelete(record)
  })
}

function formatDateTime(timestamp: string): string {
  const date = new Date(timestamp)
  return date.toLocaleString(locale.value, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false
  })
}

function modeLabel(record: HistoryRecord): string {
  if (isFailedRecord(record)) {
    return t('history.transcriptionFailed')
  }
  if (record.mode.startsWith('translate_en')) {
    return t('history.englishTranslation')
  }
  if (record.mode.startsWith('assistant')) {
    return t('history.assistantMode')
  }
  return t('history.plainTranscription')
}

function modelLabel(value: string | null | undefined, fallback?: string): string {
  const fallbackText = fallback ?? t('common.notRecorded')
  if (!value) return fallbackText
  const trimmed = value.trim()
  return trimmed || fallbackText
}

function llmModelLabel(record: HistoryRecord): string {
  if (isFailedRecord(record)) {
    return t('common.none')
  }
  if (record.llmInvoked) {
    return modelLabel(record.llmModelName)
  }
  return t('common.none')
}

function resolvedOriginalText(record: HistoryRecord): string | null {
  if (isFailedRecord(record)) {
    return null
  }
  if (record.originalText) {
    return record.originalText
  }
  if (record.llmInvoked && !isTranslateMode(record)) {
    return record.text
  }
  return null
}

function showNoCorrectionTag(record: HistoryRecord): boolean {
  return record.llmInvoked && !isTranslateMode(record) && !canCompare(record)
}

function renderIcon(path: string) {
  return () =>
    h(
      'svg',
      { viewBox: '0 0 24 24', fill: 'currentColor', width: '18', height: '18' },
      [h('path', { d: path })]
    )
}

function moreOptions(record: HistoryRecord): DropdownOption[] {
  const isPlaying = playingId.value === record.id

  return [
    {
      label: isPlaying ? t('history.stopPlayback') : t('history.playAudio'),
      key: 'play',
      disabled: !record.audioPath,
      icon: renderIcon(isPlaying ? 'M8 5h3v14H8zm5 0h3v14h-3z' : 'M8 5.14 19 12l-11 6.86V5.14z')
    },
    {
      label: t('history.reTranscribe'),
      key: 'retranscribe',
      disabled: !record.audioPath,
      icon: renderIcon('M17.65 6.35A7.96 7.96 0 0 0 12 4C7.58 4 4.01 7.58 4.01 12S7.58 20 12 20c3.73 0 6.84-2.55 7.73-6h-2.08A5.99 5.99 0 0 1 12 18c-3.31 0-6-2.69-6-6s2.69-6 6-6c1.66 0 3.14.69 4.22 1.78L13 11h7V4l-2.35 2.35z')
    },
    {
      label: t('history.viewDetails'),
      key: 'details',
      icon: renderIcon('M14 2H6c-1.1 0-2 .9-2 2v16c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V8l-6-6zm0 2.5L17.5 8H14V4.5zM8 13h8v2H8v-2zm0 4h8v2H8v-2zm0-8h5v2H8V9z')
    }
  ]
}

function handleMoreAction(key: string | number, record: HistoryRecord) {
  if (key === 'play') {
    void togglePlayback(record)
    return
  }

  if (key === 'retranscribe') {
    reTranscribeRecord.value = record
    reTranscribeVisible.value = true
    return
  }

  if (key === 'details') {
    openDetails(record)
  }
}
</script>

<template>
  <div class="page history-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('history.title') }}</h1>
      <div class="page-subtitle">
        <span class="subtitle-item">
          <span class="subtitle-label">{{ t('history.textRetention') }}</span>
          <span class="pill">{{ textRetention === 0 ? t('common.forever') : t('common.days', { count: textRetention }) }}</span>
        </span>
        <span class="subtitle-item">
          <span class="subtitle-label">{{ t('history.audioRetention') }}</span>
          <span class="pill">{{ audioRetention === 0 ? t('common.forever') : t('common.days', { count: audioRetention }) }}</span>
        </span>
      </div>
    </div>

    <div class="surface-card retention-card">
      <div class="retention-title">
        <div class="section-title">{{ t('history.retentionPolicy') }}</div>
        <div class="section-hint">{{ t('history.retentionPolicyHint') }}</div>
      </div>
      <div class="retention-body">
        <div class="retention-fields">
          <div class="retention-field">
            <div class="retention-label">{{ t('history.text') }}</div>
            <NSelect v-model:value="textRetention" :options="retentionOptions" size="small" class="retention-select" />
          </div>
          <div class="retention-field">
            <div class="retention-label">{{ t('history.audio') }}</div>
            <NSelect v-model:value="audioRetention" :options="retentionOptions" size="small" class="retention-select" />
          </div>
          <NButton size="small" quaternary class="open-folder" @click="openRecordingsFolder">
            {{ t('history.openRecordingsFolder') }}
          </NButton>
        </div>
        <div class="retention-hint">{{ t('history.autoCleanupHint') }}</div>
      </div>
    </div>

    <div v-if="historyStore.isLoading && historyStore.records.length === 0" class="loading-container">
      <NSpin size="medium" />
    </div>

    <div v-else-if="historyStore.records.length === 0" class="empty-container">
      <NEmpty :description="t('history.empty')" />
    </div>

    <div v-else class="surface-card list-wrapper">
      <template v-for="group in groupedRecords" :key="group.label">
        <div class="date-header">{{ group.label }}</div>
        <div class="list-card">
          <div
            v-for="record in group.records"
            :key="record.id"
            class="list-row"
          >
            <div class="row-header">
              <div class="row-meta">
                <span class="row-time">{{ formatTime(record.timestamp) }}</span>
                <span class="device-meta">{{ formatDevice(record) }}</span>
                <span
                  v-if="modeBadge(record)"
                  class="tag mode-tag row-tag"
                  :class="{ translate: isTranslateMode(record) }"
                >
                  {{ modeBadge(record) }}
                </span>
                <span
                  v-if="showCompareTag(record)"
                  class="tag row-tag"
                >
                  {{ compareTagLabel(record) }}
                </span>
                <span
                  v-if="isFailedRecord(record)"
                  class="tag row-tag failed-tag"
                >
                  {{ t('history.transcriptionFailed') }}
                </span>
              </div>
              <div class="list-actions">
                <span class="list-meta duration-chip">{{ formatDuration(record.durationMs) }}</span>
                <span v-if="playingId === record.id" class="pill accent">{{ t('history.playing') }}</span>
                <div class="action-toolbar">
                  <NButton quaternary size="small" class="toolbar-button" aria-label="复制" @click="() => handleCopy(record)">
                    <template #icon>
                      <svg viewBox="0 0 24 24" fill="currentColor" width="16" height="16">
                        <path d="M16 1H4c-1.1 0-2 .9-2 2v14h2V3h12V1zm3 4H8c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h11c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm0 16H8V7h11v14z" />
                      </svg>
                    </template>
                  </NButton>
                  <div class="toolbar-divider" aria-hidden="true" />
                  <NButton
                    quaternary
                    size="small"
                    type="error"
                    class="toolbar-button"
                    aria-label="删除"
                    @click="() => confirmDelete(record)"
                  >
                    <template #icon>
                      <svg viewBox="0 0 24 24" fill="currentColor" width="16" height="16">
                        <path d="M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM19 4h-3.5l-1-1h-5l-1 1H5v2h14V4z" />
                      </svg>
                    </template>
                  </NButton>
                  <div class="toolbar-divider" aria-hidden="true" />
                  <NDropdown
                    trigger="click"
                    placement="bottom-end"
                    :options="moreOptions(record)"
                    @select="(key) => handleMoreAction(key, record)"
                  >
                    <NButton quaternary size="small" class="toolbar-button" aria-label="更多操作">
                      <template #icon>
                        <svg viewBox="0 0 24 24" fill="currentColor" width="16" height="16">
                          <path d="M12 8a2 2 0 1 0 0-4 2 2 0 0 0 0 4zm0 8a2 2 0 1 0 0-4 2 2 0 0 0 0 4zm0 8a2 2 0 1 0 0-4 2 2 0 0 0 0 4z" transform="translate(0 -4)" />
                        </svg>
                      </template>
                    </NButton>
                  </NDropdown>
                </div>
              </div>
            </div>
            <div class="list-text" :class="{ failed: isFailedRecord(record) }">
              {{ displayText(record) }}
            </div>
            <div v-if="isFailedRecord(record)" class="failure-note">
              {{ failureMessage(record) }}
            </div>
          </div>
        </div>
      </template>

      <div v-if="historyStore.hasMore" class="load-more">
        <NButton
          :loading="historyStore.isLoading"
          @click="loadMore"
          quaternary
        >
          {{ t('common.loadMore') }}
        </NButton>
      </div>
    </div>

    <NModal
      v-model:show="detailVisible"
      preset="card"
      :title="t('history.detailTitle')"
      style="max-width: 720px;"
      @after-leave="closeDetails"
    >
      <div v-if="detailRecord" class="detail-panel">
        <div class="detail-summary">
          <div class="detail-summary-row">
            <span class="detail-item" :title="formatDateTime(detailRecord.timestamp)">
              <svg viewBox="0 0 24 24" fill="currentColor" width="14" height="14" aria-hidden="true">
                <path d="M19 4h-1V2h-2v2H8V2H6v2H5a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V6a2 2 0 0 0-2-2zm0 15H5V10h14v9zm0-11H5V6h14v2z" />
              </svg>
              <span class="detail-text">{{ formatDateTime(detailRecord.timestamp) }}</span>
            </span>
            <span class="detail-item" :title="formatDevice(detailRecord)">
              <svg viewBox="0 0 24 24" fill="currentColor" width="14" height="14" aria-hidden="true">
                <path d="M4 6h16a1 1 0 0 1 1 1v10H3V7a1 1 0 0 1 1-1zm-1 13h18v1H3v-1z" />
              </svg>
              <span class="detail-text">{{ formatDevice(detailRecord) }}</span>
            </span>
            <span class="detail-item" :title="modeLabel(detailRecord)">
              <svg viewBox="0 0 24 24" fill="currentColor" width="14" height="14" aria-hidden="true">
                <path d="M12 3 1 9l11 6 9-4.91V17h2V9L12 3zm0 9.82L5.04 9 12 5.18 18.96 9 12 12.82zM5 13.18l7 3.82 7-3.82V17l-7 4-7-4v-3.82z" />
              </svg>
              <span class="detail-text">{{ modeLabel(detailRecord) }}</span>
            </span>
            <span class="detail-item" :title="formatDuration(detailRecord.durationMs)">
              <svg viewBox="0 0 24 24" fill="currentColor" width="14" height="14" aria-hidden="true">
                <path d="M12 1a11 11 0 1 0 11 11A11 11 0 0 0 12 1zm1 11.41 3.29 3.3-1.41 1.41L11 13V6h2z" />
              </svg>
              <span class="detail-text">{{ formatDuration(detailRecord.durationMs) }}</span>
            </span>
          </div>
          <div class="detail-summary-row detail-summary-models">
            <span class="detail-item detail-item-model" :title="`ASR ${modelLabel(detailRecord.asrModelName)}`">
              <svg viewBox="0 0 24 24" fill="currentColor" width="14" height="14" aria-hidden="true">
                <path d="M12 14a3 3 0 0 0 3-3V6a3 3 0 0 0-6 0v5a3 3 0 0 0 3 3zm5-3a5 5 0 0 1-10 0H5a7 7 0 0 0 6 6.92V21h2v-3.08A7 7 0 0 0 19 11z" />
              </svg>
              <span class="detail-text">{{ modelLabel(detailRecord.asrModelName) }}</span>
            </span>
            <span class="detail-item detail-item-model" :title="`LLM ${llmModelLabel(detailRecord)}`">
              <svg viewBox="0 0 24 24" fill="currentColor" width="14" height="14" aria-hidden="true">
                <path d="M12 2 4 6v6c0 5 3.4 9.74 8 11 4.6-1.26 8-6 8-11V6l-8-4zm-1 15-4-4 1.41-1.41L11 14.17l4.59-4.58L17 11l-6 6z" />
              </svg>
              <span class="detail-text">{{ llmModelLabel(detailRecord) }}</span>
            </span>
          </div>
        </div>

        <div class="detail-section">
          <div class="detail-header">
            <div class="detail-title">
              {{ t('history.finalText') }}
              <span v-if="showNoCorrectionTag(detailRecord)" class="tag detail-tag no-correction-tag">
                {{ t('history.noCorrection') }}
              </span>
              <span v-if="isFailedRecord(detailRecord)" class="tag detail-tag failed-tag">
                {{ t('history.transcriptionFailed') }}
              </span>
            </div>
            <NButton quaternary size="tiny" :disabled="!hasRecordText(detailRecord)" @click="handleCopy(detailRecord)">
              {{ t('common.copy') }}
            </NButton>
          </div>
          <div class="detail-body" :class="{ failed: isFailedRecord(detailRecord) }">
            {{ displayText(detailRecord) }}
          </div>
        </div>

        <div v-if="isFailedRecord(detailRecord)" class="detail-section">
          <div class="detail-header">
            <div class="detail-title">{{ t('history.failureReason') }}</div>
          </div>
          <div class="detail-body failed-detail">
            {{ failureMessage(detailRecord) }}
          </div>
        </div>

        <div class="detail-section">
          <div class="detail-header">
            <div class="detail-title">{{ t('history.originalRecognition') }}</div>
            <NButton
              quaternary
              size="tiny"
              :disabled="!resolvedOriginalText(detailRecord)"
              @click="copyOriginal"
            >
              {{ t('common.copy') }}
            </NButton>
          </div>
          <div class="detail-body muted" v-if="resolvedOriginalText(detailRecord)">
            {{ resolvedOriginalText(detailRecord) }}
          </div>
          <div class="detail-body muted" v-else>
            {{ t('history.noOriginalText') }}
          </div>
        </div>

      </div>
    </NModal>

    <ReTranscribeDialog
      v-model:show="reTranscribeVisible"
      :record="reTranscribeRecord"
    />
  </div>
</template>

<style scoped>
.retention-card {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-xl);
  padding: var(--spacing-md) var(--spacing-lg);
}

.retention-title {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.retention-body {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 6px;
  flex: 1;
}

.retention-fields {
  display: flex;
  align-items: center;
  gap: var(--spacing-md);
  justify-content: flex-end;
  flex-wrap: nowrap;
}

.retention-field {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
  flex-shrink: 0;
}

.retention-label {
  font-size: var(--font-sm);
  color: var(--color-text-secondary);
  white-space: nowrap;
  flex-shrink: 0;
}

.retention-select {
  width: 140px;
}

.retention-hint {
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
  text-align: right;
}

.open-folder {
  border: 1px solid var(--color-border);
  background: var(--color-bg-tertiary);
  white-space: nowrap;
}

.loading-container,
.empty-container {
  display: flex;
  justify-content: center;
  align-items: center;
  padding: var(--spacing-2xl);
}

.list-wrapper {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-lg);
}

.date-header {
  font-size: var(--font-sm);
  font-weight: 600;
  color: var(--color-text-secondary);
}

.list-card {
  border: 1px solid var(--color-border);
  border-radius: var(--radius-lg);
  overflow: hidden;
  background: var(--color-bg-tertiary);
}

.device-meta {
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.list-row {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-sm);
  align-items: stretch;
}

.list-row:nth-child(odd) {
  background: rgba(255, 255, 255, 0.01);
}

.row-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-md);
  width: 100%;
}

.row-meta {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
  min-width: 0;
  flex: 1;
  flex-wrap: wrap;
}

.row-time {
  color: var(--color-text-secondary);
  white-space: nowrap;
}

.row-tag {
  flex-shrink: 0;
}

.list-text {
  min-width: 0;
  overflow-wrap: anywhere;
}

.list-text.failed {
  color: var(--color-text-secondary);
}

.failure-note {
  font-size: var(--font-sm);
  color: color-mix(in srgb, var(--color-error) 62%, var(--color-text-primary));
  overflow-wrap: anywhere;
}

.list-actions {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: var(--spacing-sm);
  flex-shrink: 0;
  margin-left: auto;
}

.action-toolbar {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  padding: 4px;
  border-radius: calc(var(--radius-lg) + 2px);
  border: 1px solid var(--color-border);
  background: var(--color-bg-secondary);
  box-shadow: var(--shadow-sm);
}

.toolbar-button {
  min-width: 32px;
}

.toolbar-divider {
  width: 1px;
  height: 18px;
  background: var(--color-divider);
}

.duration-chip {
  padding: 2px 8px;
  border-radius: var(--radius-sm);
  background: var(--color-bg-secondary);
}

.mode-tag {
  border: 1px solid var(--color-border);
}

.mode-tag.translate {
  border-color: rgba(134, 239, 172, 0.5);
  color: rgba(134, 239, 172, 0.95);
}

.failed-tag {
  border-color: color-mix(in srgb, var(--color-error) 35%, transparent);
  color: color-mix(in srgb, var(--color-error) 62%, var(--color-text-primary));
}

.load-more {
  display: flex;
  justify-content: center;
  padding: var(--spacing-lg);
}

.detail-panel {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-lg);
}

.detail-summary {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 10px 12px;
  border: 1px solid var(--color-border);
  border-radius: var(--radius-md);
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.018), rgba(255, 255, 255, 0.008)),
    var(--color-bg-tertiary);
}

.detail-summary-row {
  display: flex;
  flex-wrap: wrap;
  gap: 8px 16px;
  align-items: center;
}

.detail-summary-models {
  padding-top: 8px;
  border-top: 1px solid rgba(255, 255, 255, 0.05);
}

.detail-item {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
  max-width: 100%;
  color: var(--color-text-tertiary);
  font-size: var(--font-xs);
  line-height: 1.3;
}

.detail-item-model {
  min-width: min(320px, 100%);
}

.detail-item svg {
  flex: 0 0 auto;
  opacity: 0.6;
}

.detail-text {
  word-break: break-word;
  color: var(--color-text-tertiary);
}

.detail-section {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-sm);
}

.detail-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.detail-title {
  display: inline-flex;
  align-items: center;
  gap: var(--spacing-sm);
  font-weight: 600;
}

.detail-tag {
  font-size: var(--font-xs);
}

.no-correction-tag {
  border-color: rgba(163, 230, 53, 0.35);
  color: #a3e635;
}

.detail-body {
  border: 1px solid var(--color-border);
  border-radius: var(--radius-md);
  padding: var(--spacing-md);
  background: var(--color-bg-tertiary);
  white-space: pre-wrap;
  min-height: 120px;
  font-size: var(--font-md);
}

.detail-body.failed {
  color: var(--color-text-secondary);
}

.failed-detail {
  color: #fda4af;
}

@media (max-width: 640px) {
  .row-header {
    flex-direction: column;
    align-items: stretch;
  }

  .list-actions {
    justify-content: flex-start;
    flex-wrap: wrap;
  }

  .detail-summary {
    padding: 10px;
  }

  .detail-summary-row {
    gap: 8px 10px;
  }

  .detail-item-model {
    min-width: 0;
  }
}
</style>
