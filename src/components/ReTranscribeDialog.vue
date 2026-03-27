<script setup lang="ts">
import { ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NModal, NSelect, NSwitch, NSpin } from 'naive-ui'
import { useSettingsStore } from '../stores/settings'
import type { HistoryRecord } from '../stores/history'

const props = defineProps<{
  show: boolean
  record: HistoryRecord | null
}>()

const emit = defineEmits<{
  'update:show': [value: boolean]
}>()

const settingsStore = useSettingsStore()

const asrProvider = ref(settingsStore.settings.asrProviderType)
const enableLlm = ref(settingsStore.settings.enableLlmCorrection)
const llmProvider = ref(settingsStore.settings.llmProviderType)

const isRunning = ref(false)
const error = ref<string | null>(null)
const result = ref<{
  asrText: string
  llmText: string | null
  asrModelName: string
  llmModelName: string | null
} | null>(null)

const asrProviderOptions = [
  { label: '火山引擎', value: 'volcengine' },
  { label: 'Google', value: 'google' },
  { label: 'Qwen', value: 'qwen' },
  { label: 'Gemini', value: 'gemini' },
  { label: 'Gemini Live', value: 'gemini-live' },
  { label: 'Cohere', value: 'cohere' },
  { label: '本地 (Coli)', value: 'coli' },
]

const llmProviderOptions = [
  { label: '火山引擎 (Doubao)', value: 'volcengine' },
  { label: 'OpenAI', value: 'openai' },
  { label: 'Qwen', value: 'qwen' },
  { label: '自定义', value: 'custom' },
]

// Reset state when dialog opens
watch(() => props.show, (visible) => {
  if (visible) {
    asrProvider.value = settingsStore.settings.asrProviderType
    enableLlm.value = settingsStore.settings.enableLlmCorrection
    llmProvider.value = settingsStore.settings.llmProviderType
    error.value = null
    result.value = null
    isRunning.value = false
  }
})

async function startTranscribe() {
  if (!props.record?.audioPath) return

  isRunning.value = true
  error.value = null
  result.value = null

  try {
    const res = await invoke<{
      asrText: string
      llmText: string | null
      asrModelName: string
      llmModelName: string | null
    }>('re_transcribe', {
      request: {
        audioPath: props.record.audioPath,
        asrProvider: asrProvider.value,
        enableLlm: enableLlm.value,
        llmProvider: enableLlm.value ? llmProvider.value : null,
      }
    })
    result.value = res
  } catch (e) {
    error.value = String(e)
  } finally {
    isRunning.value = false
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
    title="重新转录"
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
          <span class="setting-label">ASR 模型</span>
          <NSelect
            v-model:value="asrProvider"
            :options="asrProviderOptions"
            :disabled="isRunning"
            size="small"
            style="width: 200px;"
          />
        </div>
        <div class="setting-row">
          <span class="setting-label">LLM 纠错</span>
          <NSwitch v-model:value="enableLlm" :disabled="isRunning" />
        </div>
        <div v-if="enableLlm" class="setting-row">
          <span class="setting-label">LLM 模型</span>
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
        <NButton
          v-if="!isRunning"
          type="primary"
          :disabled="!record?.audioPath"
          @click="startTranscribe"
        >
          开始转录
        </NButton>
        <NButton
          v-else
          type="warning"
          @click="cancelTranscribe"
        >
          <template #icon>
            <NSpin :size="14" />
          </template>
          取消
        </NButton>
      </div>

      <!-- Error -->
      <div v-if="error" class="error-section">
        {{ error }}
      </div>

      <!-- Original text from history record -->
      <div v-if="record" class="result-block original-block">
        <div class="result-header">
          <div class="result-title">
            原始记录
            <span v-if="record.asrModelName" class="result-model">{{ record.asrModelName }}<template v-if="record.llmModelName"> + {{ record.llmModelName }}</template></span>
          </div>
          <NButton quaternary size="tiny" @click="copyText(record.text)">
            复制
          </NButton>
        </div>
        <div class="result-body muted">
          {{ record.text }}
        </div>
      </div>

      <!-- Results -->
      <div v-if="result" class="results-section">
        <div class="result-block">
          <div class="result-header">
            <div class="result-title">
              ASR 识别结果
              <span class="result-model">{{ result.asrModelName }}</span>
            </div>
            <NButton quaternary size="tiny" @click="copyText(result.asrText)">
              复制
            </NButton>
          </div>
          <div class="result-body">
            {{ result.asrText }}
          </div>
        </div>

        <div v-if="result.llmText !== null" class="result-block">
          <div class="result-header">
            <div class="result-title">
              LLM 纠错结果
              <span v-if="result.llmModelName" class="result-model">{{ result.llmModelName }}</span>
            </div>
            <NButton quaternary size="tiny" @click="copyText(result.llmText!)">
              复制
            </NButton>
          </div>
          <div class="result-body">
            {{ result.llmText }}
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
