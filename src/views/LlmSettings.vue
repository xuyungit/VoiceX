<script setup lang="ts">
import { computed, ref } from 'vue'
import { NInput, NSwitch, NButton, NSelect, NTabs, NTabPane } from 'naive-ui'
import { useSettingsStore, type AppSettings } from '../stores/settings'

const settingsStore = useSettingsStore()

const defaultPrompt = `你是一个语音转写文本纠正助手。

你的任务：
- 修正语音识别文本中的识别错误、同音字错误、错别字和标点问题
- 保持原意，不增删信息
- 当识别结果中出现与用户词典中词汇发音相似、拼写接近或语义相关的词时，将其替换为词典中的标准形式
- 不要更改词典中词汇的拼写、大小写或符号
- 即便识别文本中的英文和用户词典的词汇语义相似，不要用用户词典中的词汇去替换原文中的英文

用户热词词典：
{{DICTIONARY}}

输出：
纠正后的文本或原文（如果不需要任何修改），另外不要输出任何其他说明性的内容`

const defaultTranslationPrompt = `你是一个专业翻译助手。

你的任务：
- 将用户提供的原文准确翻译成英文
- 保持原意，不增删信息
- 保留专有名词、数字、代码片段与格式
- 如果原文已经是英文，只做必要润色并保持原意

输出：
只输出英文结果，不要输出解释或额外说明`

const providerOptions = [
  { label: '火山引擎 (Doubao)', value: 'volcengine' },
  { label: 'OpenAI', value: 'openai' },
  { label: '千问 (Qwen)', value: 'qwen' },
  { label: '自定义 (OpenAI 兼容)', value: 'custom' }
]

const reasoningEffortOptions = [
  { label: 'Low', value: 'low' },
  { label: 'Minimal (Default)', value: 'minimal' },
  { label: 'Medium', value: 'medium' },
  { label: 'High', value: 'high' }
]

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

function resetPrompt() {
  if (activePromptTab.value === 'assistant') {
    llmPromptTemplate.value = defaultPrompt
    return
  }
  translationPromptTemplate.value = defaultTranslationPrompt
}
</script>

<template>
  <div class="page settings-page">
    <div class="page-header">
      <h1 class="page-title">LLM</h1>
    </div>

    <div class="surface-card llm-card">
      <div class="card-header">
        <div class="card-title">AI Correction</div>
        <div class="card-sub">Uses OpenAI-compatible /chat/completions.</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Enable AI correction</div>
          </div>
          <NSwitch v-model:value="enableLlmCorrection" />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Include recent input history</div>
            <div class="field-sub">Uses last 5 inputs for system prompt context.</div>
          </div>
          <NSwitch v-model:value="enableLlmHistoryContext" />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Enable translation mode</div>
            <div class="field-sub">Used by double-tap trigger in Input settings.</div>
          </div>
          <NSwitch v-model:value="translationEnabled" />
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Provider</div>
            <div class="field-sub">Select LLM service provider</div>
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
              <div class="field-label">Base URL</div>
            </div>
            <NInput v-model:value="llmVolcengineBaseUrl" class="field-control" />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">API Key</div>
            </div>
            <NInput
              v-model:value="llmVolcengineApiKey"
              type="password"
              show-password-on="click"
              placeholder="Enter API key..."
              class="field-control"
            />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">Model Name</div>
            </div>
            <NInput v-model:value="llmVolcengineModel" class="field-control short" />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">Reasoning Effort</div>
              <div class="field-sub">Controls inference effort for Doubao models</div>
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
              <div class="field-label">Base URL</div>
            </div>
            <NInput v-model:value="llmOpenaiBaseUrl" class="field-control" />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">API Key</div>
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
              <div class="field-label">Model Name</div>
            </div>
            <NInput v-model:value="llmOpenaiModel" class="field-control short" />
          </div>
        </template>

        <!-- Qwen Settings -->
        <template v-if="isQwen">
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">Base URL</div>
            </div>
            <NInput v-model:value="llmQwenBaseUrl" class="field-control" />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">API Key</div>
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
              <div class="field-label">Model Name</div>
            </div>
            <NInput v-model:value="llmQwenModel" class="field-control short" />
          </div>
        </template>

        <!-- Custom Settings -->
        <template v-if="isCustom">
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">Base URL</div>
              <div class="field-sub">Enter your OpenAI-compatible API endpoint</div>
            </div>
            <NInput v-model:value="llmCustomBaseUrl" placeholder="https://your-api.example.com/v1" class="field-control" />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">API Key</div>
            </div>
            <NInput
              v-model:value="llmCustomApiKey"
              type="password"
              show-password-on="click"
              placeholder="Enter API key..."
              class="field-control"
            />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">Model Name</div>
            </div>
            <NInput v-model:value="llmCustomModel" placeholder="your-model-id" class="field-control short" />
          </div>
        </template>
      </div>
    </div>

    <div class="surface-card llm-card">
      <div class="card-header prompt-header">
        <div class="card-title">Prompts</div>
        <NButton size="small" quaternary @click="resetPrompt">Reset current tab</NButton>
      </div>
      <NTabs v-model:value="activePromptTab" type="line" animated>
        <NTabPane name="assistant" tab="Assistant Prompt">
          <NInput
            v-model:value="llmPromptTemplate"
            type="textarea"
            :rows="12"
            placeholder="留空使用默认模板"
          />
        </NTabPane>
        <NTabPane name="translation" tab="Translate Prompt">
          <NInput
            v-model:value="translationPromptTemplate"
            type="textarea"
            :rows="12"
            placeholder="翻译模式提示词"
          />
        </NTabPane>
      </NTabs>
      <div class="card-sub" style="margin-top: var(--spacing-sm);">
        Assistant 模板支持 {{ '{' }}{DICTIONARY}{{ '}' }} 与 {{ '{' }}{INPUT_HISTORY}{{ '}' }} 占位符；Translate 模板默认只输出英文结果。
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
