<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NModal, NSelect, NSwitch, NSpin } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'
import type { HistoryRecord } from '../stores/history'
import { buildAsrProviderOptions } from '../utils/providerOptions'
import { buildLlmProviderOptions } from '../utils/llmOptions'

const props = defineProps<{
  show: boolean
  record: HistoryRecord | null
}>()

const emit = defineEmits<{
  'update:show': [value: boolean]
}>()

const settingsStore = useSettingsStore()
const { t } = useI18n()

const asrProvider = ref(settingsStore.settings.asrProviderType)
const enableLlm = ref(settingsStore.settings.enableLlmCorrection)
const llmProvider = ref(settingsStore.settings.llmProviderType)

const isRunning = ref(false)
const runningAction = ref<'transcribe' | 'replayInject' | null>(null)
const error = ref<string | null>(null)
const result = ref<{
  asrText: string
  llmText: string | null
  finalText: string
  asrModelName: string
  llmModelName: string | null
  targetAppName?: string | null
  injectionMode?: string | null
} | null>(null)

const asrProviderOptions = computed(() => buildAsrProviderOptions(t))
const llmProviderOptions = computed(() => buildLlmProviderOptions(t))

// Reset state when dialog opens
watch(() => props.show, (visible) => {
  if (visible) {
    asrProvider.value = settingsStore.settings.asrProviderType
    enableLlm.value = settingsStore.settings.enableLlmCorrection
    llmProvider.value = settingsStore.settings.llmProviderType
    error.value = null
    result.value = null
    isRunning.value = false
    runningAction.value = null
  }
})

async function startTranscribe() {
  if (!props.record?.audioPath) return

  isRunning.value = true
  runningAction.value = 'transcribe'
  error.value = null
  result.value = null

  try {
    const res = await invoke<{
      asrText: string
      llmText: string | null
      finalText: string
      asrModelName: string
      llmModelName: string | null
    }>('re_transcribe', {
      request: {
        audioPath: props.record.audioPath,
        asrProvider: asrProvider.value,
        enableLlm: enableLlm.value,
        llmProvider: enableLlm.value ? llmProvider.value : null,
        historyMode: props.record.mode
      }
    })
    result.value = res
  } catch (e) {
    error.value = String(e)
  } finally {
    isRunning.value = false
    runningAction.value = null
  }
}

async function startReplayInject() {
  if (!props.record?.audioPath) return

  isRunning.value = true
  runningAction.value = 'replayInject'
  error.value = null
  result.value = null

  try {
    const res = await invoke<{
      asrText: string
      llmText: string | null
      finalText: string
      asrModelName: string
      llmModelName: string | null
      targetAppName: string | null
      injectionMode: string
    }>('replay_history_injection', {
      request: {
        audioPath: props.record.audioPath,
        asrProvider: asrProvider.value,
        enableLlm: enableLlm.value,
        llmProvider: enableLlm.value ? llmProvider.value : null,
        historyMode: props.record.mode,
        delayMs: 3000
      }
    })
    result.value = res
  } catch (e) {
    error.value = String(e)
  } finally {
    isRunning.value = false
    runningAction.value = null
  }
}

async function cancelTranscribe() {
  try {
    await invoke('cancel_retranscribe')
  } catch {
    // ignore
  }
}

async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text)
  } catch {
    // fallback
  }
}

function hasOriginalText(record: HistoryRecord): boolean {
  return record.text.trim().length > 0
}

function originalRecordBody(record: HistoryRecord): string {
  if (hasOriginalText(record)) {
    return record.text
  }
  const message = record.errorMessage?.trim()
  return message && message.length > 0
    ? message
    : t('history.reTranscribeFailedPlaceholder')
}

function close() {
  if (isRunning.value) {
    void cancelTranscribe()
  }
  emit('update:show', false)
}
</script>

<template>
  <NModal
    :show="show"
    preset="card"
    :title="t('history.reTranscribe')"
    style="max-width: 600px;"
    :mask-closable="!isRunning"
    :close-on-esc="!isRunning"
    @update:show="close"
    @after-leave="() => { result = null; error = null; }"
  >
    <div class="retranscribe-panel">
      <!-- Settings -->
      <div class="settings-section">
        <div class="setting-row">
          <span class="setting-label">{{ t('history.reTranscribeAsrModel') }}</span>
          <NSelect
            v-model:value="asrProvider"
            :options="asrProviderOptions"
            :disabled="isRunning"
            size="small"
            style="width: 200px;"
          />
        </div>
        <div class="setting-row">
          <span class="setting-label">{{ t('history.reTranscribeLlmCorrection') }}</span>
          <NSwitch v-model:value="enableLlm" :disabled="isRunning" />
        </div>
        <div v-if="enableLlm" class="setting-row">
          <span class="setting-label">{{ t('history.reTranscribeLlmModel') }}</span>
          <NSelect
            v-model:value="llmProvider"
            :options="llmProviderOptions"
            :disabled="isRunning"
            size="small"
            style="width: 200px;"
          />
        </div>
      </div>

      <!-- Action -->
      <div class="action-section">
        <template v-if="!isRunning">
          <NButton
            type="primary"
            :disabled="!record?.audioPath"
            @click="startTranscribe"
          >
            {{ t('history.reTranscribeStart') }}
          </NButton>
          <NButton
            secondary
            :disabled="!record?.audioPath"
            @click="startReplayInject"
          >
            {{ t('history.replayInjectStart') }}
          </NButton>
        </template>
        <NButton
          v-else
          type="warning"
          @click="cancelTranscribe"
        >
          <template #icon>
            <NSpin :size="14" />
          </template>
          {{ runningAction === 'replayInject' ? t('history.replayInjectCancel') : t('history.reTranscribeCancel') }}
        </NButton>
      </div>

      <div v-if="isRunning && runningAction === 'replayInject'" class="hint-section">
        {{ t('history.replayInjectHint') }}
      </div>

      <!-- Error -->
      <div v-if="error" class="error-section">
        {{ error }}
      </div>

      <!-- Original text from history record -->
      <div v-if="record" class="result-block original-block">
          <div class="result-header">
            <div class="result-title">
              {{ t('history.originalRecord') }}
              <span v-if="record.asrModelName" class="result-model">{{ record.asrModelName }}<template v-if="record.llmModelName"> + {{ record.llmModelName }}</template></span>
            </div>
          <NButton quaternary size="tiny" :disabled="!hasOriginalText(record)" @click="copyText(record.text)">
            {{ t('common.copy') }}
          </NButton>
        </div>
        <div class="result-body muted">
          {{ originalRecordBody(record) }}
        </div>
      </div>

      <!-- Results -->
      <div v-if="result" class="results-section">
        <div class="result-block">
          <div class="result-header">
            <div class="result-title">
              {{ t('history.asrRecognitionResult') }}
              <span class="result-model">{{ result.asrModelName }}</span>
            </div>
            <NButton quaternary size="tiny" @click="copyText(result.asrText)">
              {{ t('common.copy') }}
            </NButton>
          </div>
          <div class="result-body">
            {{ result.asrText }}
          </div>
        </div>

        <div v-if="result.llmText !== null" class="result-block">
          <div class="result-header">
            <div class="result-title">
              {{ t('history.llmCorrectionResult') }}
              <span v-if="result.llmModelName" class="result-model">{{ result.llmModelName }}</span>
            </div>
            <NButton quaternary size="tiny" @click="copyText(result.llmText!)">
              {{ t('common.copy') }}
            </NButton>
          </div>
          <div class="result-body">
            {{ result.llmText }}
          </div>
        </div>

        <div class="result-block">
          <div class="result-header">
            <div class="result-title">
              {{ t('history.finalText') }}
              <span v-if="result.targetAppName || result.injectionMode" class="result-model">
                <template v-if="result.targetAppName">{{ result.targetAppName }}</template>
                <template v-if="result.targetAppName && result.injectionMode"> · </template>
                <template v-if="result.injectionMode">{{ result.injectionMode }}</template>
              </span>
            </div>
            <NButton quaternary size="tiny" @click="copyText(result.finalText)">
              {{ t('common.copy') }}
            </NButton>
          </div>
          <div class="result-body">
            {{ result.finalText }}
          </div>
        </div>
      </div>
    </div>
  </NModal>
</template>

<style scoped>
.retranscribe-panel {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-lg);
}

.settings-section {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-md);
}

.setting-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-md);
}

.setting-label {
  font-size: var(--font-sm);
  color: var(--color-text-secondary);
  flex-shrink: 0;
}

.action-section {
  display: flex;
  justify-content: center;
  gap: var(--spacing-md);
  flex-wrap: wrap;
}

.hint-section {
  padding: var(--spacing-md);
  border-radius: var(--radius-md);
  border: 1px solid rgba(96, 165, 250, 0.35);
  background: rgba(96, 165, 250, 0.08);
  color: var(--color-text-secondary);
  font-size: var(--font-sm);
}

.error-section {
  padding: var(--spacing-md);
  border-radius: var(--radius-md);
  border: 1px solid rgba(248, 113, 113, 0.4);
  background: rgba(248, 113, 113, 0.08);
  color: #f87171;
  font-size: var(--font-sm);
}

.results-section {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-lg);
}

.result-block {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-sm);
}

.result-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.result-title {
  display: inline-flex;
  align-items: center;
  gap: var(--spacing-sm);
  font-weight: 600;
  font-size: var(--font-sm);
}

.result-model {
  font-weight: 400;
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
}

.result-body {
  border: 1px solid var(--color-border);
  border-radius: var(--radius-md);
  padding: var(--spacing-md);
  background: var(--color-bg-tertiary);
  white-space: pre-wrap;
  min-height: 80px;
  font-size: var(--font-md);
}

.result-body.muted {
  color: var(--color-text-tertiary);
}

.original-block {
  padding-bottom: var(--spacing-md);
  border-bottom: 1px solid var(--color-border);
}
</style>
