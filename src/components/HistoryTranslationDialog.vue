<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NModal, NSpin, useMessage } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import type { HistoryRecord } from '../stores/history'

const props = defineProps<{
  show: boolean
  record: HistoryRecord | null
}>()

const emit = defineEmits<{
  'update:show': [value: boolean]
}>()

type TranslationResult = { text: string; modelName: string | null }
const { t } = useI18n()
const message = useMessage()
const result = ref<TranslationResult | null>(null)
const error = ref<string | null>(null)
const isRunning = ref(false)
const wasCancelled = ref(false)
let activeRequestId: string | null = null

function errorReason(value: unknown): string {
  if (value instanceof Error) return value.message
  if (typeof value === 'object' && value !== null && 'message' in value) {
    return String(value.message)
  }
  return typeof value === 'string' ? value : JSON.stringify(value) ?? String(value)
}

function translationError(value: unknown): string {
  const code = typeof value === 'object' && value !== null && 'code' in value ? value.code : null
  switch (code) {
    case 'empty_text': return t('history.translationEmpty')
    case 'not_configured': return t('history.translationNotConfigured')
    case 'empty_prompt': return t('history.translationEmptyPrompt')
    case 'timeout': return t('history.translationTimeout')
    case 'cancelled': return t('history.translationCancelled')
    default: return t('history.translationFailed', { reason: errorReason(value) })
  }
}

function cancelTranslation() {
  const requestId = activeRequestId
  // Invalidate first: a late response must never update a closed/reopened
  // dialog, or replace the result of a newer attempt.
  activeRequestId = null
  isRunning.value = false
  if (!requestId) return
  wasCancelled.value = true
  void invoke('cancel_history_translation', { requestId }).catch((err: unknown) => {
    console.error('Failed to cancel history translation:', err)
    if (props.show) {
      message.error(t('history.translationCancelFailed', { reason: errorReason(err) }))
    }
  })
}

async function startTranslation() {
  if (isRunning.value || !props.show || !props.record) return
  const requestId = crypto.randomUUID()
  activeRequestId = requestId
  isRunning.value = true
  wasCancelled.value = false
  error.value = null
  result.value = null

  try {
    const translated = await invoke<TranslationResult>('translate_history_text', {
      text: props.record.text,
      requestId
    })
    if (activeRequestId === requestId) result.value = translated
  } catch (err) {
    if (activeRequestId === requestId) error.value = translationError(err)
  } finally {
    if (activeRequestId === requestId) {
      activeRequestId = null
      isRunning.value = false
    }
  }
}

async function copyTranslation() {
  if (!result.value) return
  try {
    await navigator.clipboard.writeText(result.value.text)
    message.success(t('history.translationCopied'))
  } catch (err) {
    message.error(t('history.translationCopyFailed', { reason: errorReason(err) }))
  }
}

function close() {
  cancelTranslation()
  emit('update:show', false)
}

watch(() => [props.show, props.record?.id] as const, ([visible]) => {
  cancelTranslation()
  result.value = null
  error.value = null
  wasCancelled.value = false
  if (visible) void startTranslation()
}, { immediate: true })

onBeforeUnmount(cancelTranslation)
</script>

<template>
  <NModal
    :show="show"
    preset="card"
    :title="t('history.translateToEnglish')"
    style="width: min(760px, calc(100vw - 48px));"
    @update:show="close"
  >
    <div class="translation-panel">
      <section class="text-section">
        <h3 class="section-title">{{ t('history.sourceText') }}</h3>
        <div class="text-body source-body" tabindex="0">{{ record?.text }}</div>
      </section>

      <section class="text-section">
        <div class="translation-heading">
          <h3 class="section-title">{{ t('history.englishTranslation') }}</h3>
          <span v-if="result?.modelName" class="model-name">{{ result.modelName }}</span>
        </div>
        <div v-if="isRunning" class="translation-status" role="status" aria-live="polite">
          <NSpin :size="20" />
          <span>{{ t('history.translating') }}</span>
        </div>
        <div v-else-if="error" class="translation-error" role="alert">{{ error }}</div>
        <div v-else-if="wasCancelled" class="translation-status" role="status">
          {{ t('history.translationCancelled') }}
        </div>
        <div v-else-if="result" class="text-body translated-body" tabindex="0">{{ result.text }}</div>
      </section>
    </div>

    <template #footer>
      <div class="translation-actions">
        <NButton @click="close">{{ t('history.translationClose') }}</NButton>
        <NButton v-if="isRunning" @click="cancelTranslation">{{ t('history.translationCancel') }}</NButton>
        <NButton v-else @click="startTranslation">{{ t('history.translationRetry') }}</NButton>
        <NButton type="primary" :disabled="!result" @click="copyTranslation">
          {{ t('history.translationCopyAll') }}
        </NButton>
      </div>
    </template>
  </NModal>
</template>

<style scoped>
.translation-panel {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-lg);
  max-height: calc(100vh - 220px);
  overflow-y: auto;
}

.text-section {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-sm);
  flex-shrink: 0;
}

.translation-heading {
  display: flex;
  align-items: baseline;
  flex-wrap: wrap;
  gap: var(--spacing-sm);
}

.section-title {
  font-size: var(--font-sm);
  font-weight: 600;
}

.model-name {
  color: var(--color-text-tertiary);
  font-size: var(--font-xs);
  overflow-wrap: anywhere;
}

.text-body,
.translation-error {
  padding: var(--spacing-md);
  border: 1px solid var(--color-border);
  border-radius: var(--radius-md);
  background: var(--color-bg-tertiary);
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  user-select: text;
  -webkit-user-select: text;
  cursor: text;
}

.source-body {
  max-height: 160px;
  overflow-y: auto;
  color: var(--color-text-secondary);
}

.translated-body {
  min-height: 120px;
}

.translation-status {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--spacing-sm);
  min-height: 120px;
  color: var(--color-text-secondary);
}

.translation-error {
  color: var(--color-error);
  border-color: color-mix(in srgb, var(--color-error) 40%, transparent);
}

.translation-actions {
  display: flex;
  justify-content: flex-end;
  flex-wrap: wrap;
  gap: var(--spacing-sm);
}
</style>
