<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NSelect, NSwitch, NTooltip } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'
import type { LocalAsrStatus } from '../types/asr'
import { buildAsrProviderOptions, type AsrProviderValue as ProviderValue } from '../utils/providerOptions'
import { isWindows } from '../utils/platform'
import { ASR_DICTIONARY_POLICIES, isAsrDictionaryEnabled } from '../utils/asrDictionary'
import AsrVolcengineSettings from '../components/asr/AsrVolcengineSettings.vue'
import AsrGoogleSettings from '../components/asr/AsrGoogleSettings.vue'
import AsrFunAsrSettings from '../components/asr/AsrFunAsrSettings.vue'
import AsrQwenSettings from '../components/asr/AsrQwenSettings.vue'
import AsrGeminiSettings from '../components/asr/AsrGeminiSettings.vue'
import AsrCohereSettings from '../components/asr/AsrCohereSettings.vue'
import AsrOpenAISettings from '../components/asr/AsrOpenAISettings.vue'
import AsrElevenLabsSettings from '../components/asr/AsrElevenLabsSettings.vue'
import AsrSonioxSettings from '../components/asr/AsrSonioxSettings.vue'
import AsrStepAudioSettings from '../components/asr/AsrStepAudioSettings.vue'
import AsrMimoSettings from '../components/asr/AsrMimoSettings.vue'
import AsrQwenLocalSettings from '../components/asr/AsrQwenLocalSettings.vue'
import AsrColiSettings from '../components/asr/AsrColiSettings.vue'
import SettingsSection from '../components/settings/SettingsSection.vue'
import SettingRow from '../components/settings/SettingRow.vue'
import ProbeStatus from '../components/settings/ProbeStatus.vue'

const settingsStore = useSettingsStore()
const { t } = useI18n()
interface AsrProviderProbeResult {
  provider: string
  ok: boolean
  recognitionTimeMs: number | null
  recognitionResult: string
  errorMessage: string | null
}

interface SonioxDebugHarnessStatus {
  wsOverride: string | null
  faultMode: string | null
  mockRunning: boolean
  mockUrl: string | null
  mockScenario: string | null
}

// --- Coli status probe ---
const coliStatus = ref<LocalAsrStatus | null>(null)
const coliStatusLoading = ref(false)
const coliStatusError = ref('')
let coliStatusRefreshTimer: number | null = null

async function refreshColiStatus() {
  coliStatusLoading.value = true
  coliStatusError.value = ''
  try {
    coliStatus.value = await invoke<LocalAsrStatus>('probe_local_asr', {
      commandPath: settingsStore.settings.coliCommandPath.trim() || null
    })
  } catch (error) {
    coliStatusError.value = error instanceof Error ? error.message : String(error)
  } finally {
    coliStatusLoading.value = false
  }
}

watch(() => settingsStore.settings.coliCommandPath, () => {
  if (coliStatusRefreshTimer !== null) {
    clearTimeout(coliStatusRefreshTimer)
  }
  coliStatusRefreshTimer = window.setTimeout(() => {
    refreshColiStatus()
    coliStatusRefreshTimer = null
  }, 350)
})

onMounted(() => {
  refreshColiStatus()
  refreshSonioxDebugStatus()
})

// --- Provider selection ---
// The `qwen-asr` CLI this provider drives builds against Accelerate/vDSP by
// default and its live capture is documented as macOS-only, so we have never
// had a working Windows binary to point at. Offering the option there would
// only produce a "command not found" at the first hotkey press.
const qwenLocalUnsupported = isWindows

const asrProviderType = computed({
  get: () => settingsStore.settings.asrProviderType,
  set: (v: ProviderValue) => {
    if (v === 'coli' && coliStatus.value && !coliStatus.value.available) {
      return
    }
    if (v === 'qwen-local' && qwenLocalUnsupported) {
      return
    }
    settingsStore.updateSetting('asrProviderType', v)
  }
})

const isVolcengine = computed(() => settingsStore.settings.asrProviderType === 'volcengine')
const isGoogle = computed(() => settingsStore.settings.asrProviderType === 'google')
const isFunAsr = computed(() => settingsStore.settings.asrProviderType === 'funasr')
const isQwen = computed(() => settingsStore.settings.asrProviderType === 'qwen')
const isGemini = computed(() => settingsStore.settings.asrProviderType === 'gemini')
const isCohere = computed(() => settingsStore.settings.asrProviderType === 'cohere')
const isOpenAI = computed(() => settingsStore.settings.asrProviderType === 'openai')
const isElevenLabs = computed(() => settingsStore.settings.asrProviderType === 'elevenlabs')
const isSoniox = computed(() => settingsStore.settings.asrProviderType === 'soniox')
const isStepAudio = computed(() => settingsStore.settings.asrProviderType === 'stepaudio')
const isMimo = computed(() => settingsStore.settings.asrProviderType === 'mimo')
const isQwenLocal = computed(() => settingsStore.settings.asrProviderType === 'qwen-local')
const isColi = computed(() => settingsStore.settings.asrProviderType === 'coli')
const diagnosticsEnabled = computed(() => settingsStore.settings.enableDiagnostics)

const dictionaryPolicy = computed(() => ASR_DICTIONARY_POLICIES[asrProviderType.value])
const dictionaryEnabled = computed({
  get: () => isAsrDictionaryEnabled(settingsStore.settings),
  set: (enabled: boolean) => settingsStore.updateSetting('asrDictionaryEnabled', {
    ...settingsStore.settings.asrDictionaryEnabled,
    [asrProviderType.value]: enabled
  })
})

watch([diagnosticsEnabled, isSoniox], ([enabled, soniox]) => {
  if (enabled && soniox) {
    refreshSonioxDebugStatus()
  }
})

const providerOptions = computed(() => {
  const coliDetected = coliStatus.value?.available ?? false
  const coliLabel = coliDetected
    ? t('asr.providerColiReady')
    : coliStatusLoading.value
      ? t('asr.providerColiChecking')
      : t('asr.providerColiUnavailable')

  return buildAsrProviderOptions(t, {
    coliLabel,
    coliDisabled: !coliDetected && settingsStore.settings.asrProviderType !== 'coli',
    qwenLocalLabel: qwenLocalUnsupported ? t('asr.providerQwenLocalUnsupported') : undefined,
    // Still selectable if it somehow already is the provider — a config synced
    // from a Mac can name it — so the picker keeps showing what is in effect.
    qwenLocalDisabled:
      qwenLocalUnsupported && settingsStore.settings.asrProviderType !== 'qwen-local'
  })
})

// Reachable only through a synced or hand-edited config; the picker cannot get
// here on its own.
const showQwenLocalUnsupportedWarning = computed(
  () => qwenLocalUnsupported && settingsStore.settings.asrProviderType === 'qwen-local'
)

const showColiUnavailableWarning = computed(() =>
  settingsStore.settings.asrProviderType === 'coli' &&
  !coliStatusLoading.value &&
  !!coliStatus.value &&
  !coliStatus.value.available
)

const providerProbeLoading = ref(false)
const providerProbeResult = ref<AsrProviderProbeResult | null>(null)
const providerProbeError = ref('')
const probeAudioLoading = ref(false)
const isProbeAudioPlaying = ref(false)
const probeAudioPlayer = ref<HTMLAudioElement | null>(null)
const probeAudioUrl = ref<string | null>(null)
const sonioxDebugLoading = ref(false)
const sonioxDebugStatus = ref<SonioxDebugHarnessStatus | null>(null)
const sonioxDebugError = ref('')
const sonioxMockScenario = ref('server_error_502')
const sonioxFaultMode = ref<string | null>('server_error_502')

const sonioxMockScenarioOptions = computed(() => [
  { label: t('asr.sonioxDebugScenarioHappyPath'), value: 'happy_path' },
  { label: t('asr.sonioxDebugScenario401'), value: 'server_error_401' },
  { label: t('asr.sonioxDebugScenario429'), value: 'server_error_429' },
  { label: t('asr.sonioxDebugScenario502'), value: 'server_error_502' },
  { label: t('asr.sonioxDebugScenarioCloseHandshake'), value: 'close_after_handshake' },
  { label: t('asr.sonioxDebugScenarioCloseFirstAudio'), value: 'close_after_first_audio' },
  { label: t('asr.sonioxDebugScenarioPartialClose'), value: 'partial_then_close' },
  { label: t('asr.sonioxDebugScenarioStallFinalizing'), value: 'stall_finalizing' }
])

const sonioxFaultModeOptions = computed(() => [
  { label: t('asr.sonioxDebugFaultNone'), value: 'none' },
  { label: t('asr.sonioxDebugFaultConnectFail'), value: 'connect_fail' },
  { label: t('asr.sonioxDebugFaultHandshakeFail'), value: 'handshake_fail' },
  { label: t('asr.sonioxDebugFault401'), value: 'server_error_401' },
  { label: t('asr.sonioxDebugFault429'), value: 'server_error_429' },
  { label: t('asr.sonioxDebugFault502'), value: 'server_error_502' },
  { label: t('asr.sonioxDebugFaultCloseFirstAudio'), value: 'close_after_first_audio' },
  { label: t('asr.sonioxDebugFaultFinalTimeout'), value: 'final_timeout' }
])

const probeLabel = computed(() => {
  const result = providerProbeResult.value
  if (!result || !result.ok) return t('asr.providerProbeStatusFailed')
  const verdict = t('asr.providerProbeStatusOk')
  return result.recognitionTimeMs !== null ? `${verdict} · ${result.recognitionTimeMs} ms` : verdict
})

const probeErrorMessage = computed(() => providerProbeError.value || providerProbeResult.value?.errorMessage || '')

async function runProviderProbe() {
  providerProbeLoading.value = true
  providerProbeError.value = ''
  try {
    await settingsStore.forceSaveSettings()
    providerProbeResult.value = await invoke<AsrProviderProbeResult>('probe_current_asr_provider')
  } catch (error) {
    providerProbeResult.value = null
    providerProbeError.value = error instanceof Error ? error.message : String(error)
  } finally {
    providerProbeLoading.value = false
  }
}

async function toggleProbeAudioPlayback() {
  if (isProbeAudioPlaying.value) {
    stopProbeAudioPlayback()
    return
  }

  probeAudioLoading.value = true
  try {
    stopProbeAudioPlayback()
    const bytes = await invoke<number[]>('load_provider_probe_audio')
    const buffer = new Uint8Array(bytes)
    const url = URL.createObjectURL(new Blob([buffer], { type: 'audio/ogg' }))
    probeAudioUrl.value = url

    const audio = new Audio(url)
    probeAudioPlayer.value = audio
    audio.onended = () => {
      stopProbeAudioPlayback()
    }
    audio.onerror = () => {
      stopProbeAudioPlayback()
    }

    await audio.play()
    isProbeAudioPlaying.value = true
  } catch (error) {
    stopProbeAudioPlayback()
    providerProbeError.value = error instanceof Error ? error.message : String(error)
  } finally {
    probeAudioLoading.value = false
  }
}

function stopProbeAudioPlayback() {
  if (probeAudioPlayer.value) {
    probeAudioPlayer.value.pause()
    probeAudioPlayer.value.currentTime = 0
    probeAudioPlayer.value = null
  }
  isProbeAudioPlaying.value = false
  if (probeAudioUrl.value) {
    URL.revokeObjectURL(probeAudioUrl.value)
    probeAudioUrl.value = null
  }
}

async function refreshSonioxDebugStatus() {
  if (!diagnosticsEnabled.value) {
    sonioxDebugStatus.value = null
    sonioxDebugError.value = ''
    return
  }

  try {
    sonioxDebugStatus.value = await invoke<SonioxDebugHarnessStatus>('get_soniox_debug_harness_status')
    sonioxDebugError.value = ''
    sonioxFaultMode.value = sonioxDebugStatus.value.faultMode ?? 'none'
    if (sonioxDebugStatus.value.mockScenario) {
      sonioxMockScenario.value = sonioxDebugStatus.value.mockScenario
    }
  } catch (error) {
    sonioxDebugError.value = error instanceof Error ? error.message : String(error)
  }
}

async function startSonioxMockServer() {
  sonioxDebugLoading.value = true
  try {
    sonioxDebugStatus.value = await invoke<SonioxDebugHarnessStatus>('start_soniox_debug_mock_server', {
      scenario: sonioxMockScenario.value
    })
    sonioxDebugError.value = ''
    sonioxFaultMode.value = sonioxDebugStatus.value.faultMode ?? 'none'
  } catch (error) {
    sonioxDebugError.value = error instanceof Error ? error.message : String(error)
  } finally {
    sonioxDebugLoading.value = false
  }
}

async function stopSonioxMockServer() {
  sonioxDebugLoading.value = true
  try {
    sonioxDebugStatus.value = await invoke<SonioxDebugHarnessStatus>('stop_soniox_debug_mock_server')
    sonioxDebugError.value = ''
  } catch (error) {
    sonioxDebugError.value = error instanceof Error ? error.message : String(error)
  } finally {
    sonioxDebugLoading.value = false
  }
}

async function applySonioxFaultMode() {
  sonioxDebugLoading.value = true
  try {
    sonioxDebugStatus.value = await invoke<SonioxDebugHarnessStatus>('set_soniox_debug_fault_mode', {
      faultMode: sonioxFaultMode.value === 'none' ? null : sonioxFaultMode.value
    })
    sonioxDebugError.value = ''
  } catch (error) {
    sonioxDebugError.value = error instanceof Error ? error.message : String(error)
  } finally {
    sonioxDebugLoading.value = false
  }
}

async function clearSonioxDebugOverrides() {
  sonioxDebugLoading.value = true
  try {
    sonioxDebugStatus.value = await invoke<SonioxDebugHarnessStatus>('clear_soniox_debug_overrides')
    sonioxDebugError.value = ''
    sonioxFaultMode.value = 'none'
  } catch (error) {
    sonioxDebugError.value = error instanceof Error ? error.message : String(error)
  } finally {
    sonioxDebugLoading.value = false
  }
}

</script>

<template>
  <div class="page settings-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('asr.title') }}</h1>
    </div>

    <!-- Provider Selection -->
    <SettingsSection :title="t('asr.provider')">
      <template #extra>
        <NButton quaternary size="small" :loading="probeAudioLoading" @click="toggleProbeAudioPlayback">
          {{ isProbeAudioPlaying ? t('asr.providerProbeStopAudio') : t('asr.providerProbePlayAudio') }}
        </NButton>
        <ProbeStatus
          v-if="(providerProbeResult || providerProbeError) && !providerProbeLoading"
          :tone="providerProbeResult?.ok ? 'ok' : 'error'"
          :label="probeLabel"
        >
          <template v-if="providerProbeResult?.ok" #default>
            <div class="probe-detail-label">{{ t('asr.providerProbeTranscript') }}</div>
            <div class="probe-detail-text">{{ providerProbeResult.recognitionResult || t('asr.providerProbeTranscriptEmpty') }}</div>
          </template>
        </ProbeStatus>
        <NTooltip :delay="400" style="max-width: 280px">
          <template #trigger>
            <NButton :loading="providerProbeLoading" type="primary" secondary size="small" @click="runProviderProbe">
              {{ t('asr.providerProbeAction') }}
            </NButton>
          </template>
          {{ t('asr.providerProbeNote') }}
        </NTooltip>
      </template>

      <div v-if="probeErrorMessage && !providerProbeLoading" class="settings-callout is-error">
        {{ probeErrorMessage }}
      </div>
      <SettingRow :label="t('asr.asrProvider')">
        <NSelect
          v-model:value="asrProviderType"
          :options="providerOptions"
          size="small"
        />
      </SettingRow>
      <SettingRow :label="t('asr.useDictionary')" :help="t('asr.useDictionaryHelp')"
        :disabled="dictionaryPolicy.channel === 'unsupported'">
        <template #status>
          <span>{{ t(`asr.dictionaryChannel_${dictionaryPolicy.channel}`) }}</span>
          <span v-if="isGemini">{{ t('asr.dictionaryGeminiNote') }}</span>
        </template>
        <NSwitch v-model:value="dictionaryEnabled" :aria-label="t('asr.useDictionary')"
          :aria-disabled="dictionaryPolicy.channel === 'unsupported'"
          :disabled="dictionaryPolicy.channel === 'unsupported'" />
      </SettingRow>
      <div v-if="showColiUnavailableWarning" class="settings-callout">
        {{ t('asr.warningColiUnavailable') }}
      </div>
      <div v-if="showQwenLocalUnsupportedWarning" class="settings-callout">
        {{ t('asr.warningQwenLocalUnsupported') }}
      </div>
    </SettingsSection>

    <!-- Provider-specific configuration -->
    <AsrVolcengineSettings v-if="isVolcengine" />
    <AsrGoogleSettings v-if="isGoogle" />
    <AsrFunAsrSettings v-if="isFunAsr" />
    <AsrQwenSettings v-if="isQwen" />
    <AsrGeminiSettings v-if="isGemini" />
    <AsrCohereSettings v-if="isCohere" />
    <AsrOpenAISettings v-if="isOpenAI" />
    <AsrElevenLabsSettings v-if="isElevenLabs" />
    <AsrSonioxSettings v-if="isSoniox" />
    <AsrStepAudioSettings v-if="isStepAudio" />
    <AsrMimoSettings v-if="isMimo" />
    <AsrQwenLocalSettings v-if="isQwenLocal" />
    <AsrColiSettings
      v-if="isColi"
      :coli-status="coliStatus"
      :coli-status-loading="coliStatusLoading"
      :coli-status-error="coliStatusError"
      @refresh="refreshColiStatus"
    />

    <SettingsSection v-if="isSoniox && diagnosticsEnabled" :title="t('asr.sonioxDebugTitle')" :help="t('asr.sonioxDebugSub')">
      <SettingRow stacked :label="t('asr.sonioxDebugMockServer')" :help="t('asr.sonioxDebugMockServerNote')">
        <div class="debug-actions">
          <NSelect
            v-model:value="sonioxMockScenario"
            :options="sonioxMockScenarioOptions"
            size="small"
            class="debug-select"
          />
          <NButton
            :loading="sonioxDebugLoading"
            size="small"
            @click="startSonioxMockServer"
          >
            {{ t('asr.sonioxDebugStartMock') }}
          </NButton>
          <NButton
            :loading="sonioxDebugLoading"
            size="small"
            @click="stopSonioxMockServer"
          >
            {{ t('asr.sonioxDebugStopMock') }}
          </NButton>
        </div>
      </SettingRow>

      <SettingRow stacked :label="t('asr.sonioxDebugFaultMode')" :help="t('asr.sonioxDebugFaultModeNote')">
        <div class="debug-actions">
          <NSelect
            v-model:value="sonioxFaultMode"
            :options="sonioxFaultModeOptions"
            size="small"
            class="debug-select"
          />
          <NButton
            :loading="sonioxDebugLoading"
            size="small"
            @click="applySonioxFaultMode"
          >
            {{ t('asr.sonioxDebugApplyFault') }}
          </NButton>
          <NButton
            :loading="sonioxDebugLoading"
            size="small"
            @click="clearSonioxDebugOverrides"
          >
            {{ t('asr.sonioxDebugClear') }}
          </NButton>
        </div>
      </SettingRow>

      <div v-if="sonioxDebugStatus" class="probe-result" :class="{ ok: sonioxDebugStatus.mockRunning || !!sonioxDebugStatus.faultMode }">
        <div class="probe-line">
          <span>{{ t('asr.sonioxDebugActiveMock') }}</span>
          <strong>{{ sonioxDebugStatus.mockScenario || t('asr.sonioxDebugInactive') }}</strong>
        </div>
        <div class="probe-line">
          <span>{{ t('asr.sonioxDebugMockUrl') }}</span>
          <strong>{{ sonioxDebugStatus.mockUrl || t('asr.sonioxDebugInactive') }}</strong>
        </div>
        <div class="probe-line">
          <span>{{ t('asr.sonioxDebugActiveFault') }}</span>
          <strong>{{ sonioxDebugStatus.faultMode || t('asr.sonioxDebugInactive') }}</strong>
        </div>
      </div>

      <div v-if="sonioxDebugError" class="settings-callout is-error">
        {{ sonioxDebugError }}
      </div>
    </SettingsSection>

  </div>
</template>

<style scoped>
@import '../styles/asr-settings.css';

.debug-actions {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
}

.debug-select {
  width: 240px;
}
</style>
