<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { NButton, NInput, NSelect, NSlider, NSwitch } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'
import { formatHotkey } from '../utils/hotkey'
import { isMacOS } from '../utils/platform'

interface TtsVoiceOption {
  id: string
  name: string
  language: string
}

interface ReadSelectionStatus {
  bound: boolean
  enabled: boolean
  conflictsWithDictation: boolean
  display: string | null
}

// The engine's own default rate, i.e. where the 1x mark sits on the stored
// 0..1 scale. Everything the sliders show is a multiple of it.
const DEFAULT_RATE = 0.5

const settingsStore = useSettingsStore()
const { t } = useI18n()

const voices = ref<TtsVoiceOption[]>([])
const voicesError = ref('')
const hotkeyStatus = ref<ReadSelectionStatus | null>(null)
const isRecording = ref(false)
const previewLoading = ref(false)
const previewError = ref('')
// Speech is in progress. Known because the backend reports the end now; before
// the delegate existed there was no such event, which is why this used to be
// two buttons instead of one toggle.
const previewSpeaking = ref(false)
let unlistenPreviewEnded: UnlistenFn | null = null

const DIAGNOSE_COUNTDOWN_S = 5
const diagnoseCountdown = ref(0)
const diagnoseReport = ref<string>('')
const diagnoseError = ref('')
let diagnoseTimer: number | null = null
const showAdvanced = ref(false)

const ttsEnabled = computed({
  get: () => settingsStore.settings.ttsEnabled,
  set: (value: boolean) => {
    settingsStore.updateSetting('ttsEnabled', value)
    void applyHotkey()
  }
})

type ProviderValue = 'system' | 'volcengine' | 'aliyun' | 'mimo' | 'azure'
type AliyunModel = 'qwen3-tts-flash' | 'qwen-audio-3.0-tts-flash' | 'cosyvoice-v3-flash'

const providerOptions = computed(() => [
  { label: t('reading.providerSystem'), value: 'system' },
  { label: t('reading.providerVolcengine'), value: 'volcengine' },
  { label: t('reading.providerAliyun'), value: 'aliyun' },
  { label: t('reading.providerMimo'), value: 'mimo' },
  { label: t('reading.providerAzure'), value: 'azure' }
])

const ttsProviderType = computed({
  get: () => settingsStore.settings.ttsProviderType,
  set: (value: ProviderValue) => {
    settingsStore.updateSetting('ttsProviderType', value)
    // The voice list is per provider and shares nothing across them.
    void loadVoices(value)
  }
})

const isVolcengine = computed(() => settingsStore.settings.ttsProviderType === 'volcengine')
const isAliyun = computed(() => settingsStore.settings.ttsProviderType === 'aliyun')
const isMimo = computed(() => settingsStore.settings.ttsProviderType === 'mimo')
const isAzure = computed(() => settingsStore.settings.ttsProviderType === 'azure')
// Everything that distinguishes "speaks over the network" from "speaks through
// macOS" — voice list availability, the missing pitch control, whether the
// controls work off macOS at all.
const isCloud = computed(
  () => isVolcengine.value || isAliyun.value || isMimo.value || isAzure.value
)
// Empty id is the `say` path (Siri / Spoken Content). Compact AVSpeech voices
// are everything else in the picker; pitch and volume only exist there.
const isSystemDefaultVoice = computed(
  () => !isCloud.value && !settingsStore.settings.systemTtsVoiceId
)

const aliyunModelOptions = computed(() => [
  { label: t('reading.aliyunModelQwen3'), value: 'qwen3-tts-flash' },
  { label: t('reading.aliyunModelQwenAudio'), value: 'qwen-audio-3.0-tts-flash' },
  { label: t('reading.aliyunModelCosyVoice'), value: 'cosyvoice-v3-flash' }
])

const aliyunTtsModel = computed({
  get: () => settingsStore.settings.aliyunTtsModel,
  set: (value: AliyunModel) => {
    settingsStore.updateSetting('aliyunTtsModel', value)
    // The models have entirely separate voice tables.
    void loadVoices('aliyun', value)
  }
})

// Which voice setting the picker is editing. The Alibaba Cloud models
// reject each other's ids, so they cannot share one key: switching model would
// otherwise leave a voice that fails on the next read.
const aliyunVoiceKey = computed(() => {
  switch (settingsStore.settings.aliyunTtsModel) {
    case 'qwen-audio-3.0-tts-flash':
      return 'aliyunTtsVoiceQwenAudio' as const
    case 'cosyvoice-v3-flash':
      return 'aliyunTtsVoiceCosyVoice' as const
    default:
      return 'aliyunTtsVoiceQwen3' as const
  }
})

const voiceOptions = computed(() => {
  const listed = voices.value.map((voice) => ({
    label: `${voice.name} · ${voice.language}`,
    value: voice.id
  }))
  // The cloud providers have no default-voice concept, and an empty speaker is
  // not a valid request — so only the local engine gets that entry.
  return isCloud.value ? listed : [{ label: t('reading.voiceDefault'), value: '' }, ...listed]
})

// Voice identifiers do not carry across providers, so each keeps its own.
const ttsVoiceId = computed({
  get: () => {
    if (isVolcengine.value) return settingsStore.settings.volcTtsSpeaker
    if (isAliyun.value) return settingsStore.settings[aliyunVoiceKey.value]
    if (isMimo.value) return settingsStore.settings.mimoTtsVoice
    if (isAzure.value) return settingsStore.settings.azureTtsVoice
    return settingsStore.settings.systemTtsVoiceId
  },
  set: (value: string) => {
    if (isVolcengine.value) settingsStore.updateSetting('volcTtsSpeaker', value)
    else if (isAliyun.value) settingsStore.updateSetting(aliyunVoiceKey.value, value)
    else if (isMimo.value) settingsStore.updateSetting('mimoTtsVoice', value)
    else if (isAzure.value) settingsStore.updateSetting('azureTtsVoice', value)
    else settingsStore.updateSetting('systemTtsVoiceId', value)
  }
})

const volcTtsApiKey = computed({
  get: () => settingsStore.settings.volcTtsApiKey,
  set: (value: string) => settingsStore.updateSetting('volcTtsApiKey', value)
})

const volcTtsResourceId = computed({
  get: () => settingsStore.settings.volcTtsResourceId,
  set: (value: string) => settingsStore.updateSetting('volcTtsResourceId', value)
})

const aliyunTtsApiKey = computed({
  get: () => settingsStore.settings.aliyunTtsApiKey,
  set: (value: string) => settingsStore.updateSetting('aliyunTtsApiKey', value)
})

const mimoTtsApiKey = computed({
  get: () => settingsStore.settings.mimoTtsApiKey,
  set: (value: string) => settingsStore.updateSetting('mimoTtsApiKey', value)
})

const mimoTtsInstruction = computed({
  get: () => settingsStore.settings.mimoTtsInstruction,
  set: (value: string) => settingsStore.updateSetting('mimoTtsInstruction', value)
})

const azureTtsApiKey = computed({
  get: () => settingsStore.settings.azureTtsApiKey,
  set: (value: string) => settingsStore.updateSetting('azureTtsApiKey', value)
})

const azureTtsRegion = computed({
  get: () => settingsStore.settings.azureTtsRegion,
  set: (value: string) => settingsStore.updateSetting('azureTtsRegion', value)
})

// Rate and volume belong to the provider, not to the feature: engines differ
// in baseline speed and loudness, so tuning one must not move the other.
// MiMo is absent here on purpose: its API has no speed parameter, so the rate
// slider is hidden for it rather than shown doing nothing.
const rateMultiplier = computed({
  get: () => {
    const stored = isVolcengine.value
      ? settingsStore.settings.volcTtsRate
      : isAliyun.value
        ? settingsStore.settings.aliyunTtsRate
        : isAzure.value
          ? settingsStore.settings.azureTtsRate
          : settingsStore.settings.systemTtsRate
    return round2(stored / DEFAULT_RATE)
  },
  set: (value: number) => {
    const stored = clamp(value * DEFAULT_RATE, 0, 1)
    if (isVolcengine.value) settingsStore.updateSetting('volcTtsRate', stored)
    else if (isAliyun.value) settingsStore.updateSetting('aliyunTtsRate', stored)
    else if (isAzure.value) settingsStore.updateSetting('azureTtsRate', stored)
    else settingsStore.updateSetting('systemTtsRate', stored)
  }
})

const volumePercent = computed({
  get: () => {
    const stored = isVolcengine.value
      ? settingsStore.settings.volcTtsVolume
      : isAliyun.value
        ? settingsStore.settings.aliyunTtsVolume
        : isMimo.value
          ? settingsStore.settings.mimoTtsVolume
          : isAzure.value
            ? settingsStore.settings.azureTtsVolume
            : settingsStore.settings.systemTtsVolume
    return Math.round(stored * 100)
  },
  set: (value: number) => {
    const stored = clamp(value / 100, 0, 1)
    if (isVolcengine.value) settingsStore.updateSetting('volcTtsVolume', stored)
    else if (isAliyun.value) settingsStore.updateSetting('aliyunTtsVolume', stored)
    else if (isMimo.value) settingsStore.updateSetting('mimoTtsVolume', stored)
    else if (isAzure.value) settingsStore.updateSetting('azureTtsVolume', stored)
    else settingsStore.updateSetting('systemTtsVolume', stored)
  }
})

// System voice only — no cloud provider exposes pitch.
const pitchMultiplier = computed({
  get: () => round2(settingsStore.settings.systemTtsPitch),
  set: (value: number) => settingsStore.updateSetting('systemTtsPitch', clamp(value, 0.5, 2))
})

const clipboardFallback = computed({
  get: () => settingsStore.settings.ttsClipboardFallback,
  set: (value: boolean) => settingsStore.updateSetting('ttsClipboardFallback', value)
})

const displayHotkey = computed(
  () =>
    hotkeyStatus.value?.display ??
    formatHotkey(settingsStore.settings.ttsHotkeyConfig) ??
    'Option + Command + R'
)

const showConflict = computed(() => hotkeyStatus.value?.conflictsWithDictation ?? false)

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value))
}

function round2(value: number) {
  return Math.round(value * 100) / 100
}

async function applyHotkey() {
  try {
    hotkeyStatus.value = await invoke<ReadSelectionStatus>('apply_read_selection_hotkey', {
      config: settingsStore.settings.ttsHotkeyConfig,
      enabled: settingsStore.settings.ttsEnabled
    })
  } catch (error) {
    console.error('Failed to apply the reading hotkey', error)
  }
}

async function startRecording() {
  isRecording.value = true
  try {
    const result = await invoke<{ storage: string; display: string }>('record_hotkey')
    settingsStore.updateSetting('ttsHotkeyConfig', result.storage)
    await applyHotkey()
  } catch (error) {
    console.error('Hotkey record failed', error)
  } finally {
    isRecording.value = false
  }
}

async function resetHotkey() {
  settingsStore.updateSetting('ttsHotkeyConfig', null)
  await applyHotkey()
}

async function loadVoices(
  provider: ProviderValue = settingsStore.settings.ttsProviderType,
  model: AliyunModel = settingsStore.settings.aliyunTtsModel
) {
  // The cloud providers are network-only, so their voice lists work everywhere;
  // only the system voice needs macOS.
  if (!isMacOS && provider === 'system') {
    voices.value = []
    return
  }
  // Clear first: a slow reply must not leave the previous provider's voices on
  // screen, which is how system voices used to show up under the cloud engine.
  voices.value = []
  try {
    // Both passed explicitly — the store's save is debounced, so the backend
    // would still read the previous provider and model from the database.
    voices.value = await invoke<TtsVoiceOption[]>('list_tts_voices', { provider, model })
    voicesError.value = ''
  } catch (error) {
    voices.value = []
    voicesError.value = error instanceof Error ? error.message : String(error)
  }
}

async function togglePreview() {
  if (previewSpeaking.value) {
    await stopPreview()
    return
  }

  previewLoading.value = true
  previewError.value = ''
  try {
    // The backend reads the voice parameters from the store, so the debounced
    // save has to land before the preview starts or it auditions stale values.
    await settingsStore.forceSaveSettings()
    await invoke('preview_tts', { text: t('reading.previewText') })
    previewSpeaking.value = true
  } catch (error) {
    previewError.value = error instanceof Error ? error.message : String(error)
    previewSpeaking.value = false
  } finally {
    previewLoading.value = false
  }
}

async function stopPreview() {
  // Cleared here rather than waiting for the event: the button must respond to
  // the click, not to the round trip.
  previewSpeaking.value = false
  try {
    await invoke('stop_tts')
  } catch (error) {
    console.error('Failed to stop speech', error)
  }
}

async function runDiagnostics() {
  diagnoseError.value = ''
  diagnoseReport.value = ''
  // Clicking the button puts VoiceX in front, so a read now would only ever
  // find our own window. Count down while the user switches back.
  diagnoseCountdown.value = DIAGNOSE_COUNTDOWN_S
  diagnoseTimer = window.setInterval(() => {
    diagnoseCountdown.value -= 1
    if (diagnoseCountdown.value <= 0 && diagnoseTimer !== null) {
      clearInterval(diagnoseTimer)
      diagnoseTimer = null
    }
  }, 1000)

  try {
    const report = await invoke('diagnose_selection', {
      delayMs: DIAGNOSE_COUNTDOWN_S * 1000
    })
    diagnoseReport.value = JSON.stringify(report, null, 2)
  } catch (error) {
    diagnoseError.value = error instanceof Error ? error.message : String(error)
  } finally {
    diagnoseCountdown.value = 0
    if (diagnoseTimer !== null) {
      clearInterval(diagnoseTimer)
      diagnoseTimer = null
    }
  }
}

async function copyDiagnostics() {
  try {
    await navigator.clipboard.writeText(diagnoseReport.value)
  } catch (error) {
    console.error('Failed to copy the report', error)
  }
}

onMounted(async () => {
  // The dictation hotkey may have changed on another page since this binding
  // was applied, so ask what the state actually is rather than assuming.
  try {
    hotkeyStatus.value = await invoke<ReadSelectionStatus>('read_selection_hotkey_status')
  } catch (error) {
    console.error('Failed to read the reading hotkey status', error)
  }
  await loadVoices()
  unlistenPreviewEnded = await listen('tts:preview_ended', () => {
    previewSpeaking.value = false
  })
})

onBeforeUnmount(() => {
  unlistenPreviewEnded?.()
  unlistenPreviewEnded = null
  if (diagnoseTimer !== null) {
    clearInterval(diagnoseTimer)
    diagnoseTimer = null
  }
})
</script>

<template>
  <div class="page settings-page reading-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('reading.title') }}</h1>
    </div>

    <div v-if="!isMacOS" class="surface-card asr-card">
      <div class="warning-box">{{ t('reading.unsupportedPlatform') }}</div>
    </div>

    <!-- The feature itself comes first — whether it is on, and which key. Both
         are independent of the engine, so leaving them between the engine and
         the engine's own parameters split one subject across two cards with an
         unrelated one wedged in the middle. -->
    <div class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('reading.general') }}</div>
        <div class="card-sub">{{ t('reading.generalSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('reading.enabled') }}</div>
            <div class="field-note">{{ t('reading.enabledNote') }}</div>
          </div>
          <div class="field-control end">
            <NSwitch v-model:value="ttsEnabled" :disabled="!isMacOS" />
          </div>
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('reading.hotkey') }}</div>
            <div class="field-note">{{ t('reading.hotkeyNote') }}</div>
            <div class="field-note">{{ t('reading.hotkeySystemNote') }}</div>
          </div>
          <div class="field-control end">
            <div class="hotkey-display" :class="{ recording: isRecording }">
              {{ isRecording ? t('reading.pressHotkey') : displayHotkey }}
            </div>
            <div class="hotkey-actions">
              <NButton
                :disabled="isRecording || !ttsEnabled || !isMacOS"
                size="small"
                @click="startRecording"
              >
                {{ t('reading.record') }}
              </NButton>
              <NButton
                v-if="settingsStore.settings.ttsHotkeyConfig && !isRecording"
                quaternary
                size="small"
                @click="resetHotkey"
              >
                {{ t('reading.clear') }}
              </NButton>
            </div>
          </div>
        </div>

        <div v-if="showConflict" class="warning-box">
          {{ t('reading.hotkeyConflict') }}
        </div>
      </div>
    </div>

    <div class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('reading.provider') }}</div>
        <div class="card-sub">{{ t('reading.providerSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('reading.ttsProvider') }}</div>
          </div>
          <NSelect
            v-model:value="ttsProviderType"
            :options="providerOptions"
            size="small"
            class="field-control"
          />
        </div>

        <!-- Plan §3.4: told once, when the engine is chosen, rather than
             asked on every read. -->
        <div v-if="isCloud" class="notice-box">{{ t('reading.cloudPrivacy') }}</div>

        <template v-if="isVolcengine">
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('reading.volcApiKey') }}</div>
              <div class="field-note">{{ t('reading.volcApiKeyNote') }}</div>
            </div>
            <NInput
              v-model:value="volcTtsApiKey"
              type="password"
              show-password-on="click"
              size="small"
              class="field-control"
              :placeholder="t('reading.volcApiKeyPlaceholder')"
            />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('reading.volcResourceId') }}</div>
              <div class="field-note">{{ t('reading.volcResourceIdNote') }}</div>
            </div>
            <NInput
              v-model:value="volcTtsResourceId"
              size="small"
              class="field-control"
              placeholder="seed-tts-2.0"
            />
          </div>
        </template>

        <template v-if="isAliyun">
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('reading.aliyunApiKey') }}</div>
              <div class="field-note">{{ t('reading.aliyunApiKeyNote') }}</div>
            </div>
            <NInput
              v-model:value="aliyunTtsApiKey"
              type="password"
              show-password-on="click"
              size="small"
              class="field-control"
              placeholder="sk-..."
            />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('reading.aliyunModel') }}</div>
              <div class="field-note">{{ t('reading.aliyunModelNote') }}</div>
            </div>
            <NSelect
              v-model:value="aliyunTtsModel"
              :options="aliyunModelOptions"
              size="small"
              class="field-control"
            />
          </div>
        </template>

        <template v-if="isMimo">
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('reading.mimoApiKey') }}</div>
              <div class="field-note">{{ t('reading.mimoApiKeyNote') }}</div>
            </div>
            <NInput
              v-model:value="mimoTtsApiKey"
              type="password"
              show-password-on="click"
              size="small"
              class="field-control"
              :placeholder="t('reading.mimoApiKeyPlaceholder')"
            />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('reading.mimoInstruction') }}</div>
              <div class="field-note">{{ t('reading.mimoInstructionNote') }}</div>
            </div>
            <NInput
              v-model:value="mimoTtsInstruction"
              size="small"
              class="field-control"
              :placeholder="t('reading.mimoInstructionPlaceholder')"
            />
          </div>
        </template>

        <template v-if="isAzure">
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('reading.azureApiKey') }}</div>
              <div class="field-note">{{ t('reading.azureApiKeyNote') }}</div>
            </div>
            <NInput
              v-model:value="azureTtsApiKey"
              type="password"
              show-password-on="click"
              size="small"
              class="field-control"
              :placeholder="t('reading.azureApiKeyPlaceholder')"
            />
          </div>
          <div class="field-row">
            <div class="field-text">
              <div class="field-label">{{ t('reading.azureRegion') }}</div>
              <div class="field-note">{{ t('reading.azureRegionNote') }}</div>
            </div>
            <NInput
              v-model:value="azureTtsRegion"
              size="small"
              class="field-control"
              placeholder="eastus"
            />
          </div>
        </template>
      </div>
    </div>

    <div class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('reading.voice') }}</div>
        <div class="card-sub">{{ t('reading.voiceSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('reading.voiceLabel') }}</div>
            <div class="field-note">
              {{
                isCloud
                  ? t('reading.cloudSpeakerNote')
                  : isSystemDefaultVoice
                    ? t('reading.voiceNoteDefault')
                    : t('reading.voiceNote')
              }}
            </div>
          </div>
          <NSelect
            v-model:value="ttsVoiceId"
            :options="voiceOptions"
            :disabled="!isMacOS && !isCloud"
            :tag="isCloud"
            filterable
            size="small"
            class="field-control"
          />
        </div>

        <div v-if="voicesError" class="warning-box">
          {{ t('reading.voiceLoadFailed') }} — {{ voicesError }}
        </div>

        <!-- MiMo has no speed parameter — its only delivery control is the
             instruction text — so the slider is hidden there rather than
             shown doing nothing. -->
        <div v-if="!isMimo" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('reading.rate') }}</div>
          </div>
          <div class="field-control end">
            <NSlider
              v-model:value="rateMultiplier"
              :min="0.5"
              :max="2"
              :step="0.05"
              :disabled="!isMacOS && !isCloud"
              class="slider"
            />
            <span class="slider-value">{{ rateMultiplier.toFixed(2) }}x</span>
          </div>
        </div>

        <!-- Pitch and volume exist on compact AVSpeech voices, not on `say`.
             Hiding the rows when the system default is selected is honest:
             `say` has no flags for either, and a slider that does nothing
             would look like a broken setting. Cloud volume is local playback
             gain, so that row stays. -->
        <div v-if="!isCloud && !isSystemDefaultVoice" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('reading.pitch') }}</div>
          </div>
          <div class="field-control end">
            <NSlider
              v-model:value="pitchMultiplier"
              :min="0.5"
              :max="2"
              :step="0.05"
              :disabled="!isMacOS"
              class="slider"
            />
            <span class="slider-value">{{ pitchMultiplier.toFixed(2) }}x</span>
          </div>
        </div>

        <div v-if="isCloud || !isSystemDefaultVoice" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('reading.volume') }}</div>
          </div>
          <div class="field-control end">
            <NSlider
              v-model:value="volumePercent"
              :min="0"
              :max="100"
              :step="5"
              :disabled="!isMacOS && !isCloud"
              class="slider"
            />
            <span class="slider-value">{{ volumePercent }}%</span>
          </div>
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('reading.preview') }}</div>
            <div class="field-note">{{ t('reading.previewNote') }}</div>
          </div>
          <div class="field-control end">
            <NButton
              :loading="previewLoading"
              :disabled="!isMacOS && !isCloud"
              :type="previewSpeaking ? 'default' : 'primary'"
              secondary
              size="small"
              @click="togglePreview"
            >
              {{ previewSpeaking ? t('reading.previewStop') : t('reading.preview') }}
            </NButton>
          </div>
        </div>

        <div v-if="previewError" class="warning-box">
          {{ t('reading.previewFailed') }} — {{ previewError }}
        </div>
      </div>
    </div>

    <div v-if="settingsStore.settings.enableDiagnostics" class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('reading.diagnostics') }}</div>
        <div class="card-sub">{{ t('reading.diagnosticsSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row align-start">
          <div class="field-text">
            <div class="field-label">{{ t('reading.diagnosticsRun') }}</div>
            <div class="field-note">{{ t('reading.diagnosticsHint') }}</div>
          </div>
          <div class="field-control end">
            <NButton
              :loading="diagnoseCountdown > 0"
              :disabled="diagnoseCountdown > 0"
              size="small"
              @click="runDiagnostics"
            >
              {{
                diagnoseCountdown > 0
                  ? t('reading.diagnosticsCountdown', { seconds: diagnoseCountdown })
                  : t('reading.diagnosticsRun')
              }}
            </NButton>
            <NButton
              v-if="diagnoseReport"
              quaternary
              size="small"
              @click="copyDiagnostics"
            >
              {{ t('common.copy') }}
            </NButton>
          </div>
        </div>

        <pre v-if="diagnoseReport" class="diagnostics-report">{{ diagnoseReport }}</pre>
        <div v-if="diagnoseError" class="warning-box">{{ diagnoseError }}</div>
      </div>
    </div>

    <div class="surface-card asr-card">
      <button class="advanced-toggle" @click="showAdvanced = !showAdvanced">
        <span class="card-title">{{ t('reading.advanced') }}</span>
        <span class="advanced-chevron" :class="{ open: showAdvanced }">›</span>
      </button>
      <div v-if="showAdvanced" class="field-list advanced-body">
        <div class="card-sub">{{ t('reading.advancedSub') }}</div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('reading.clipboardFallback') }}</div>
            <div class="field-note">{{ t('reading.clipboardFallbackNote') }}</div>
          </div>
          <div class="field-control end">
            <NSwitch v-model:value="clipboardFallback" :disabled="!isMacOS" />
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
@import '../styles/asr-settings.css';

.settings-page {
  width: 100%;
  max-width: 1120px;
  padding-bottom: var(--spacing-2xl);
}

.field-control.end {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
  justify-content: flex-end;
}

.hotkey-display {
  flex: 1;
  padding: 6px var(--spacing-lg);
  background-color: var(--color-bg-tertiary);
  border: 1px solid var(--color-border);
  border-radius: var(--radius-md);
  font-family: ui-monospace, monospace;
  font-size: var(--font-md);
  color: var(--color-text-primary);
  min-height: 28px;
  display: flex;
  align-items: center;
}

.hotkey-display.recording {
  border-color: var(--color-accent);
  box-shadow: 0 0 0 2px var(--color-accent-light);
}

.hotkey-actions {
  display: flex;
  gap: var(--spacing-sm);
}

.slider {
  flex: 1;
}

.slider-value {
  width: 56px;
  text-align: right;
  font-size: var(--font-xs);
  color: var(--color-text-secondary);
  font-variant-numeric: tabular-nums;
}

.advanced-toggle {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
  width: 100%;
  text-align: left;
  color: var(--color-text-primary);
}

.advanced-chevron {
  color: var(--color-text-tertiary);
  transition: transform var(--transition-fast);
}

.advanced-chevron.open {
  transform: rotate(90deg);
}

.advanced-body {
  margin-top: var(--spacing-md);
}

.diagnostics-report {
  margin: 0;
  padding: 12px 14px;
  border-radius: 12px;
  background: var(--color-bg-tertiary);
  border: 1px solid var(--color-border);
  font-family: 'SF Mono', 'Menlo', monospace;
  font-size: var(--font-xs);
  line-height: 1.55;
  color: var(--color-text-secondary);
  max-height: 320px;
  overflow: auto;
  white-space: pre;
}
</style>
