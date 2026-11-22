<script setup lang="ts">
import { computed, ref, watch, type ComputedRef } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NInput, NSwitch, NButton, NSelect, NTabs, NTabPane, NTooltip, useDialog } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import type { ResolvedLocale } from '../i18n'
import { useSettingsStore, type AppSettings, type CustomLlmEndpoint } from '../stores/settings'
import { buildModelCacheKey, shouldAutoFetchModels, useLlmModelCacheStore } from '../stores/llmModels'
import llmModels from '../shared/llmModels.json'
import { getDefaultPrompt } from '../utils/llmPrompts'
import {
  buildLlmApiModeOptions,
  buildLlmProviderOptions,
  buildReasoningEffortOptions,
  isOfficialOpenAiBaseUrl,
  providerSelectionKey,
  ADD_CUSTOM_ENDPOINT_VALUE,
  CUSTOM_ENDPOINT_PREFIX,
  type LlmProviderValue
} from '../utils/llmOptions'
import SettingsSection from '../components/settings/SettingsSection.vue'
import SettingRow from '../components/settings/SettingRow.vue'
import ProbeStatus from '../components/settings/ProbeStatus.vue'
import { diffText } from '../utils/textDiff'
import DiffText from '../components/DiffText.vue'

const settingsStore = useSettingsStore()
const modelCacheStore = useLlmModelCacheStore()
const { t, locale } = useI18n()

interface LlmProviderProbeResult {
  provider: string
  ok: boolean
  responseTimeMs: number | null
  requestText: string
  responseText: string
  expectedMatch: boolean
  errorMessage: string | null
  inputTokens: number | null
  outputTokens: number | null
  reasoningTokens: number | null
  generatedTokens: number | null
  tokensPerSecond: number | null
  reasoningApplied: string
}

interface LlmReasoningPreview {
  // Compact JSON of the fields, '' when nothing is sent.
  fields: string
  source: 'lowest' | 'not_needed' | 'unknown' | 'chosen'
  supportedEfforts: string[]
}

const providerOptions = computed(() =>
  buildLlmProviderOptions(t, settingsStore.settings.llmCustomEndpoints, { includeAddAction: true })
)
const apiModeOptions = computed(() => buildLlmApiModeOptions(t))

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
  get: () => settingsStore.settings.llmOpenaiModel || null,
  set: (v: string | null) => settingsStore.updateSetting('llmOpenaiModel', v?.trim() ?? '')
})
const llmOpenaiServiceTier = computed({
  get: () => settingsStore.settings.llmOpenaiServiceTier,
  set: (v: AppSettings['llmOpenaiServiceTier']) => settingsStore.updateSetting('llmOpenaiServiceTier', v)
})
const openaiServiceTierOptions = computed(() => [
  { label: t('llm.openaiServiceTierStandard'), value: 'default' },
  { label: t('llm.openaiServiceTierFast'), value: 'fast' }
])
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
const llmQwenReasoningEffort = computed({
  get: () => settingsStore.settings.llmQwenReasoningEffort ?? '',
  set: (v: string) => settingsStore.updateSetting('llmQwenReasoningEffort', v || null)
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
const llmDeepseekReasoningEffort = computed({
  get: () => settingsStore.settings.llmDeepseekReasoningEffort ?? '',
  set: (v: string) => settingsStore.updateSetting('llmDeepseekReasoningEffort', v || null)
})

// Gemini-specific (Google's official API)
const llmGeminiApiKey = computed({
  get: () => settingsStore.settings.llmGeminiApiKey,
  set: (v: string) => settingsStore.updateSetting('llmGeminiApiKey', v)
})
const llmGeminiModel = computed({
  get: () => settingsStore.settings.llmGeminiModel || null,
  set: (v: string | null) => settingsStore.updateSetting('llmGeminiModel', v?.trim() ?? '')
})
const llmGeminiReasoningEffort = computed({
  get: () => settingsStore.settings.llmGeminiReasoningEffort ?? '',
  set: (v: string) => settingsStore.updateSetting('llmGeminiReasoningEffort', v || null)
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
const llmAnthropicReasoningEffort = computed({
  get: () => settingsStore.settings.llmAnthropicReasoningEffort ?? '',
  set: (v: string) => settingsStore.updateSetting('llmAnthropicReasoningEffort', v || null)
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
  get: () => activeCustomEndpoint.value?.model || null,
  set: (v: string | null) => {
    if (activeCustomEndpoint.value) activeCustomEndpoint.value.model = v?.trim() ?? ''
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
interface RemoteModelSnapshot {
  canLoad: boolean
  cacheKey: string
  fetchModels: () => Promise<string[]>
}

function useRemoteModelList(options: {
  active: ComputedRef<boolean>
  autoFetch: ComputedRef<boolean>
  snapshot: ComputedRef<RemoteModelSnapshot>
}) {
  const models = computed(() => modelCacheStore.getEntry(options.snapshot.value.cacheKey)?.models ?? null)
  const modelOptions = computed(() => (models.value ?? []).map((value) => ({ value, label: value })))
  const loading = computed(() => modelCacheStore.isLoading(options.snapshot.value.cacheKey))
  const error = computed(() => {
    const entry = modelCacheStore.getEntry(options.snapshot.value.cacheKey)
    if (entry?.lastFailedAt == null) return ''
    return entry.lastError || t('llm.modelListLastRefreshFailed')
  })

  // Snapshot credentials synchronously so any async await inside refresh()
  // still pairs the response with the exact cacheKey that initiated it.
  async function load() {
    const req = options.snapshot.value
    if (!req.canLoad) return
    await modelCacheStore.refresh(req.cacheKey, req.fetchModels, { force: true })
  }

  watch(
    [
      () => options.snapshot.value.cacheKey,
      options.active,
      options.autoFetch,
      () => settingsStore.isLoading,
      () => modelCacheStore.isLoaded
    ],
    (
      [key, active, autoFetch, settingsLoading, cacheLoaded],
      [prevKey, wasActive, _prevAuto, wasLoading],
      onCleanup
    ) => {
      if (!cacheLoaded) {
        void modelCacheStore.ensureLoaded()
        return
      }
      const req = options.snapshot.value
      if (settingsLoading || !active || !autoFetch || !req.canLoad) return
      if (!shouldAutoFetchModels(modelCacheStore.getEntry(key))) return

      const prefetch = () => {
        void modelCacheStore.refresh(req.cacheKey, req.fetchModels, { force: false })
      }
      // Load saved credentials immediately. Give edits time to settle before
      // asking the provider to list models for the new credentials.
      if (prevKey !== undefined && key !== prevKey && wasActive && !wasLoading) {
        const timer = window.setTimeout(prefetch, 500)
        onCleanup(() => window.clearTimeout(timer))
      } else {
        prefetch()
      }
    },
    { immediate: true }
  )

  return { models, modelOptions, loading, error, load }
}

const isOfficialOpenai = computed(() => isOfficialOpenAiBaseUrl(llmOpenaiBaseUrl.value))
const openaiSnapshot = computed<RemoteModelSnapshot>(() => {
  const baseUrl = llmOpenaiBaseUrl.value
  const apiKey = llmOpenaiApiKey.value
  return {
    canLoad: Boolean(baseUrl.trim() && apiKey.trim()),
    cacheKey: buildModelCacheKey('openai', baseUrl, apiKey),
    fetchModels: () => invoke<string[]>('list_openai_models', { baseUrl, apiKey })
  }
})
const canLoadOpenaiModels = computed(() => openaiSnapshot.value.canLoad)
const openaiModelList = useRemoteModelList({
  active: isOpenai,
  autoFetch: isOfficialOpenai,
  snapshot: openaiSnapshot
})

const geminiSnapshot = computed<RemoteModelSnapshot>(() => {
  const apiKey = llmGeminiApiKey.value
  return {
    canLoad: Boolean(apiKey.trim()),
    cacheKey: buildModelCacheKey('gemini', llmModels.geminiBaseUrl, apiKey),
    fetchModels: () => invoke<string[]>('list_gemini_models', { baseUrl: null, apiKey })
  }
})
const canLoadGeminiModels = computed(() => geminiSnapshot.value.canLoad)
const geminiModelList = useRemoteModelList({
  active: isGemini,
  autoFetch: computed(() => true),
  snapshot: geminiSnapshot
})

const customSnapshot = computed<RemoteModelSnapshot>(() => {
  const apiMode = llmCustomApiMode.value
  const baseUrl = llmCustomBaseUrl.value
  const apiKey = llmCustomApiKey.value
  const scope = apiMode === 'gemini' ? 'custom-gemini' : 'custom-openai'
  return {
    canLoad: Boolean(baseUrl.trim() && apiKey.trim()),
    cacheKey: buildModelCacheKey(scope, baseUrl, apiKey),
    fetchModels: () =>
      apiMode === 'gemini'
        ? invoke<string[]>('list_gemini_models', { baseUrl, apiKey })
        : invoke<string[]>('list_openai_models', { baseUrl, apiKey })
  }
})
const canLoadCustomModels = computed(() => customSnapshot.value.canLoad)
const customModelList = useRemoteModelList({
  active: isCustom,
  autoFetch: computed(() => false),
  snapshot: customSnapshot
})

// The reasoning fields the selected provider will be sent, resolved by the
// backend from the page's current values, saved or not.
const reasoningPreview = ref<LlmReasoningPreview | null>(null)
const reasoningPreviewError = ref('')
let reasoningPreviewRequest = 0

const activeReasoningEffort = computed({
  get: (): string => {
    switch (llmProviderType.value) {
      case 'volcengine': return llmVolcengineReasoningEffort.value
      case 'openai': return llmOpenaiReasoningEffort.value
      case 'qwen': return llmQwenReasoningEffort.value
      case 'deepseek': return llmDeepseekReasoningEffort.value
      case 'gemini': return llmGeminiReasoningEffort.value
      case 'anthropic': return llmAnthropicReasoningEffort.value
      case 'custom': return llmCustomReasoningEffort.value
      default: return ''
    }
  },
  set: (v: string) => {
    switch (llmProviderType.value) {
      case 'volcengine': llmVolcengineReasoningEffort.value = v; break
      case 'openai': llmOpenaiReasoningEffort.value = v; break
      case 'qwen': llmQwenReasoningEffort.value = v; break
      case 'deepseek': llmDeepseekReasoningEffort.value = v; break
      case 'gemini': llmGeminiReasoningEffort.value = v; break
      case 'anthropic': llmAnthropicReasoningEffort.value = v; break
      case 'custom': llmCustomReasoningEffort.value = v; break
    }
  }
})

const activeReasoningEffortOptions = computed(() =>
  buildReasoningEffortOptions(t, reasoningPreview.value?.supportedEfforts ?? [])
)

watch(
  () => {
    const s = settingsStore.settings
    const endpoint = activeCustomEndpoint.value
    return [
      s.llmProviderType,
      s.llmVolcengineBaseUrl, s.llmVolcengineModel, s.llmVolcengineReasoningEffort,
      s.llmOpenaiBaseUrl, s.llmOpenaiModel, s.llmOpenaiReasoningEffort,
      s.llmQwenBaseUrl, s.llmQwenModel, s.llmQwenReasoningEffort,
      s.llmDeepseekBaseUrl, s.llmDeepseekModel, s.llmDeepseekReasoningEffort,
      s.llmGeminiModel, s.llmGeminiReasoningEffort,
      s.llmAnthropicBaseUrl, s.llmAnthropicModel, s.llmAnthropicReasoningEffort,
      endpoint?.id, endpoint?.baseUrl, endpoint?.model, endpoint?.apiMode, endpoint?.reasoningEffort,
      endpoint?.extraBody
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

      const current = activeReasoningEffort.value
      if (
        !settingsStore.isLoading &&
        current &&
        current !== 'server_default' &&
        !preview.supportedEfforts.includes(current)
      ) {
        activeReasoningEffort.value = ''
      }
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
    if (isGemini.value || isAnthropic.value || (isCustom.value && llmCustomApiMode.value === 'gemini')) {
      return t('llm.reasoningPreviewUnknownModel')
    }
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
  return result?.responseText ? diffText(result.requestText, result.responseText) : null
})

const probeUsageSummary = computed(() => {
  const result = llmProbeResult.value
  if (!result) return ''
  const parts: string[] = []
  if (result.reasoningTokens !== null) {
    parts.push(`${t('llm.providerProbeReasoningTokens')} ${result.reasoningTokens}`)
  }
  if (result.outputTokens !== null) {
    parts.push(`${t('llm.providerProbeOutputTokens')} ${result.outputTokens}`)
  }
  if (result.generatedTokens !== null) {
    parts.push(`${t('llm.providerProbeTotalGenerated')} ${result.generatedTokens}`)
  }
  if (result.tokensPerSecond !== null) {
    parts.push(`${result.tokensPerSecond.toFixed(1)} tok/s`)
  }
  return parts.join(' · ')
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
              <DiffText v-if="probeDiff" :diff="probeDiff" view="before" />
              <template v-else>{{ llmProbeResult.requestText }}</template>
            </div>
            <div class="probe-detail-label">{{ t('llm.providerProbeOutput') }}</div>
            <div class="probe-detail-text">
              <DiffText v-if="probeDiff" :diff="probeDiff" view="after" />
              <template v-else>{{ t('llm.providerProbeOutputEmpty') }}</template>
            </div>
            <div class="probe-detail-label">{{ t('llm.providerProbeReasoning') }}</div>
            <div class="probe-detail-text mono-inline">
              {{ llmProbeResult.reasoningApplied || t('llm.providerProbeNoReasoningField') }}
            </div>
            <template v-if="probeUsageSummary">
              <div class="probe-detail-label">{{ t('llm.providerProbeUsage') }}</div>
              <div class="probe-detail-text">{{ probeUsageSummary }}</div>
            </template>
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
        <SettingRow :label="t('llm.modelName')" :help="t(isOfficialOpenai ? 'llm.openaiModelsNote' : 'llm.openaiGatewayModelsNote')">
          <template #status>
            <div v-if="openaiModelList.error.value" class="is-error">{{ openaiModelList.error.value }}</div>
          </template>
          <div class="control-group">
            <NSelect
              v-model:value="llmOpenaiModel"
              :options="openaiModelList.modelOptions.value"
              :loading="openaiModelList.loading.value"
              :placeholder="t('llm.openaiModelPlaceholder')"
              filterable
              tag
            >
              <template v-if="openaiModelList.loading.value && openaiModelList.models.value === null" #empty>
                {{ t('llm.openaiModelsLoading') }}
              </template>
            </NSelect>
            <NButton
              size="small"
              :disabled="!canLoadOpenaiModels"
              :loading="openaiModelList.loading.value"
              @click="openaiModelList.load"
            >{{ t('llm.openaiModelsFetch') }}</NButton>
          </div>
        </SettingRow>
        <SettingRow v-if="isOfficialOpenai" :label="t('llm.openaiServiceTier')" :help="t('llm.openaiServiceTierHelp')">
          <NSelect v-model:value="llmOpenaiServiceTier" :options="openaiServiceTierOptions" />
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
      </template>

      <!-- Gemini Settings (official endpoint only) -->
      <template v-if="isGemini">
        <SettingRow :label="t('llm.apiKey')">
          <NInput
            v-model:value="llmGeminiApiKey"
            type="password"
            show-password-on="click"
            :placeholder="t('llm.enterApiKey')"
          />
        </SettingRow>
        <SettingRow :label="t('llm.modelName')" :help="t('llm.geminiModelsNote')">
          <template #status>
            <div v-if="geminiModelList.error.value" class="is-error">{{ geminiModelList.error.value }}</div>
          </template>
          <div class="control-group">
            <NSelect
              v-model:value="llmGeminiModel"
              :options="geminiModelList.modelOptions.value"
              :loading="geminiModelList.loading.value"
              :placeholder="t('llm.openaiModelPlaceholder')"
              filterable
              tag
            >
              <template v-if="geminiModelList.loading.value && geminiModelList.models.value === null" #empty>
                {{ t('llm.openaiModelsLoading') }}
              </template>
            </NSelect>
            <NButton
              size="small"
              :disabled="!canLoadGeminiModels"
              :loading="geminiModelList.loading.value"
              @click="geminiModelList.load"
            >{{ t('llm.openaiModelsFetch') }}</NButton>
          </div>
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
        <SettingRow :label="t('llm.apiMode')" :help="t('llm.customApiModeSub')">
          <NSelect v-model:value="llmCustomApiMode" :options="apiModeOptions" />
        </SettingRow>
        <SettingRow :label="t('llm.baseUrl')" :help="t(llmCustomApiMode === 'gemini' ? 'llm.customGeminiBaseUrlSub' : 'llm.customBaseUrlSub')">
          <NInput
            v-model:value="llmCustomBaseUrl"
            :placeholder="llmCustomApiMode === 'gemini' ? 'https://your-gemini-proxy.example.com' : 'https://your-api.example.com/v1'"
          />
        </SettingRow>
        <SettingRow :label="t('llm.apiKey')">
          <NInput
            v-model:value="llmCustomApiKey"
            type="password"
            show-password-on="click"
            :placeholder="t('llm.enterApiKey')"
          />
        </SettingRow>
        <SettingRow :label="t('llm.modelName')" :help="t('llm.customModelsNote')">
          <template #status>
            <div v-if="customModelList.error.value" class="is-error">{{ customModelList.error.value }}</div>
          </template>
          <div class="control-group">
            <NSelect
              v-model:value="llmCustomModel"
              :options="customModelList.modelOptions.value"
              :loading="customModelList.loading.value"
              :placeholder="t('llm.openaiModelPlaceholder')"
              filterable
              tag
            >
              <template v-if="customModelList.loading.value && customModelList.models.value === null" #empty>
                {{ t('llm.openaiModelsLoading') }}
              </template>
            </NSelect>
            <NButton
              size="small"
              :disabled="!canLoadCustomModels"
              :loading="customModelList.loading.value"
              @click="customModelList.load"
            >{{ t('llm.openaiModelsFetch') }}</NButton>
          </div>
        </SettingRow>
        </template>
      </template>

      <SettingRow
        v-if="!isCustom || activeCustomEndpoint"
        :label="t('llm.reasoningEffort')"
        :help="t('llm.reasoningEffortOptionalSub')"
      >
        <template #status>
          <div class="reasoning-preview" :class="{ warns: reasoningPreviewWarns }">
            {{ reasoningPreviewText }}
            <code v-if="reasoningPreview?.fields">{{ reasoningPreview.fields }}</code>
          </div>
        </template>
        <NSelect
          v-model:value="activeReasoningEffort"
          :options="activeReasoningEffortOptions"
        />
      </SettingRow>

      <SettingRow
        v-if="isCustom && activeCustomEndpoint"
        stacked
        :label="t('llm.extraBody')"
        :help="t('llm.extraBodySub')"
      >
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

.mono :deep(textarea) {
  font-family: var(--font-mono);
  font-size: var(--font-xs);
}

.mono-inline {
  font-family: var(--font-mono);
  font-size: var(--font-xs);
}

.prompt-tabs {
  margin-bottom: var(--spacing-lg);
}
</style>
