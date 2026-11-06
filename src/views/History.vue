<script setup lang="ts">
import { computed, h, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NTabs, NTabPane, NSpin, NEmpty, NSelect, NModal, NDropdown, NTooltip, NPopover, useDialog, useMessage, type DropdownOption } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useHistoryStore, finalText, type HistoryRecord } from '../stores/history'
import { usePostEditStore } from '../stores/postEdit'
import { useSyncStore } from '../stores/sync'
import { useSettingsStore } from '../stores/settings'
import { trimTrailingPunctuation } from '../utils/text'
import ReTranscribeDialog from '../components/ReTranscribeDialog.vue'
import HistoryTranslationDialog from '../components/HistoryTranslationDialog.vue'
import PostEditHistory from '../components/PostEditHistory.vue'
import InlineTextEditor from '../components/InlineTextEditor.vue'
import LoadMoreSentinel from '../components/LoadMoreSentinel.vue'

const activeTab = ref('dictations')
const historyStore = useHistoryStore()
const settingsStore = useSettingsStore()
const syncStore = useSyncStore()
const postEditStore = usePostEditStore()
const dialog = useDialog()
const message = useMessage()
const { t, locale } = useI18n()
const editingId = ref<string | null>(null)
const savingEdit = ref(false)

// The edit records tab belongs to the post-edit switch in Input settings.
watch(() => postEditStore.enabled, enabled => {
  if (!enabled && activeTab.value === 'edits') activeTab.value = 'dictations'
})

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

function retentionLabel(days: number) {
  return retentionOptions.value.find(option => option.value === days)?.label ?? t('common.days', { count: days })
}

const retentionSummary = computed(() => t('history.retentionSummary', {
  text: retentionLabel(textRetention.value),
  audio: retentionLabel(audioRetention.value),
}))

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
const translationVisible = ref(false)
const translationRecord = ref<HistoryRecord | null>(null)
const audioPlayer = ref<HTMLAudioElement | null>(null)
const playingId = ref<string | null>(null)
const objectUrl = ref<string | null>(null)
onMounted(async () => {
  await Promise.all([
    settingsStore.loadSettings(),
    historyStore.loadHistory(true),
    postEditStore.load()
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
  await copyText(finalText(record))
}

async function copyText(text: string) {
  try {
    await historyStore.copyText(text)
    message.success(t('postEdit.copied'))
  } catch (error) {
    message.error(t('history.copyFailed', { reason: String(error) }))
  }
}

function canEdit(record: HistoryRecord) {
  return !isFailedRecord(record) && hasRecordText(record)
}

async function saveEdit(record: HistoryRecord, text: string) {
  savingEdit.value = true
  try {
    await historyStore.setFinalText(record.id, text)
    editingId.value = null
  } catch (error) {
    message.error(t('postEdit.saveFailed', { reason: String(error) }))
  } finally {
    savingEdit.value = false
  }
}

async function handleDelete(record: HistoryRecord) {
  if (playingId.value === record.id) {
    stopPlayback()
  }
  try {
    await historyStore.deleteRecord(record.id)
    if (detailRecord.value?.id === record.id) closeDetails()
  } catch (error) {
    message.error(t('history.deleteFailed', { reason: String(error) }))
    return false
  }
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
      title: t('history.audioMissingTitle'),
      content: t('history.audioMissing'),
      positiveText: t('common.ok')
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
      title: t('history.playbackFailedTitle'),
      content: t('history.playbackFailed')
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

/// A translate-and-read session: the text is the translation, the original
/// is the selection it came from, and there is no recording behind it.
function isTranslateRead(record: HistoryRecord): boolean {
  return record.mode === 'translate_read'
}

function isTranslateMode(record: HistoryRecord): boolean {
  return record.mode.startsWith('translate_en') || isTranslateRead(record)
}

function isFailedRecord(record: HistoryRecord): boolean {
  return record.errorCode !== 0
}

function hasRecordText(record: HistoryRecord): boolean {
  return record.text.trim().length > 0
}

function displayText(record: HistoryRecord): string {
  if (hasRecordText(record)) {
    return finalText(record)
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
  if (isTranslateRead(record)) {
    return t('history.translateRead')
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
  await copyText(original)
}

async function openRecordingsFolder() {
  try {
    await invoke('open_recordings_dir')
  } catch (error) {
    console.error('Failed to open recordings directory:', error)
    dialog.error({
      title: t('history.openFolderFailedTitle'),
      content: t('history.openFolderFailed', { reason: error instanceof Error ? error.message : String(error) })
    })
  }
}

function confirmDelete(record: HistoryRecord) {
  dialog.error({
    title: t('history.deleteTitle'),
    content: t('history.deleteConfirm'),
    positiveText: t('common.delete'),
    negativeText: t('common.cancel'),
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
  if (isTranslateRead(record)) {
    return t('history.translateRead')
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

// Nothing was recognised for a translate-and-read; showing "not recorded"
// there would read as a missing value rather than an inapplicable one.
function asrModelLabel(record: HistoryRecord): string {
  return isTranslateRead(record) ? t('common.none') : modelLabel(record.asrModelName)
}

function originalHeading(record: HistoryRecord): string {
  return isTranslateRead(record) ? t('history.sourceText') : t('history.originalRecognition')
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
      label: t('history.translateToEnglish'),
      key: 'translate',
      disabled: isFailedRecord(record) || !hasRecordText(record),
      icon: renderIcon('M12.87 15.07 10.33 12.56l.03-.03A17.52 17.52 0 0 0 14.07 6H17V4h-7V2H8v2H1v2h11.17A15.7 15.7 0 0 1 9 11.35 15.65 15.65 0 0 1 6.69 8H4.69a17.58 17.58 0 0 0 2.98 4.56L2.58 17.58 4 19l5-5 3.11 3.11.76-2.04zM18.5 10h-2L12 22h2l1.12-3h4.75L21 22h2l-4.5-12zm-2.62 7 1.62-4.33L19.12 17h-3.24z')
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

  if (key === 'translate' && !isFailedRecord(record) && hasRecordText(record)) {
    translationRecord.value = record
    translationVisible.value = true
  }
}
</script>

<template>
  <div class="page history-page">
    <div class="page-header history-header">
      <h1 class="page-title">{{ t('history.title') }}</h1>
      <div class="header-tools">
        <NPopover trigger="click" placement="bottom-end" :show-arrow="false">
          <template #trigger>
            <NButton quaternary size="small" class="retention-button">
              <template #icon>
                <svg viewBox="0 0 24 24" fill="currentColor" width="15" height="15">
                  <path d="M13 3a9 9 0 0 0-9 9H1l3.89 3.89.07.14L9 12H6c0-3.87 3.13-7 7-7s7 3.13 7 7-3.13 7-7 7c-1.93 0-3.68-.79-4.94-2.06l-1.42 1.42A8.954 8.954 0 0 0 13 21a9 9 0 0 0 0-18zm-1 5v5l4.28 2.54.72-1.21-3.5-2.08V8H12z" />
                </svg>
              </template>
              {{ retentionSummary }}
            </NButton>
          </template>
          <div class="retention-popover">
            <div class="retention-popover-title">{{ t('history.retentionPolicy') }}</div>
            <label class="retention-field">
              <span class="retention-label">{{ t('history.text') }}</span>
              <NSelect v-model:value="textRetention" :options="retentionOptions" size="small" class="retention-select" :aria-label="t('history.text')" />
            </label>
            <label class="retention-field">
              <span class="retention-label">{{ t('history.audio') }}</span>
              <NSelect v-model:value="audioRetention" :options="retentionOptions" size="small" class="retention-select" :aria-label="t('history.audio')" />
            </label>
          </div>
        </NPopover>
        <NTooltip>
          <template #trigger>
            <NButton quaternary size="small" class="open-folder" :aria-label="t('history.openRecordingsFolder')" @click="openRecordingsFolder">
              <template #icon>
                <svg viewBox="0 0 24 24" fill="currentColor" width="16" height="16">
                  <path d="M10 4H4a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-8l-2-2z" />
                </svg>
              </template>
            </NButton>
          </template>
          {{ t('history.openRecordingsFolder') }}
        </NTooltip>
      </div>
    </div>

    <NTabs v-model:value="activeTab" type="line" class="history-tabs" :class="{ single: !postEditStore.enabled }">
      <NTabPane name="dictations" :tab="t('postEdit.allDictations')">
    <div v-if="historyStore.isLoading && historyStore.records.length === 0" class="loading-container">
      <NSpin size="medium" />
    </div>

    <div v-else-if="historyStore.records.length === 0" class="empty-container">
      <NEmpty :description="t('history.empty')" />
    </div>

    <div v-else class="list-wrapper">
      <template v-for="group in groupedRecords" :key="group.label">
        <div class="date-group">
        <div class="date-header">{{ group.label }}</div>
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
                <span v-if="record.editedText !== null" class="tag row-tag">
                  {{ t('postEdit.editedTag') }}
                </span>
                <span
                  v-if="isFailedRecord(record)"
                  class="tag row-tag failed-tag"
                >
                  {{ t('history.transcriptionFailed') }}
                </span>
              </div>
              <div class="list-actions">
                <span class="duration">{{ formatDuration(record.durationMs) }}</span>
                <span v-if="playingId === record.id" class="pill accent">{{ t('history.playing') }}</span>
                <div class="action-toolbar">
                  <NButton quaternary size="small" class="toolbar-button" :aria-label="t('common.copy')" @click="() => handleCopy(record)">
                    <template #icon>
                      <svg viewBox="0 0 24 24" fill="currentColor" width="16" height="16">
                        <path d="M16 1H4c-1.1 0-2 .9-2 2v14h2V3h12V1zm3 4H8c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h11c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm0 16H8V7h11v14z" />
                      </svg>
                    </template>
                  </NButton>
                  <NButton
                    quaternary
                    size="small"
                    class="toolbar-button"
                    :aria-label="t('postEdit.edit')"
                    :title="t('postEdit.edit')"
                    :disabled="!canEdit(record) || editingId === record.id"
                    @click="editingId = record.id"
                  >
                    <template #icon>
                      <svg viewBox="0 0 24 24" fill="currentColor" width="16" height="16">
                        <path d="M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25zM20.71 7.04a1 1 0 0 0 0-1.41l-2.34-2.34a1 1 0 0 0-1.41 0l-1.83 1.83 3.75 3.75 1.83-1.83z" />
                      </svg>
                    </template>
                  </NButton>
                  <NButton
                    quaternary
                    size="small"
                    class="toolbar-button danger"
                    :aria-label="t('common.delete')"
                    @click="() => confirmDelete(record)"
                  >
                    <template #icon>
                      <svg viewBox="0 0 24 24" fill="currentColor" width="16" height="16">
                        <path d="M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM19 4h-3.5l-1-1h-5l-1 1H5v2h14V4z" />
                      </svg>
                    </template>
                  </NButton>
                  <NDropdown
                    trigger="click"
                    placement="bottom-end"
                    :options="moreOptions(record)"
                    @select="(key) => handleMoreAction(key, record)"
                  >
                    <NButton quaternary size="small" class="toolbar-button" :aria-label="t('common.moreActions')">
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
            <InlineTextEditor
              v-if="editingId === record.id"
              :text="finalText(record)"
              :original="record.text"
              :saving="savingEdit"
              @save="text => saveEdit(record, text)"
              @cancel="editingId = null"
            />
            <div v-else class="list-text" :class="{ failed: isFailedRecord(record) }">
              {{ displayText(record) }}
            </div>
            <div v-if="isFailedRecord(record)" class="failure-note">
              {{ failureMessage(record) }}
            </div>
          </div>
        </div>
      </template>

      <div v-if="historyStore.loadError" class="load-error" role="alert">
        <span>{{ t('history.loadError', { reason: historyStore.loadError }) }}</span>
        <NButton size="small" :loading="historyStore.isLoading" @click="loadMore">{{ t('postEdit.retry') }}</NButton>
      </div>
      <LoadMoreSentinel v-else-if="historyStore.hasMore" :loading="historyStore.isLoading" @load="loadMore" />
    </div>

      </NTabPane>
      <NTabPane v-if="postEditStore.enabled" name="edits" :tab="t('postEdit.tab')" display-directive="show:lazy">
        <PostEditHistory />
      </NTabPane>
    </NTabs>

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
            <span class="detail-item detail-item-model" :title="`ASR ${asrModelLabel(detailRecord)}`">
              <svg viewBox="0 0 24 24" fill="currentColor" width="14" height="14" aria-hidden="true">
                <path d="M12 14a3 3 0 0 0 3-3V6a3 3 0 0 0-6 0v5a3 3 0 0 0 3 3zm5-3a5 5 0 0 1-10 0H5a7 7 0 0 0 6 6.92V21h2v-3.08A7 7 0 0 0 19 11z" />
              </svg>
              <span class="detail-text">{{ asrModelLabel(detailRecord) }}</span>
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
              <span v-if="detailRecord.editedText !== null" class="tag detail-tag">
                {{ t('postEdit.editedTag') }}
              </span>
              <!-- Describes the LLM step, so it sits on whichever section shows its output. -->
              <span v-else-if="showNoCorrectionTag(detailRecord)" class="tag detail-tag no-correction-tag">
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

        <div v-if="detailRecord.editedText !== null" class="detail-section">
          <div class="detail-header">
            <div class="detail-title">
              {{ t('postEdit.beforeEdit') }}
              <span v-if="showNoCorrectionTag(detailRecord)" class="tag detail-tag no-correction-tag">
                {{ t('history.noCorrection') }}
              </span>
            </div>
            <NButton quaternary size="tiny" @click="copyText(detailRecord.text)">
              {{ t('common.copy') }}
            </NButton>
          </div>
          <div class="detail-body muted">{{ detailRecord.text }}</div>
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
            <div class="detail-title">{{ originalHeading(detailRecord) }}</div>
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
    <HistoryTranslationDialog
      v-model:show="translationVisible"
      :record="translationRecord"
    />
  </div>
</template>

<style scoped>
.history-header {
  flex-direction: row;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-md);
}

.header-tools {
  display: flex;
  align-items: center;
  gap: 2px;
  min-width: 0;
}

.retention-button {
  color: var(--color-text-secondary);
}

.retention-popover {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-sm);
  padding: 4px 2px;
}

.retention-popover-title {
  font-weight: 600;
  margin-bottom: 2px;
}

.retention-field {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-lg);
}

.retention-label {
  color: var(--color-text-secondary);
  white-space: nowrap;
}

.retention-select {
  width: 120px;
}

.history-tabs { min-width: 0; }
.history-tabs :deep(.n-tab-pane) { padding-top: 8px; }
/* Only the dictation list is left when edit tracking is off: no tab bar. */
.history-tabs.single :deep(.n-tabs-nav) { display: none; }
.history-tabs.single :deep(.n-tab-pane) { padding-top: 0; }

.loading-container,
.empty-container {
  display: flex;
  justify-content: center;
  align-items: center;
  padding: var(--spacing-2xl);
}

/* A plain list: no panels, records separated by a hairline. */
.list-wrapper {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-md);
  margin: 0 -10px;
}

.date-header {
  position: sticky;
  top: 0;
  z-index: 1;
  padding: 8px 10px 4px;
  font-size: var(--font-sm);
  color: var(--color-text-tertiary);
  background: var(--color-bg-primary);
}

.list-row {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 10px 10px 12px;
  border-radius: var(--radius-md);
}

.list-row + .list-row::before {
  content: '';
  position: absolute;
  top: 0;
  left: 10px;
  right: 10px;
  border-top: 1px solid var(--color-divider);
}

.list-row:hover {
  background: var(--color-bg-hover);
}

.list-row:hover::before,
.list-row:hover + .list-row::before {
  border-color: transparent;
}

.row-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-md);
  width: 100%;
  min-height: 26px;
}

.row-meta {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
  flex: 1;
  flex-wrap: wrap;
  font-size: var(--font-sm);
  font-variant-numeric: tabular-nums;
}

.row-time {
  color: var(--color-text-secondary);
  white-space: nowrap;
}

.device-meta {
  color: var(--color-text-tertiary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.row-tag {
  flex-shrink: 0;
}

.list-text {
  min-width: 0;
  font-size: var(--font-lg);
  line-height: 1.65;
  overflow-wrap: anywhere;
  user-select: text;
}

.list-text.failed {
  color: var(--color-text-secondary);
}

.failure-note {
  font-size: var(--font-sm);
  color: var(--color-error);
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

.duration {
  font-size: var(--font-sm);
  color: var(--color-text-tertiary);
  font-variant-numeric: tabular-nums;
}

/* Actions stay out of the way until the row is hovered or focused. */
.action-toolbar {
  display: inline-flex;
  align-items: center;
  gap: 0;
  opacity: 0;
  transition: opacity var(--transition-fast);
}

.list-row:hover .action-toolbar,
.list-row:focus-within .action-toolbar {
  opacity: 1;
}

.toolbar-button {
  width: 28px;
  padding: 0;
  color: var(--color-text-secondary);
}

.toolbar-button.danger:hover {
  color: var(--color-error);
}

.mode-tag.translate {
  background: var(--color-accent-light);
  color: var(--color-accent);
}

.failed-tag {
  background: color-mix(in srgb, var(--color-error) 14%, transparent);
  color: var(--color-error);
}

.load-error {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-md);
  color: var(--color-error);
  overflow-wrap: anywhere;
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
  border-radius: var(--radius-md);
  background: var(--color-bg-tertiary);
}

.detail-summary-row {
  display: flex;
  flex-wrap: wrap;
  gap: 8px 16px;
  align-items: center;
}

.detail-summary-models {
  padding-top: 8px;
  border-top: 1px solid var(--color-divider);
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
  background: color-mix(in srgb, var(--color-success) 14%, transparent);
  color: var(--color-success);
}

.detail-body {
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
  color: var(--color-error);
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
