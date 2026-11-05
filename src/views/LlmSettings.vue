<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NInput, NAutoComplete, NSwitch, NButton, NSelect, NTabs, NTabPane, NTooltip, useDialog } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import type { ResolvedLocale } from '../i18n'
import { useSettingsStore, type AppSettings, type CustomLlmEndpoint } from '../stores/settings'
import { getDefaultPrompt } from '../utils/llmPrompts'
import {
  buildLlmApiModeOptions,
  buildLlmProviderOptions,
  providerSelectionKey,
  ADD_CUSTOM_ENDPOINT_VALUE,
  CUSTOM_ENDPOINT_PREFIX,
  type LlmProviderValue
} from '../utils/llmOptions'
import SettingsSection from '../components/settings/SettingsSection.vue'
import SettingRow from '../components/settings/SettingRow.vue'
import ProbeStatus from '../components/settings/ProbeStatus.vue'
import { diffChars } from '../utils/textDiff'

const settingsStore = useSettingsStore()
const { t, locale } = useI18n()

interface LlmProviderProbeResult {
  provider: string
  ok: boolean
  responseTimeMs: number | null
  requestText: string
  responseText: string
  expectedMatch: boolean
  errorMessage: string | null
}

interface LlmReasoningPreview {
  // Compact JSON of the fields, '' when nothing is sent.
  fields: string
  source: 'lowest' | 'not_needed' | 'unknown' | 'chosen'
}

const providerOptions = computed(() =>
  buildLlmProviderOptions(t, settingsStore.settings.llmCustomEndpoints, { includeAddAction: true })
)
const apiModeOptions = computed(() => buildLlmApiModeOptions(t))

// '' is the default everywhere: the backend spells "lowest" per vendor and
// model (`llm/reasoning.rs`). Ark has no level below `low` that every model
// honors, so its list starts there.
const reasoningEffortOptions = computed(() => [
  { label: t('llm.reasoningLowest'), value: '' },
  { label: t('llm.low'), value: 'low' },
  { label: t('llm.medium'), value: 'medium' },
  { label: t('llm.high'), value: 'high' }
])

// OpenAI-style endpoints: each vendor accepts its own subset of the levels,
// and `server_default` sends no reasoning field at all.
const optionalReasoningEffortOptions = computed(() => [
  { label: t('llm.reasoningLowest'), value: '' },
  { label: t('llm.reasoningServerDefault'), value: 'server_default' },
  { label: t('llm.reasoningNone'), value: 'none' },
  { label: t('llm.reasoningMinimal'), value: 'minimal' },
  { label: t('llm.low'), value: 'low' },
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
  get: () => settingsStore.settings.llmVolcengineReasoningEffort ?? '',
  set: (v: string) => settingsStore.updateSetting('llmVolcengineReasoningEffort', v || null)
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
const llmOpenaiReasoningEffort = computed({
  get: () => settingsStore.settings.llmOpenaiReasoningEffort ?? '',
  set: (v: string) => settingsStore.updateSetting('llmOpenaiReasoningEffort', v || null)
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

// DeepSeek-specific
const llmDeepseekBaseUrl = computed({
  get: () => settingsStore.settings.llmDeepseekBaseUrl,
  set: (v: string) => settingsStore.updateSetting('llmDeepseekBaseUrl', v)
})
const llmDeepseekApiKey = computed({
  get: () => settingsStore.settings.llmDeepseekApiKey,
  set: (v: string) => settingsStore.updateSetting('llmDeepseekApiKey', v)
})
const llmDeepseekModel = computed({
  get: () => settingsStore.settings.llmDeepseekModel,
  set: (v: string) => settingsStore.updateSetting('llmDeepseekModel', v)
})

// Gemini-specific
const llmGeminiBaseUrl = computed({
  get: () => settingsStore.settings.llmGeminiBaseUrl,
  set: (v: string) => settingsStore.updateSetting('llmGeminiBaseUrl', v)
})
const llmGeminiApiKey = computed({
  get: () => settingsStore.settings.llmGeminiApiKey,
  set: (v: string) => settingsStore.updateSetting('llmGeminiApiKey', v)
})
const llmGeminiModel = computed({
  get: () => settingsStore.settings.llmGeminiModel,
  set: (v: string) => settingsStore.updateSetting('llmGeminiModel', v)
})

// Anthropic-specific
const llmAnthropicBaseUrl = computed({
  get: () => settingsStore.settings.llmAnthropicBaseUrl,
  set: (v: string) => settingsStore.updateSetting('llmAnthropicBaseUrl', v)
})
const llmAnthropicApiKey = computed({
  get: () => settingsStore.settings.llmAnthropicApiKey,
  set: (v: string) => settingsStore.updateSetting('llmAnthropicApiKey', v)
})
const llmAnthropicModel = computed({
  get: () => settingsStore.settings.llmAnthropicModel,
  set: (v: string) => settingsStore.updateSetting('llmAnthropicModel', v)
})

// Custom-specific: the currently selected named endpoint within llmCustomEndpoints.
const activeCustomEndpoint = computed<CustomLlmEndpoint | null>(() => {
  const { llmCustomEndpoints, llmActiveCustomEndpointId } = settingsStore.settings
  return (
    llmCustomEndpoints.find((endpoint) => endpoint.id === llmActiveCustomEndpointId) ??
    llmCustomEndpoints[0] ??
    null
  )
})

const llmCustomName = computed({
  get: () => activeCustomEndpoint.value?.name ?? '',
  set: (v: string) => {
    if (activeCustomEndpoint.value) activeCustomEndpoint.value.name = v
  }
})
const llmCustomBaseUrl = computed({
  get: () => activeCustomEndpoint.value?.baseUrl ?? '',
  set: (v: string) => {
    if (activeCustomEndpoint.value) activeCustomEndpoint.value.baseUrl = v
  }
})
const llmCustomApiKey = computed({
  get: () => activeCustomEndpoint.value?.apiKey ?? '',
  set: (v: string) => {
    if (activeCustomEndpoint.value) activeCustomEndpoint.value.apiKey = v
  }
})
const llmCustomModel = computed({
  get: () => activeCustomEndpoint.value?.model ?? '',
  set: (v: string) => {
    if (activeCustomEndpoint.value) activeCustomEndpoint.value.model = v
  }
})
const llmCustomApiMode = computed({
  get: () => activeCustomEndpoint.value?.apiMode ?? 'chat_completions',
  set: (v: CustomLlmEndpoint['apiMode']) => {
    if (activeCustomEndpoint.value) activeCustomEndpoint.value.apiMode = v
  }
})
const llmCustomReasoningEffort = computed({
  get: () => activeCustomEndpoint.value?.reasoningEffort ?? '',
  set: (v: string) => {
    if (activeCustomEndpoint.value) activeCustomEndpoint.value.reasoningEffort = v
  }
})
const llmCustomExtraBody = computed({
  get: () => activeCustomEndpoint.value?.extraBody ?? '',
  set: (v: string) => {
    if (activeCustomEndpoint.value) activeCustomEndpoint.value.extraBody = v
  }
})
// The backend refuses to send a request whose extra fields do not parse, so
// say so here, where the user can still fix it.
const llmCustomExtraBodyError = computed(() => {
  const raw = llmCustomExtraBody.value.trim()
  if (!raw) return ''
  try {
    const parsed: unknown = JSON.parse(raw)
    if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return t('llm.extraBodyNotObject')
    }
    return ''
  } catch (error) {
    return `${t('llm.extraBodyInvalid')}: ${(error as Error).message}`
  }
})

// Dropdown selection: maps built-in providers and `custom:<id>` keys onto the
// persisted (llmProviderType, llmActiveCustomEndpointId) pair.
const selectedProviderKey = computed({
  get: () =>
    providerSelectionKey(
      settingsStore.settings.llmProviderType,
      settingsStore.settings.llmCustomEndpoints,
      settingsStore.settings.llmActiveCustomEndpointId
    ),
  set: (key: string) => {
    if (key === ADD_CUSTOM_ENDPOINT_VALUE) {
      addCustomEndpoint()
      return
    }
    if (key.startsWith(CUSTOM_ENDPOINT_PREFIX)) {
      settingsStore.updateSetting('llmProviderType', 'custom')
      settingsStore.updateSetting('llmActiveCustomEndpointId', key.slice(CUSTOM_ENDPOINT_PREFIX.length))
      return
    }
    settingsStore.updateSetting('llmProviderType', key as LlmProviderValue)
  }
})

function addCustomEndpoint() {
  const endpoints = settingsStore.settings.llmCustomEndpoints
  const id = crypto.randomUUID()
  endpoints.push({
    id,
    name: `${t('llm.customDefaultEndpointName')} ${endpoints.length + 1}`,
    baseUrl: '',
    apiKey: '',
    model: '',
    apiMode: 'chat_completions',
    reasoningEffort: '',
    extraBody: ''
  })
  settingsStore.updateSetting('llmProviderType', 'custom')
  settingsStore.updateSetting('llmActiveCustomEndpointId', id)
}

const dialog = useDialog()

function confirmDeleteActiveCustomEndpoint() {
  const endpoint = activeCustomEndpoint.value
  if (!endpoint) return
  dialog.error({
    title: t('llm.deleteCustomEndpoint'),
    content: t('llm.deleteCustomEndpointConfirm', { name: endpoint.name }),
    positiveText: t('common.delete'),
    negativeText: t('common.cancel'),
    onPositiveClick: deleteActiveCustomEndpoint,
  })
}

function deleteActiveCustomEndpoint() {
  const endpoints = settingsStore.settings.llmCustomEndpoints
  const index = endpoints.findIndex((endpoint) => endpoint.id === activeCustomEndpoint.value?.id)
  if (index === -1) return
  endpoints.splice(index, 1)
  if (endpoints.length > 0) {
    const next = endpoints[Math.max(0, index - 1)]
    settingsStore.updateSetting('llmActiveCustomEndpointId', next.id)
  } else {
    settingsStore.updateSetting('llmActiveCustomEndpointId', '')
    settingsStore.updateSetting('llmProviderType', 'volcengine')
  }
}

const isVolcengine = computed(() => llmProviderType.value === 'volcengine')
const isOpenai = computed(() => llmProviderType.value === 'openai')
const isQwen = computed(() => llmProviderType.value === 'qwen')
const isDeepseek = computed(() => llmProviderType.value === 'deepseek')
const isGemini = computed(() => llmProviderType.value === 'gemini')
const isAnthropic = computed(() => llmProviderType.value === 'anthropic')
const isCustom = computed(() => llmProviderType.value === 'custom')
const activePromptTab = ref<'assistant' | 'translation'>('assistant')
const llmProbeLoading = ref(false)
const llmProbeResult = ref<LlmProviderProbeResult | null>(null)
const llmProbeError = ref('')
const openaiModels = ref<string[]>([])
const openaiModelsLoading = ref(false)
const openaiModelsError = ref('')
let openaiModelsRequest = 0
watch([llmOpenaiBaseUrl, llmOpenaiApiKey], () => {
  openaiModelsRequest++
  openaiModels.value = []
  openaiModelsError.value = ''
  openaiModelsLoading.value = false
})
async function loadOpenaiModels() {
  const request = ++openaiModelsRequest
  openaiModelsLoading.value = true
  openaiModelsError.value = ''
  try {
    const models = await invoke<string[]>('list_openai_models', {
      baseUrl: llmOpenaiBaseUrl.value, apiKey: llmOpenaiApiKey.value
    })
    if (request === openaiModelsRequest) openaiModels.value = models
  } catch (error) {
    if (request === openaiModelsRequest) openaiModelsError.value = String(error)
  } finally {
    if (request === openaiModelsRequest) openaiModelsLoading.value = false
  }
}

// The reasoning fields the selected provider will be sent, resolved by the
// backend from the page's current values, saved or not.
const reasoningPreview = ref<LlmReasoningPreview | null>(null)
const reasoningPreviewError = ref('')
let reasoningPreviewRequest = 0

watch(
  () => {
    const s = settingsStore.settings
    const endpoint = activeCustomEndpoint.value
    return [
      s.llmProviderType,
      s.llmVolcengineBaseUrl, s.llmVolcengineModel, s.llmVolcengineReasoningEffort,
      s.llmOpenaiBaseUrl, s.llmOpenaiModel, s.llmOpenaiReasoningEffort,
      s.llmQwenBaseUrl, s.llmQwenModel,
      s.llmDeepseekBaseUrl, s.llmDeepseekModel,
      s.llmGeminiBaseUrl, s.llmGeminiModel,
      s.llmAnthropicBaseUrl, s.llmAnthropicModel,
      endpoint?.id, endpoint?.baseUrl, endpoint?.model, endpoint?.apiMode, endpoint?.reasoningEffort
    ]
  },
  async () => {
    const request = ++reasoningPreviewRequest
    try {
      const preview = await invoke<LlmReasoningPreview>('preview_llm_reasoning', {
        settings: settingsStore.settings
      })
      if (request !== reasoningPreviewRequest) return
      reasoningPreview.value = preview
      reasoningPreviewError.value = ''
    } catch (error) {
      if (request !== reasoningPreviewRequest) return
      reasoningPreview.value = null
      reasoningPreviewError.value = error instanceof Error ? error.message : String(error)
    }
  },
  { immediate: true }
)

const reasoningPreviewText = computed(() => {
  const preview = reasoningPreview.value
  if (!preview) return reasoningPreviewError.value
  if (preview.source === 'unknown') {
    // Gemini and Anthropic offer neither a level nor extra fields to fill in.
    if (isGemini.value || isAnthropic.value) return t('llm.reasoningPreviewUnknownModel')
    return t(llmCustomExtraBody.value.trim() ? 'llm.reasoningPreviewUnknownExtra' : 'llm.reasoningPreviewUnknown')
  }
  if (preview.fields) return t('llm.reasoningPreviewSends')
  return t(preview.source === 'not_needed' ? 'llm.reasoningPreviewNotNeeded' : 'llm.reasoningPreviewNothing')
})

// Worth a warning color: the lowest setting is not guaranteed.
const reasoningPreviewWarns = computed(() => {
  if (!reasoningPreview.value) return reasoningPreviewError.value !== ''
  return reasoningPreview.value.source === 'unknown' && !llmCustomExtraBody.value.trim()
})

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

async function runLlmProviderProbe() {
  llmProbeLoading.value = true
  llmProbeError.value = ''
  try {
    await settingsStore.forceSaveSettings()
    llmProbeResult.value = await invoke<LlmProviderProbeResult>('probe_current_llm_provider')
  } catch (error) {
    llmProbeResult.value = null
    llmProbeError.value = error instanceof Error ? error.message : String(error)
  } finally {
    llmProbeLoading.value = false
  }
}

// Verdict for the label beside the test button: green when the model made the
// expected correction, amber when it answered without it, red when it failed.
const probeTone = computed(() => {
  const result = llmProbeResult.value
  if (!result || !result.ok) return 'error'
  return result.expectedMatch ? 'ok' : 'warn'
})

const probeLabel = computed(() => {
  const result = llmProbeResult.value
  if (!result || !result.ok) return t('llm.providerProbeStatusFailed')
  const verdict = result.expectedMatch ? t('llm.providerProbeStatusOk') : t('llm.providerProbeCorrectionUnchecked')
  return result.responseTimeMs !== null ? `${verdict} · ${result.responseTimeMs} ms` : verdict
})

const probeErrorMessage = computed(() => llmProbeError.value || llmProbeResult.value?.errorMessage || '')

const probeDiff = computed(() => {
  const result = llmProbeResult.value
  return result?.responseText ? diffChars(result.requestText, result.responseText) : null
})
</script>

<template>
  <div class="page settings-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('llm.title') }}</h1>
    </div>

    <SettingsSection :title="t('llm.aiCorrection')">
      <SettingRow :label="t('llm.enableAiCorrection')">
        <NSwitch v-model:value="enableLlmCorrection" />
      </SettingRow>
      <SettingRow :label="t('llm.includeRecentInputHistory')" :help="t('llm.includeRecentInputHistorySub')">
        <NSwitch v-model:value="enableLlmHistoryContext" />
      </SettingRow>
      <SettingRow :label="t('llm.enableTranslationMode')" :help="t('llm.enableTranslationModeSub')">
        <NSwitch v-model:value="translationEnabled" />
      </SettingRow>
    </SettingsSection>

    <SettingsSection :title="t('llm.provider')" :help="t('llm.aiCorrectionSub')">
      <template #extra>
        <ProbeStatus v-if="(llmProbeResult || llmProbeError) && !llmProbeLoading" :tone="probeTone" :label="probeLabel">
          <template v-if="llmProbeResult?.requestText" #default>
            <div class="probe-detail-label">{{ t('llm.providerProbeInput') }}</div>
            <div class="probe-detail-text">
              <template v-if="probeDiff">
                <component :is="part.changed ? 'del' : 'span'" v-for="(part, i) in probeDiff.before" :key="i">{{ part.text }}</component>
              </template>
              <template v-else>{{ llmProbeResult.requestText }}</template>
            </div>
            <div class="probe-detail-label">{{ t('llm.providerProbeOutput') }}</div>
            <div class="probe-detail-text">
              <template v-if="probeDiff">
                <component :is="part.changed ? 'ins' : 'span'" v-for="(part, i) in probeDiff.after" :key="i">{{ part.text }}</component>
              </template>
              <template v-else>{{ t('llm.providerProbeOutputEmpty') }}</template>
            </div>
          </template>
        </ProbeStatus>
        <NTooltip :delay="400" style="max-width: 280px">
          <template #trigger>
            <NButton :loading="llmProbeLoading" size="small" secondary type="primary" @click="runLlmProviderProbe">
              {{ t('llm.providerProbeAction') }}
            </NButton>
          </template>
          {{ t('llm.providerProbeNote') }}
        </NTooltip>
      </template>

      <div v-if="probeErrorMessage && !llmProbeLoading" class="settings-callout is-error">
        {{ probeErrorMessage }}
      </div>

      <SettingRow :label="t('llm.provider')">
        <div class="control-group">
          <NSelect v-model:value="selectedProviderKey" :options="providerOptions" />
          <NTooltip v-if="isCustom && activeCustomEndpoint" :delay="400">
            <template #trigger>
              <NButton quaternary size="small" class="icon-button delete-endpoint" :aria-label="t('llm.deleteCustomEndpoint')" @click="confirmDeleteActiveCustomEndpoint">
                <template #icon>
                  <svg viewBox="0 0 24 24" fill="currentColor" width="16" height="16">
                    <path d="M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM19 4h-3.5l-1-1h-5l-1 1H5v2h14V4z" />
                  </svg>
                </template>
              </NButton>
            </template>
            {{ t('llm.deleteCustomEndpoint') }}
          </NTooltip>
        </div>
      </SettingRow>

      <!-- Volcengine Settings -->
      <template v-if="isVolcengine">
        <SettingRow :label="t('llm.baseUrl')">
          <NInput v-model:value="llmVolcengineBaseUrl" />
        </SettingRow>
        <SettingRow :label="t('llm.apiKey')">
          <NInput
            v-model:value="llmVolcengineApiKey"
            type="password"
            show-password-on="click"
            :placeholder="t('llm.enterApiKey')"
          />
        </SettingRow>
        <SettingRow :label="t('llm.modelName')">
          <NInput v-model:value="llmVolcengineModel" />
        </SettingRow>
        <SettingRow :label="t('llm.reasoningEffort')" :help="t('llm.reasoningEffortSub')">
          <template #status>
            <div class="reasoning-preview" :class="{ warns: reasoningPreviewWarns }">
              {{ reasoningPreviewText }}
              <code v-if="reasoningPreview?.fields">{{ reasoningPreview.fields }}</code>
            </div>
          </template>
          <NSelect
            v-model:value="llmVolcengineReasoningEffort"
            :options="reasoningEffortOptions"
          />
        </SettingRow>
      </template>

      <!-- OpenAI Settings -->
      <template v-if="isOpenai">
        <SettingRow :label="t('llm.baseUrl')">
          <NInput v-model:value="llmOpenaiBaseUrl" />
        </SettingRow>
        <SettingRow :label="t('llm.apiKey')">
          <NInput
            v-model:value="llmOpenaiApiKey"
            type="password"
            show-password-on="click"
            placeholder="sk-..."
          />
        </SettingRow>
        <SettingRow :label="t('llm.modelName')" :help="t('llm.openaiModelsNote')">
          <template #status>
            <div v-if="openaiModelsError">{{ openaiModelsError }}</div>
          </template>
          <div class="control-group">
            <NAutoComplete v-model:value="llmOpenaiModel" :options="openaiModels.map(value => ({ value, label: value }))" />
            <NButton size="small" :loading="openaiModelsLoading" @click="loadOpenaiModels">{{ t('llm.openaiModelsFetch') }}</NButton>
          </div>
        </SettingRow>
        <SettingRow :label="t('llm.reasoningEffort')" :help="t('llm.reasoningEffortOptionalSub')">
          <template #status>
            <div class="reasoning-preview" :class="{ warns: reasoningPreviewWarns }">
              {{ reasoningPreviewText }}
              <code v-if="reasoningPreview?.fields">{{ reasoningPreview.fields }}</code>
            </div>
          </template>
          <NSelect
            v-model:value="llmOpenaiReasoningEffort"
            :options="optionalReasoningEffortOptions"
          />
        </SettingRow>
      </template>

      <!-- Qwen Settings -->
      <template v-if="isQwen">
        <SettingRow :label="t('llm.baseUrl')">
          <NInput v-model:value="llmQwenBaseUrl" />
        </SettingRow>
        <SettingRow :label="t('llm.apiKey')">
          <NInput
            v-model:value="llmQwenApiKey"
            type="password"
            show-password-on="click"
            placeholder="sk-..."
          />
        </SettingRow>
        <SettingRow :label="t('llm.modelName')">
          <NInput v-model:value="llmQwenModel" />
        </SettingRow>
        <SettingRow :label="t('llm.reasoningEffort')" :help="t('llm.reasoningEffortFixedSub')">
          <template #status>
            <div class="reasoning-preview" :class="{ warns: reasoningPreviewWarns }">
              {{ reasoningPreviewText }}
              <code v-if="reasoningPreview?.fields">{{ reasoningPreview.fields }}</code>
            </div>
          </template>
          <div class="reasoning-fixed">{{ t('llm.reasoningLowest') }}</div>
        </SettingRow>
      </template>

      <!-- DeepSeek Settings -->
      <template v-if="isDeepseek">
        <SettingRow :label="t('llm.baseUrl')">
          <NInput v-model:value="llmDeepseekBaseUrl" />
        </SettingRow>
        <SettingRow :label="t('llm.apiKey')">
          <NInput
            v-model:value="llmDeepseekApiKey"
            type="password"
            show-password-on="click"
            placeholder="sk-..."
          />
        </SettingRow>
        <SettingRow :label="t('llm.modelName')">
          <NInput v-model:value="llmDeepseekModel" />
        </SettingRow>
        <SettingRow :label="t('llm.reasoningEffort')" :help="t('llm.reasoningEffortFixedSub')">
          <template #status>
            <div class="reasoning-preview" :class="{ warns: reasoningPreviewWarns }">
              {{ reasoningPreviewText }}
              <code v-if="reasoningPreview?.fields">{{ reasoningPreview.fields }}</code>
            </div>
          </template>
          <div class="reasoning-fixed">{{ t('llm.reasoningLowest') }}</div>
        </SettingRow>
      </template>

      <!-- Gemini Settings -->
      <template v-if="isGemini">
        <SettingRow :label="t('llm.baseUrl')">
          <NInput v-model:value="llmGeminiBaseUrl" />
        </SettingRow>
        <SettingRow :label="t('llm.apiKey')">
          <NInput
            v-model:value="llmGeminiApiKey"
            type="password"
            show-password-on="click"
            :placeholder="t('llm.enterApiKey')"
          />
        </SettingRow>
        <SettingRow :label="t('llm.modelName')">
          <NInput v-model:value="llmGeminiModel" />
        </SettingRow>
        <SettingRow :label="t('llm.reasoningEffort')" :help="t('llm.reasoningEffortFixedSub')">
          <template #status>
            <div class="reasoning-preview" :class="{ warns: reasoningPreviewWarns }">
              {{ reasoningPreviewText }}
              <code v-if="reasoningPreview?.fields">{{ reasoningPreview.fields }}</code>
            </div>
          </template>
          <div class="reasoning-fixed">{{ t('llm.reasoningLowest') }}</div>
        </SettingRow>
      </template>

      <!-- Anthropic Settings -->
      <template v-if="isAnthropic">
        <SettingRow :label="t('llm.baseUrl')">
          <NInput v-model:value="llmAnthropicBaseUrl" />
        </SettingRow>
        <SettingRow :label="t('llm.apiKey')">
          <NInput
            v-model:value="llmAnthropicApiKey"
            type="password"
            show-password-on="click"
            placeholder="sk-ant-..."
          />
        </SettingRow>
        <SettingRow :label="t('llm.modelName')">
          <NInput v-model:value="llmAnthropicModel" />
        </SettingRow>
        <SettingRow :label="t('llm.reasoningEffort')" :help="t('llm.reasoningEffortFixedSub')">
          <template #status>
            <div class="reasoning-preview" :class="{ warns: reasoningPreviewWarns }">
              {{ reasoningPreviewText }}
              <code v-if="reasoningPreview?.fields">{{ reasoningPreview.fields }}</code>
            </div>
          </template>
          <div class="reasoning-fixed">{{ t('llm.reasoningLowest') }}</div>
        </SettingRow>
      </template>

      <!-- Custom Settings -->
      <template v-if="isCustom">
        <SettingRow v-if="!activeCustomEndpoint" :label="t('llm.customNoEndpointHint')">
          <NButton type="primary" secondary size="small" @click="addCustomEndpoint">
            {{ t('llm.addCustomEndpoint') }}
          </NButton>
        </SettingRow>
        <template v-else>
        <SettingRow :label="t('llm.customEndpointName')" :help="t('llm.customEndpointNameSub')">
          <NInput v-model:value="llmCustomName" :placeholder="t('llm.customEndpointNamePlaceholder')" />
        </SettingRow>
        <SettingRow :label="t('llm.baseUrl')" :help="t('llm.customBaseUrlSub')">
          <NInput v-model:value="llmCustomBaseUrl" placeholder="https://your-api.example.com/v1" />
        </SettingRow>
        <SettingRow :label="t('llm.apiKey')">
          <NInput
            v-model:value="llmCustomApiKey"
            type="password"
            show-password-on="click"
            :placeholder="t('llm.enterApiKey')"
          />
        </SettingRow>
        <SettingRow :label="t('llm.modelName')">
          <NInput v-model:value="llmCustomModel" placeholder="your-model-id" />
        </SettingRow>
        <SettingRow :label="t('llm.apiMode')" :help="t('llm.customApiModeSub')">
          <NSelect v-model:value="llmCustomApiMode" :options="apiModeOptions" />
        </SettingRow>
        <SettingRow :label="t('llm.reasoningEffort')" :help="t('llm.reasoningEffortOptionalSub')">
          <template #status>
            <div class="reasoning-preview" :class="{ warns: reasoningPreviewWarns }">
              {{ reasoningPreviewText }}
              <code v-if="reasoningPreview?.fields">{{ reasoningPreview.fields }}</code>
            </div>
          </template>
          <NSelect
            v-model:value="llmCustomReasoningEffort"
            :options="optionalReasoningEffortOptions"
          />
        </SettingRow>
        <SettingRow stacked :label="t('llm.extraBody')" :help="t('llm.extraBodySub')">
          <template #status>
            <div v-if="llmCustomExtraBodyError" class="is-error">{{ llmCustomExtraBodyError }}</div>
          </template>
          <NInput
            v-model:value="llmCustomExtraBody"
            type="textarea"
            :rows="3"
            :status="llmCustomExtraBodyError ? 'error' : undefined"
            :placeholder="t('llm.extraBodyPlaceholder')"
            class="mono"
          />
        </SettingRow>
        </template>
      </template>


    </SettingsSection>

    <SettingsSection :title="t('llm.prompts')" :help="t('llm.promptHint')">
      <template #extra>
        <NButton size="small" quaternary @click="resetPrompt">{{ t('llm.resetCurrentTab') }}</NButton>
      </template>
      <NTabs v-model:value="activePromptTab" type="line" animated class="prompt-tabs">
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
    </SettingsSection>
  </div>
</template>

<style scoped>
.reasoning-preview {
  user-select: text;
}

.reasoning-preview code {
  font-family: var(--font-mono);
  word-break: break-all;
}

.reasoning-preview.warns {
  color: color-mix(in srgb, var(--color-warning) 72%, var(--color-text-secondary));
}

.reasoning-fixed {
  font-size: var(--font-sm);
  color: var(--color-text-secondary);
  text-align: right;
}

.mono :deep(textarea) {
  font-family: var(--font-mono);
  font-size: var(--font-xs);
}

.prompt-tabs {
  margin-bottom: var(--spacing-lg);
}
</style>
