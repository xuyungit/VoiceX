<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NCheckbox, NInput, NInputNumber, NSelect } from 'naive-ui'
import { useSettingsStore } from '../stores/settings'

const settingsStore = useSettingsStore()

type LocalAsrStatus = {
  available: boolean
  configuredCommand: string
  resolvedPath: string | null
  ffmpegAvailable: boolean
  modelsDir: string | null
  sensevoiceInstalled: boolean
  whisperInstalled: boolean
  vadInstalled: boolean
  message: string
}

type ProviderValue = 'volcengine' | 'google' | 'qwen' | 'coli'

const asrProviderType = computed({
  get: () => settingsStore.settings.asrProviderType,
  set: (v: ProviderValue) => {
    if (v === 'coli' && coliStatus.value && !coliStatus.value.available) {
      return
    }
    settingsStore.updateSetting('asrProviderType', v)
  }
})

const isVolcengine = computed(() => settingsStore.settings.asrProviderType === 'volcengine')
const isGoogle = computed(() => settingsStore.settings.asrProviderType === 'google')
const isQwen = computed(() => settingsStore.settings.asrProviderType === 'qwen')
const isColi = computed(() => settingsStore.settings.asrProviderType === 'coli')

// Volcengine settings
const asrAppKey = computed({
  get: () => settingsStore.settings.asrAppKey,
  set: (v: string) => settingsStore.updateSetting('asrAppKey', v)
})

const asrAccessKey = computed({
  get: () => settingsStore.settings.asrAccessKey,
  set: (v: string) => settingsStore.updateSetting('asrAccessKey', v)
})

const asrResourceId = computed({
  get: () => settingsStore.settings.asrResourceId,
  set: (v: string) => settingsStore.updateSetting('asrResourceId', v)
})

const asrWsUrl = computed({
  get: () => settingsStore.settings.asrWsUrl,
  set: (v: string) => settingsStore.updateSetting('asrWsUrl', v)
})

const recognitionModeOptions = [
  { label: 'Realtime (bigmodel_async)', value: 'realtime_async' },
  { label: 'Nostream (bigmodel_nostream)', value: 'nostream' }
]

const recognitionMode = computed({
  get: () => (settingsStore.settings.asrWsUrl.includes('nostream') ? 'nostream' : 'realtime_async'),
  set: (v: string) => {
    const url =
      v === 'nostream'
        ? 'wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_nostream'
        : 'wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async'
    settingsStore.updateSetting('asrWsUrl', url)
  }
})

// Google settings
const googleSttApiKey = computed({
  get: () => settingsStore.settings.googleSttApiKey,
  set: (v: string) => settingsStore.updateSetting('googleSttApiKey', v)
})

const googleSttProjectId = computed({
  get: () => settingsStore.settings.googleSttProjectId,
  set: (v: string) => settingsStore.updateSetting('googleSttProjectId', v)
})

const googleSttLocation = computed({
  get: () => settingsStore.settings.googleSttLocation,
  set: (v: string) => settingsStore.updateSetting('googleSttLocation', v)
})

const googleSttLanguageCode = computed({
  get: () => settingsStore.settings.googleSttLanguageCode,
  set: (v: string) => settingsStore.updateSetting('googleSttLanguageCode', v)
})

const googleSttEndpointing = computed({
  get: () => settingsStore.settings.googleSttEndpointing,
  set: (v: 'supershort' | 'short' | 'standard') => settingsStore.updateSetting('googleSttEndpointing', v)
})

const googleSttPhraseBoost = computed({
  get: () => settingsStore.settings.googleSttPhraseBoost,
  set: (v: number | null) => settingsStore.updateSetting('googleSttPhraseBoost', v ?? 0)
})

const googleEndpointingOptions = [
  { label: 'Supershort — 出字最快，断句更激进', value: 'supershort' },
  { label: 'Short — 较快出字，断句适中', value: 'short' },
  { label: 'Standard — 标准断句', value: 'standard' },
]

const googleLocationOptions = [
  { label: 'us (Multi-region)', value: 'us' },
  { label: 'eu (Multi-region)', value: 'eu' },
  { label: 'asia-southeast1 (Singapore)', value: 'asia-southeast1' },
  { label: 'asia-northeast1 (Tokyo)', value: 'asia-northeast1' },
  { label: 'asia-south1 (Mumbai) [Preview]', value: 'asia-south1' },
  { label: 'europe-west2 (London) [Preview]', value: 'europe-west2' },
  { label: 'europe-west3 (Frankfurt) [Preview]', value: 'europe-west3' },
  { label: 'northamerica-northeast1 (Montreal) [Preview]', value: 'northamerica-northeast1' },
]

// Qwen settings
const qwenAsrApiKey = computed({
  get: () => settingsStore.settings.qwenAsrApiKey,
  set: (v: string) => settingsStore.updateSetting('qwenAsrApiKey', v)
})

const qwenAsrModel = computed({
  get: () => settingsStore.settings.qwenAsrModel,
  set: (v: string) => settingsStore.updateSetting('qwenAsrModel', v)
})

const qwenAsrWsUrl = computed({
  get: () => settingsStore.settings.qwenAsrWsUrl,
  set: (v: string) => settingsStore.updateSetting('qwenAsrWsUrl', v)
})

const qwenAsrLanguage = computed({
  get: () => settingsStore.settings.qwenAsrLanguage,
  set: (v: string) => settingsStore.updateSetting('qwenAsrLanguage', v)
})

// Local coli settings
const coliCommandPath = computed({
  get: () => settingsStore.settings.coliCommandPath,
  set: (v: string) => settingsStore.updateSetting('coliCommandPath', v)
})

const coliUseVad = computed({
  get: () => settingsStore.settings.coliUseVad,
  set: (v: boolean) => settingsStore.updateSetting('coliUseVad', v)
})

const coliAsrIntervalMs = computed({
  get: () => settingsStore.settings.coliAsrIntervalMs,
  set: (v: number | null) => settingsStore.updateSetting('coliAsrIntervalMs', v ?? 1000)
})

const coliFinalRefinementMode = computed({
  get: () => settingsStore.settings.coliFinalRefinementMode,
  set: (v: 'off' | 'sensevoice' | 'whisper') => settingsStore.updateSetting('coliFinalRefinementMode', v)
})

const coliRealtime = computed({
  get: () => settingsStore.settings.coliRealtime,
  set: (v: boolean) => settingsStore.updateSetting('coliRealtime', v)
})

const coliStatus = ref<LocalAsrStatus | null>(null)
const coliStatusLoading = ref(false)
const coliStatusError = ref('')
let coliStatusRefreshTimer: number | null = null

const providerOptions = computed(() => {
  const coliDetected = coliStatus.value?.available ?? false
  const coliLabel = coliDetected
    ? 'Local Offline ASR (coli)'
    : coliStatusLoading.value
      ? 'Local Offline ASR (coli) - checking...'
      : 'Local Offline ASR (coli) - unavailable'

  return [
    { label: 'Volcengine Doubao (豆包)', value: 'volcengine' as ProviderValue },
    { label: 'Google Cloud Speech-to-Text V2', value: 'google' as ProviderValue },
    { label: 'Qwen Realtime ASR (通义千问)', value: 'qwen' as ProviderValue },
    {
      label: coliLabel,
      value: 'coli' as ProviderValue,
      disabled: !coliDetected && settingsStore.settings.asrProviderType !== 'coli'
    }
  ]
})

const showColiUnavailableWarning = computed(() =>
  settingsStore.settings.asrProviderType === 'coli' &&
  !coliStatusLoading.value &&
  !!coliStatus.value &&
  !coliStatus.value.available
)

const coliRefinementOptions = [
  { label: 'Off', value: 'off' },
  { label: 'SenseVoice refine', value: 'sensevoice' },
  { label: 'Whisper refine (English only)', value: 'whisper' },
]

async function refreshColiStatus() {
  coliStatusLoading.value = true
  coliStatusError.value = ''
  try {
    coliStatus.value = await invoke<LocalAsrStatus>('probe_local_asr', {
      commandPath: coliCommandPath.value.trim() || null
    })
  } catch (error) {
    coliStatusError.value = error instanceof Error ? error.message : String(error)
  } finally {
    coliStatusLoading.value = false
  }
}

const qwenModelOptions = [
  { label: 'Stable - qwen3-asr-flash-realtime', value: 'qwen3-asr-flash-realtime' },
  { label: 'Snapshot - qwen3-asr-flash-realtime-2026-02-10', value: 'qwen3-asr-flash-realtime-2026-02-10' },
  { label: 'Snapshot - qwen3-asr-flash-realtime-2025-10-27', value: 'qwen3-asr-flash-realtime-2025-10-27' },
]

const qwenWsUrlOptions = [
  { label: 'Beijing - 中国内地', value: 'wss://dashscope.aliyuncs.com/api-ws/v1/realtime' },
  { label: 'Singapore - International', value: 'wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime' },
]

// Common settings
const maxRecordingOptions = [
  { label: 'No limit', value: 0 },
  { label: '1 min', value: 1 },
  { label: '5 min', value: 5 },
  { label: '10 min', value: 10 },
  { label: '30 min', value: 30 }
]

const endWindowSize = computed({
  get: () => settingsStore.settings.endWindowSize,
  set: (v: number | null) => settingsStore.updateSetting('endWindowSize', v)
})

const forceToSpeechTime = computed({
  get: () => settingsStore.settings.forceToSpeechTime,
  set: (v: number | null) => settingsStore.updateSetting('forceToSpeechTime', v)
})

const maxRecordingMinutes = computed({
  get: () => settingsStore.settings.maxRecordingMinutes,
  set: (v: number) => settingsStore.updateSetting('maxRecordingMinutes', v)
})

const enableDdc = computed({
  get: () => settingsStore.settings.enableDdc,
  set: (v: boolean) => settingsStore.updateSetting('enableDdc', v)
})

onMounted(() => {
  refreshColiStatus()
})

watch(coliCommandPath, () => {
  if (coliStatusRefreshTimer !== null) {
    clearTimeout(coliStatusRefreshTimer)
  }
  coliStatusRefreshTimer = window.setTimeout(() => {
    refreshColiStatus()
    coliStatusRefreshTimer = null
  }, 350)
})
</script>

<template>
  <div class="page settings-page asr-page">
    <div class="page-header">
      <h1 class="page-title">ASR</h1>
    </div>

    <!-- Provider Selection -->
    <div class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">Provider</div>
        <div class="card-sub">选择语音识别服务供应商</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">ASR Provider</div>
          </div>
          <NSelect
            v-model:value="asrProviderType"
            :options="providerOptions"
            size="small"
            class="field-control"
          />
        </div>
        <div v-if="showColiUnavailableWarning" class="warning-box">
          当前设置仍指向本地 `coli`，但系统没有检测到可用命令。开始录音时本地 ASR 不会工作；请安装/修正 `coli` 路径，或切换回在线 provider。
        </div>
      </div>
    </div>

    <!-- Volcengine Credentials -->
    <div v-if="isVolcengine" class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">API Credentials</div>
        <div class="card-sub">Volcengine 豆包 ASR 服务的访问凭证</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">App Key</div>
          </div>
          <NInput v-model:value="asrAppKey" placeholder="Enter App Key" class="field-control" />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Access Key</div>
          </div>
          <NInput
            v-model:value="asrAccessKey"
            type="password"
            show-password-on="click"
            placeholder="Enter Access Key"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Resource ID</div>
          </div>
          <NInput v-model:value="asrResourceId" placeholder="Enter Resource ID" class="field-control" />
        </div>
      </div>
    </div>

    <!-- Google Configuration -->
    <div v-if="isGoogle" class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">Google Cloud Configuration</div>
        <div class="card-sub">使用 Service Account 密钥认证，在 GCP Console → IAM → Service Accounts 中创建密钥并粘贴 JSON 内容</div>
      </div>
      <div class="field-list">
        <div class="field-row sa-json-row">
          <div class="field-text">
            <div class="field-label">Service Account Key</div>
            <div class="field-note">粘贴 Service Account JSON 密钥的完整内容。</div>
          </div>
          <NInput
            v-model:value="googleSttApiKey"
            type="textarea"
            placeholder='{"type":"service_account","project_id":"...","private_key":"...",...}'
            :autosize="{ minRows: 3, maxRows: 6 }"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Project ID</div>
            <div class="field-note">GCP 项目 ID，可从 Service Account JSON 的 project_id 字段获取。</div>
          </div>
          <NInput v-model:value="googleSttProjectId" placeholder="e.g. my-project-123456" class="field-control" />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Location</div>
            <div class="field-note">Chirp 3 model availability varies by region.</div>
          </div>
          <NSelect
            v-model:value="googleSttLocation"
            :options="googleLocationOptions"
            filterable
            tag
            size="small"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Language</div>
            <div class="field-note">支持逗号分隔多个 BCP-47 语言码，例如 `cmn-Hans-CN, en-US`；填 `auto` 使用自动识别。</div>
          </div>
          <NInput
            v-model:value="googleSttLanguageCode"
            placeholder="cmn-Hans-CN, en-US"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Endpointing</div>
            <div class="field-note">Controls how quickly the model finalizes a sentence.</div>
          </div>
          <NSelect
            v-model:value="googleSttEndpointing"
            :options="googleEndpointingOptions"
            size="small"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Phrase Boost</div>
            <div class="field-note">Google 词典偏置强度。建议先从 `8` 开始，过高可能带来误识别。</div>
          </div>
          <NInputNumber
            v-model:value="googleSttPhraseBoost"
            :min="0"
            :max="20"
            :step="1"
            class="field-control short"
          />
        </div>
      </div>
    </div>

    <!-- Qwen Configuration -->
    <div v-if="isQwen" class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">Qwen Realtime Configuration</div>
        <div class="card-sub">
          使用 DashScope API Key。北京与新加坡地域的 API Key 不通用；当前接入走 realtime 文本流，热词表、ASR 历史上下文、时间戳、说话人分离和 DDC 不生效，应用内词典仍可用于后续 LLM 纠错。
        </div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">API Key</div>
            <div class="field-note">填写对应地域的 DashScope API Key。</div>
          </div>
          <NInput
            v-model:value="qwenAsrApiKey"
            type="password"
            show-password-on="click"
            placeholder="sk-..."
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Endpoint</div>
            <div class="field-note">北京地域适用于中国内地部署；新加坡地域适用于国际部署。</div>
          </div>
          <NSelect
            v-model:value="qwenAsrWsUrl"
            :options="qwenWsUrlOptions"
            filterable
            tag
            size="small"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Model</div>
            <div class="field-note">默认使用稳定版，也可以切换到快照版做对比。</div>
          </div>
          <NSelect
            v-model:value="qwenAsrModel"
            :options="qwenModelOptions"
            filterable
            tag
            size="small"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Language Hint</div>
            <div class="field-note">例如 `zh`、`en`、`ja`。默认留空，依赖服务自动语种检测，更适合中英混说。</div>
          </div>
          <NInput
            v-model:value="qwenAsrLanguage"
            placeholder="留空为自动检测"
            class="field-control"
          />
        </div>
      </div>
    </div>

    <div v-if="isColi" class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">Local coli Configuration</div>
        <div class="card-sub">
          `coli` 通过本地 CLI 跑离线识别。开启 VAD 后按语音停顿分段，性能稳定；关闭 VAD 时 HUD 可实时出 partial，但长录音性能会下降。
        </div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Detection Status</div>
            <div class="field-note">
              {{ coliStatusError || coliStatus?.message || 'Checking local coli availability…' }}
            </div>
          </div>
          <div
            class="status-pill"
            :class="{
              online: coliStatus?.available,
              offline: !coliStatusLoading && !coliStatus?.available
            }"
          >
            {{ coliStatusLoading ? 'Checking…' : coliStatus?.available ? 'Detected' : 'Not Found' }}
          </div>
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Command Path</div>
            <div class="field-note">
              留空自动探测。若 macOS 从 Finder 启动后找不到 Homebrew PATH，可手动填 `/opt/homebrew/bin/coli`。
            </div>
          </div>
          <div class="field-control action-control">
            <NInput
              v-model:value="coliCommandPath"
              placeholder="Leave empty to auto-detect `coli`"
            />
            <NButton secondary size="small" @click="refreshColiStatus">
              Refresh
            </NButton>
          </div>
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Resolved Path</div>
            <div class="field-note">VoiceX 实际会启动的 `coli` 可执行路径。</div>
          </div>
          <div class="field-value mono">
            {{ coliStatus?.resolvedPath || '—' }}
          </div>
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Model Cache</div>
            <div class="field-note">首次运行会自动下载模型到本地缓存目录。</div>
          </div>
          <div class="field-value mono">
            {{ coliStatus?.modelsDir || '—' }}
          </div>
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Realtime Streaming</div>
            <div class="field-note">
              开启后实时流式识别，HUD 会显示识别中的文字。关闭后进入 Batch 模式：录音期间 HUD 只显示波形，录完后整段音频一次性识别。
            </div>
          </div>
          <NCheckbox v-model:checked="coliRealtime" />
        </div>
        <div v-if="coliRealtime" class="field-row">
          <div class="field-text">
            <div class="field-label">Enable VAD</div>
            <div class="field-note">
              推荐开启。开启后按语音停顿分段识别，性能稳定；关闭后 HUD 可实时显示 partial，但长时间录音会显著变慢。
            </div>
          </div>
          <NCheckbox v-model:checked="coliUseVad" />
        </div>
        <div v-if="coliRealtime && !coliUseVad" class="warning-box">
          关闭 VAD 后，识别引擎每次间隔都会重新处理全部已录制音频，录音超过 30 秒后性能将明显下降。建议仅在需要实时文字预览的短录音场景下关闭。
        </div>
        <div v-if="coliRealtime && !coliUseVad" class="field-row">
          <div class="field-text">
            <div class="field-label">Streaming Interval (ms)</div>
            <div class="field-note">多久刷新一次 partial。数值越小，HUD 更新越频繁。</div>
          </div>
          <NInputNumber
            v-model:value="coliAsrIntervalMs"
            :min="200"
            :max="5000"
            :step="100"
            class="field-control short"
          />
        </div>
        <div v-if="coliRealtime" class="field-row">
          <div class="field-text">
            <div class="field-label">Final Refinement</div>
            <div class="field-note">
              录音结束后对整段音频再跑一次离线识别。适合用更完整上下文提升最终结果，不影响前面的实时 HUD。
            </div>
          </div>
          <NSelect
            v-model:value="coliFinalRefinementMode"
            :options="coliRefinementOptions"
            size="small"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Installed Models</div>
            <div class="field-note">
              SenseVoice 用于实时识别；Whisper 可用于英文最终复核；Silero VAD 只在启用 VAD 时需要。`ffmpeg` 现在只作为环境信息展示，不再是最终复核的前置依赖。
            </div>
          </div>
          <div class="pill-group">
            <span class="mini-pill" :class="{ ready: coliStatus?.sensevoiceInstalled }">
              {{ coliStatus?.sensevoiceInstalled ? 'SenseVoice ready' : 'SenseVoice pending' }}
            </span>
            <span class="mini-pill" :class="{ ready: coliStatus?.whisperInstalled }">
              {{ coliStatus?.whisperInstalled ? 'Whisper ready' : 'Whisper pending' }}
            </span>
            <span class="mini-pill" :class="{ ready: coliStatus?.vadInstalled }">
              {{ coliStatus?.vadInstalled ? 'Silero VAD ready' : 'Silero VAD pending' }}
            </span>
            <span class="mini-pill" :class="{ ready: coliStatus?.ffmpegAvailable }">
              {{ coliStatus?.ffmpegAvailable ? 'ffmpeg ready' : 'ffmpeg missing' }}
            </span>
          </div>
        </div>
      </div>
    </div>

    <!-- Volcengine Recognition Settings -->
    <div class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">Recognition</div>
        <div class="card-sub">录音时长与后处理</div>
      </div>
      <div class="field-list">
        <div v-if="isVolcengine" class="field-row">
          <div class="field-text">
            <div class="field-label">Recognition Mode</div>
          </div>
          <NSelect
            v-model:value="recognitionMode"
            :options="recognitionModeOptions"
            size="small"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Max Recording Duration</div>
            <div class="field-note">Hands-free only. Set to No limit to disable.</div>
          </div>
          <NSelect
            v-model:value="maxRecordingMinutes"
            :options="maxRecordingOptions"
            size="small"
            class="field-control short"
          />
        </div>
        <div v-if="isVolcengine" class="field-row">
          <div class="field-text">
            <div class="field-label">Enable Semantic Smoothing (DDC)</div>
            <div class="field-note">
              去除口语中的停顿词、语气词和重复词，提高文本可读性。
            </div>
          </div>
          <NCheckbox v-model:checked="enableDdc" />
        </div>
        <div v-if="isVolcengine" class="field-row">
          <div class="field-text">
            <div class="field-label">WebSocket URL</div>
            <div class="field-note">Leave URL empty to use the selected mode default.</div>
          </div>
          <NInput v-model:value="asrWsUrl" placeholder="wss://openspeech.bytedance.com/api/v3/..." class="field-control" />
        </div>
      </div>
    </div>

    <!-- Volcengine Endpoint Settings -->
    <div v-if="isVolcengine" class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">Endpoint Settings</div>
        <div class="card-sub">VAD 与出句控制</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">End Window Size (ms)</div>
            <div class="field-note">Leave blank to use service defaults.</div>
          </div>
          <NInputNumber
            v-model:value="endWindowSize"
            :min="0"
            :max="5000"
            placeholder="Service default"
            class="field-control short"
            clearable
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Force To Speech Time (ms)</div>
            <div class="field-note">Leave blank to use service defaults.</div>
          </div>
          <NInputNumber
            v-model:value="forceToSpeechTime"
            :min="0"
            :max="60000"
            placeholder="Service default"
            class="field-control short"
            clearable
          />
        </div>
      </div>
    </div>

  </div>
</template>

<style scoped>
.settings-page {
  width: 100%;
  max-width: 1120px;
  padding-bottom: var(--spacing-2xl);
}

.asr-card {
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

.field-row + .field-row {
  padding-top: 4px;
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

.field-note {
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
  max-width: 520px;
}

.field-control {
  width: 420px;
  max-width: 100%;
}

.field-control.short {
  width: 200px;
}

.action-control {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
}

.sa-json-row {
  align-items: flex-start;
}

.field-value {
  width: 420px;
  max-width: 100%;
  color: var(--color-text-secondary);
  font-size: var(--font-sm);
  text-align: right;
}

.mono {
  font-family: 'SF Mono', 'Menlo', monospace;
  font-size: var(--font-xs);
  word-break: break-all;
}

.status-pill {
  min-width: 96px;
  padding: 6px 10px;
  border-radius: 999px;
  border: 1px solid var(--color-border);
  font-size: var(--font-xs);
  font-weight: 700;
  text-align: center;
  color: var(--color-text-secondary);
  background: color-mix(in srgb, var(--color-bg-secondary) 85%, transparent);
}

.status-pill.online {
  color: #0d6b42;
  border-color: color-mix(in srgb, #0d6b42 35%, var(--color-border));
  background: color-mix(in srgb, #0d6b42 10%, var(--color-bg-secondary));
}

.status-pill.offline {
  color: #9a3412;
  border-color: color-mix(in srgb, #9a3412 35%, var(--color-border));
  background: color-mix(in srgb, #9a3412 10%, var(--color-bg-secondary));
}

.pill-group {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: var(--spacing-sm);
  width: 420px;
  max-width: 100%;
}

.mini-pill {
  padding: 6px 10px;
  border-radius: 999px;
  border: 1px solid var(--color-border);
  font-size: var(--font-xs);
  color: var(--color-text-secondary);
}

.mini-pill.ready {
  color: #0d6b42;
  border-color: color-mix(in srgb, #0d6b42 35%, var(--color-border));
  background: color-mix(in srgb, #0d6b42 10%, var(--color-bg-secondary));
}

.warning-box {
  padding: 10px 12px;
  border-radius: 12px;
  border: 1px solid color-mix(in srgb, #9a3412 35%, var(--color-border));
  background: color-mix(in srgb, #9a3412 10%, var(--color-bg-secondary));
  color: #9a3412;
  font-size: var(--font-xs);
  line-height: 1.5;
}
</style>
