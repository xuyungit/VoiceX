<script setup lang="ts">
import { ref, computed, watch, onMounted, onBeforeUnmount } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { NButton, NSelect, NInputNumber, NCheckbox, NSwitch, NTooltip } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore, type AppSettings } from '../stores/settings'
import { usePostEditStore } from '../stores/postEdit'
import { formatDictationHotkey } from '../utils/hotkey'
import type { OutputMuteStatus } from '../utils/outputMute'
import type { PostEditTestSnapshot } from '../types/postEdit'
import SettingsSection from '../components/settings/SettingsSection.vue'
import SettingRow from '../components/settings/SettingRow.vue'
import HelpTip from '../components/settings/HelpTip.vue'
import HotkeyField from '../components/settings/HotkeyField.vue'
import {
  exceedsRecordingHardLimit,
  resolveAsrRecordingHardLimitMinutes
} from '../utils/providerOptions'

type TextInjectionModeValue = AppSettings['textInjectionMode']
type TextInjectionOverride = AppSettings['textInjectionOverrides'][number]
type TextInjectionOverrideSelectValue = TextInjectionModeValue | 'default'

interface RecentTargetApp {
  platform: string
  appName: string
  matchKind: string
  matchValue: string
  displayName: string | null
  processName: string | null
  bundleId: string | null
  executablePath: string | null
  lastSeenAt: string | null
}

interface InjectionAppRow extends RecentTargetApp {
  key: string
  overrideMode: TextInjectionModeValue | null
  overrideSkipRestore: boolean | null
}

const settingsStore = useSettingsStore()
const postEditStore = usePostEditStore()
const { t } = useI18n()
const recentTargetApps = ref<RecentTargetApp[]>([])
const postEditTest = ref<PostEditTestSnapshot | null>(null)
const postEditTestError = ref<string | null>(null)
let postEditTestTimer: ReturnType<typeof setInterval> | null = null
let postEditTestRequest = 0
// Observation internals help when adapting apps; users only see the switch.
const showPostEditDiagnostics = import.meta.env.DEV
const postEditEnabled = computed({
  get: () => postEditStore.enabled,
  set: (value: boolean) => { void postEditStore.setEnabled(value) }
})

async function refreshPostEditTest() {
  const request = ++postEditTestRequest
  try {
    const result = await invoke<PostEditTestSnapshot>('get_post_edit_test_status')
    if (!inputSettingsDisposed && request === postEditTestRequest) {
      postEditTest.value = result
      postEditTestError.value = null
    }
  } catch (error) {
    if (!inputSettingsDisposed && request === postEditTestRequest) postEditTestError.value = String(error)
  }
}

watch(() => postEditStore.settings?.mode, () => { if (showPostEditDiagnostics) void refreshPostEditTest() })

const loadingRecentTargetApps = ref(false)
let unlistenRecentTargetApps: UnlistenFn | null = null

const textInjectionMode = computed({
  get: () => settingsStore.settings.textInjectionMode,
  set: (v: TextInjectionModeValue) => settingsStore.updateSetting('textInjectionMode', v)
})

const injectionOptions = computed(() => [
  { label: t('input.pasteboard'), value: 'pasteboard' },
  { label: t('input.typing'), value: 'typing' }
])

const translationTriggerMode = computed({
  get: () => settingsStore.settings.translationTriggerMode,
  set: (v: 'double_tap' | 'off') => settingsStore.updateSetting('translationTriggerMode', v)
})

const doubleTapWindowMs = computed({
  get: () => settingsStore.settings.doubleTapWindowMs,
  set: (v: number | null) => {
    const value = Math.round(Math.max(250, Math.min(700, Number(v ?? 400))))
    settingsStore.updateSetting('doubleTapWindowMs', value)
  }
})

const translationEnabled = computed({
  get: () => settingsStore.settings.translationEnabled,
  set: (v: boolean) => settingsStore.updateSetting('translationEnabled', v)
})

// Mirrors can_open_double_tap_window in src-tauri/src/state.rs: the double tap
// only does something when translation is on and bound to it.
const doubleTapActive = computed(() =>
  translationEnabled.value && translationTriggerMode.value === 'double_tap'
)

const gestureLegend = computed(() => [
  { gesture: t('input.gestureTap'), action: t('input.actionHandsFree') },
  { gesture: t('input.gestureHold'), action: t('input.actionPushToTalk') },
  ...(doubleTapActive.value
    ? [{ gesture: t('input.gestureDoubleTap'), action: t('input.actionTranslate') }]
    : [])
])

const translationTriggerOptions = computed(() => [
  { label: t('input.translationTriggerDoubleTap'), value: 'double_tap' },
  { label: t('input.translationTriggerOff'), value: 'off' }
])

const maxRecordingOptions = computed(() => [
  { label: t('asr.noLimit'), value: 0 },
  { label: t('asr.oneMinute'), value: 1 },
  { label: t('asr.fiveMinutes'), value: 5 },
  { label: t('asr.tenMinutes'), value: 10 },
  { label: t('asr.thirtyMinutes'), value: 30 }
])

const maxRecordingMinutes = computed({
  get: () => settingsStore.settings.maxRecordingMinutes,
  set: (v: number) => settingsStore.updateSetting('maxRecordingMinutes', v)
})

const hudTransparent = computed({
  get: () => settingsStore.settings.hudTransparent,
  set: (v: boolean) => {
    settingsStore.updateSetting('hudTransparent', v)
    void settingsStore.forceSaveSettings()
  }
})

const outputMuteStatus = ref<OutputMuteStatus | null>(null)
const outputMuteQueryError = ref<string | null>(null)
let unlistenOutputMute: UnlistenFn | null = null
let inputSettingsDisposed = false

const muteOutputWhileRecording = computed({
  get: () => settingsStore.settings.muteOutputWhileRecording,
  set: (v: boolean) => {
    settingsStore.updateSetting('muteOutputWhileRecording', v)
    void settingsStore.forceSaveSettings()
  }
})

// Only problems are worth a line: a device that cannot be muted would
// otherwise look muted to someone who just turned the switch on.
const outputMuteHint = computed(() => {
  const status = outputMuteStatus.value
  if (outputMuteQueryError.value) return t('input.muteWhileRecordingError', { message: outputMuteQueryError.value })
  if (!status) return null
  if (status.recoveryError) return t('input.muteRecoveryError', { message: status.recoveryError })
  if (status.pendingRestores.length) {
    const devices = status.pendingRestores.map(d => d.error ? `${d.device}: ${d.error}` : d.device).join('; ')
    return t('input.muteRestorePending', { devices })
  }
  if (!muteOutputWhileRecording.value && !status.recording) return null
  if (status.support === 'unsupported') {
    return t('input.muteWhileRecordingUnsupported', { device: status.device })
  }
  if (status.support === 'error') {
    return t('input.muteWhileRecordingError', { message: status.message })
  }
  if (status.support === 'no_device') return t('input.muteNoOutputDevice')
  return null
})

async function refreshOutputMuteStatus() {
  try {
    const status = await invoke<OutputMuteStatus>('get_output_mute_status')
    if (!inputSettingsDisposed && status.revision >= (outputMuteStatus.value?.revision ?? 0)) {
      outputMuteStatus.value = status
      outputMuteQueryError.value = null
    }
  } catch (error) {
    if (!inputSettingsDisposed) outputMuteQueryError.value = String(error)
  }
}

watch(
  () => settingsStore.settings.muteOutputWhileRecording,
  () => void refreshOutputMuteStatus(),
  { immediate: true }
)

const asrRecordingHardLimitMinutes = computed(() =>
  resolveAsrRecordingHardLimitMinutes(settingsStore.settings)
)

const showMaxRecordingDurationLimitHint = computed(() =>
  exceedsRecordingHardLimit(maxRecordingMinutes.value, asrRecordingHardLimitMinutes.value)
)

const maxRecordingDurationLimitHint = computed(() => {
  if (!showMaxRecordingDurationLimitHint.value || asrRecordingHardLimitMinutes.value == null) {
    return null
  }

  return t('asr.maxRecordingDurationProviderCapNote', {
    minutes: asrRecordingHardLimitMinutes.value
  })
})

const isRecording = ref(false)
const recordedKey = ref('')
const displayHotkey = ref(t('input.clickToRecord'))

interface InputDevice {
  uid: string
  name: string
  isDefault: boolean
}

const devices = ref<InputDevice[]>([])
const deviceOptions = computed(() =>
  devices.value.map((d) => ({
    label: d.isDefault ? t('input.defaultDevice', { name: d.name }) : d.name,
    value: d.uid
  }))
)
const selectedDevice = ref<string | null>(null)
const loadingDevices = ref(false)
const devicesLoaded = ref(false)
// "System Default" is always listed, so it alone means nothing can record.
const noInputDevice = computed(() =>
  devicesLoaded.value && devices.value.every((d) => d.isDefault)
)

const hotkeyDisplay = computed(() => displayHotkey.value)
const textInjectionOverrideMap = computed(() => {
  const overrideMap = new Map<string, TextInjectionOverride>()
  for (const overrideItem of settingsStore.settings.textInjectionOverrides) {
    overrideMap.set(getAppRuleKey(overrideItem), overrideItem)
  }
  return overrideMap
})

function compareAppRows(a: InjectionAppRow, b: InjectionAppRow) {
  if (a.lastSeenAt && b.lastSeenAt) {
    return new Date(b.lastSeenAt).getTime() - new Date(a.lastSeenAt).getTime()
  }
  if (a.lastSeenAt) return -1
  if (b.lastSeenAt) return 1
  return a.appName.localeCompare(b.appName)
}

const injectionAppRows = computed<InjectionAppRow[]>(() => {
  const rows = new Map<string, InjectionAppRow>()

  for (const app of recentTargetApps.value) {
    const key = getAppRuleKey(app)
    const override = textInjectionOverrideMap.value.get(key)
    rows.set(key, {
      ...app,
      key,
      overrideMode: override?.mode ?? null,
      overrideSkipRestore: override ? (override.skipClipboardRestore ?? false) : null
    })
  }

  for (const overrideItem of settingsStore.settings.textInjectionOverrides) {
    const key = getAppRuleKey(overrideItem)
    const existing = rows.get(key)
    if (existing) {
      existing.overrideMode = overrideItem.mode
      existing.overrideSkipRestore = overrideItem.skipClipboardRestore ?? false
      if (!existing.appName) {
        existing.appName = overrideItem.appName
      }
      continue
    }

    rows.set(key, {
      key,
      platform: overrideItem.platform,
      appName: overrideItem.appName,
      matchKind: overrideItem.matchKind,
      matchValue: overrideItem.matchValue,
      displayName: overrideItem.appName,
      processName: overrideItem.matchKind === 'process_name' ? overrideItem.appName : null,
      bundleId: overrideItem.matchKind === 'bundle_id' ? overrideItem.matchValue : null,
      executablePath:
        overrideItem.matchKind === 'executable_path' ? overrideItem.matchValue : null,
      lastSeenAt: null,
      overrideMode: overrideItem.mode,
      overrideSkipRestore: overrideItem.skipClipboardRestore ?? false
    })
  }

  return Array.from(rows.values()).sort(compareAppRows)
})

const configuredInjectionAppRows = computed(() =>
  injectionAppRows.value.filter((app) => app.overrideMode !== null).sort(compareAppRows)
)

const recentUnconfiguredAppRows = computed(() =>
  injectionAppRows.value.filter((app) => app.overrideMode === null).sort(compareAppRows)
)

const overrideGroups = computed(() =>
  [
    { key: 'configured', title: t('input.configuredApps'), rows: configuredInjectionAppRows.value },
    { key: 'recent', title: t('input.recentApps'), rows: recentUnconfiguredAppRows.value }
  ].filter((group) => group.rows.length > 0)
)

watch(
  () => settingsStore.settings.hotkeyConfig,
  (config) => {
    displayHotkey.value = formatDictationHotkey(config)
  },
  { immediate: true }
)

async function startRecording() {
  isRecording.value = true
  recordedKey.value = t('input.pressHotkey')
  try {
    const result = await invoke<{ storage: string; display: string }>('record_hotkey')
    settingsStore.updateSetting('hotkeyConfig', result.storage)
    displayHotkey.value = result.display
    await applyHotkey(result.storage)
  } catch (error) {
    console.error('Hotkey record failed', error)
  } finally {
    isRecording.value = false
  }
}

async function applyHotkey(storage: string | null) {
  try {
    await invoke('apply_hotkey_config', { config: storage })
  } catch (error) {
    console.error('Failed to apply hotkey config', error)
  }
}

// No saved hotkey means the default one, so clearing restores the default.
async function restoreDefaultHotkey() {
  settingsStore.updateSetting('hotkeyConfig', null)
  displayHotkey.value = formatDictationHotkey(null)
  await applyHotkey(null)
}

watch(
  () => settingsStore.settings.inputDeviceUid,
  (uid) => {
    if (uid && uid !== selectedDevice.value) {
      selectedDevice.value = uid
    }
  }
)

async function applyDevice(uid: string | null) {
  if (!uid) return
  settingsStore.updateSetting('inputDeviceUid', uid)
  try {
    await invoke('set_input_device', { uid })
  } catch (error) {
    console.error('Failed to set input device:', error)
  }
}

watch(
  () => selectedDevice.value,
  (uid) => {
    if (uid) {
      applyDevice(uid)
    }
  }
)

async function refreshDevices() {
  loadingDevices.value = true
  try {
    devices.value = await invoke<InputDevice[]>('get_input_devices')
    devicesLoaded.value = true

    if (devices.value.length > 0) {
      const preferred = settingsStore.settings.inputDeviceUid
      const fromSettings = devices.value.find((d) => d.uid === preferred)
      const fallback = devices.value.find((d) => d.isDefault) ?? devices.value[0]
      selectedDevice.value = (fromSettings ?? fallback).uid
    } else {
      selectedDevice.value = null
    }
  } catch (error) {
    console.error('Failed to load input devices:', error)
  } finally {
    loadingDevices.value = false
  }
}

async function loadRecentTargetApps() {
  loadingRecentTargetApps.value = true
  try {
    recentTargetApps.value = await invoke<RecentTargetApp[]>('get_recent_target_apps')
  } catch (error) {
    console.error('Failed to load recent target apps:', error)
    recentTargetApps.value = []
  } finally {
    loadingRecentTargetApps.value = false
  }
}

// The default output may have changed while the window was away.
function handleWindowFocus() {
  void loadRecentTargetApps()
  void refreshOutputMuteStatus()
  void refreshDevices()
}

function handleVisibilityChange() {
  if (!document.hidden) {
    void loadRecentTargetApps()
    void refreshOutputMuteStatus()
    void refreshDevices()
  }
}

// While nothing can record, look again every few seconds so a microphone
// plugged in with this page open clears the warning without a click.
const NO_DEVICE_POLL_MS = 3000
let noDevicePollTimer: ReturnType<typeof setInterval> | null = null
watch(noInputDevice, (missing) => {
  if (missing && !noDevicePollTimer) {
    noDevicePollTimer = setInterval(() => {
      if (!document.hidden && !loadingDevices.value) void refreshDevices()
    }, NO_DEVICE_POLL_MS)
  } else if (!missing && noDevicePollTimer) {
    clearInterval(noDevicePollTimer)
    noDevicePollTimer = null
  }
})

function getAppRuleKey(app: {
  platform: string
  matchKind: string
  matchValue: string
}) {
  return [
    app.platform.trim().toLowerCase(),
    app.matchKind.trim(),
    app.matchValue.trim().toLowerCase()
  ].join('::')
}

function getInjectionModeLabel(mode: TextInjectionModeValue) {
  return mode === 'typing' ? t('input.typing') : t('input.pasteboard')
}

function getOverrideOptions() {
  return [
    {
      label: t('input.followDefaultMode', {
        mode: getInjectionModeLabel(textInjectionMode.value)
      }),
      value: 'default'
    },
    { label: t('input.pasteboard'), value: 'pasteboard' },
    { label: t('input.typing'), value: 'typing' }
  ]
}

function updateTextInjectionOverride(
  app: InjectionAppRow,
  value: TextInjectionOverrideSelectValue | null
) {
  const key = getAppRuleKey(app)
  const existingSkipRestore = settingsStore.settings.textInjectionOverrides.find(
    (overrideItem) => getAppRuleKey(overrideItem) === key
  )?.skipClipboardRestore
  const nextOverrides = settingsStore.settings.textInjectionOverrides.filter(
    (overrideItem) => getAppRuleKey(overrideItem) !== key
  )

  if (value === 'pasteboard' || value === 'typing') {
    nextOverrides.push({
      platform: app.platform,
      appName: app.appName,
      matchKind: app.matchKind,
      matchValue: app.matchValue,
      mode: value,
      skipClipboardRestore: existingSkipRestore ?? false
    })
  }

  settingsStore.updateSetting('textInjectionOverrides', nextOverrides)
}

function updateSkipClipboardRestore(app: InjectionAppRow, checked: boolean) {
  const key = getAppRuleKey(app)
  const nextOverrides = settingsStore.settings.textInjectionOverrides.map((overrideItem) =>
    getAppRuleKey(overrideItem) === key
      ? { ...overrideItem, skipClipboardRestore: checked }
      : overrideItem
  )

  settingsStore.updateSetting('textInjectionOverrides', nextOverrides)
}

function getAppIdentityLabel(app: InjectionAppRow) {
  if (app.bundleId) {
    return t('input.bundleIdValue', { value: app.bundleId })
  }
  if (app.processName) {
    return t('input.processNameValue', { value: app.processName })
  }
  return app.platform
}

function getAppUsageLabel(app: InjectionAppRow) {
  if (!app.lastSeenAt) {
    return t('input.savedOverride')
  }

  return t('input.lastCapturedAt', {
    time: new Intl.DateTimeFormat(undefined, {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit'
    }).format(new Date(app.lastSeenAt))
  })
}

function getAppBadgeLabel(appName: string) {
  const trimmed = appName.trim()
  return (trimmed[0] ?? '?').toUpperCase()
}

onMounted(async () => {
  void postEditStore.load()
  if (showPostEditDiagnostics) {
    void refreshPostEditTest()
    postEditTestTimer = setInterval(() => {
      if (postEditStore.enabled && !document.hidden) void refreshPostEditTest()
    }, 1000)
  }
  const unlistenMute = await listen<OutputMuteStatus>('audio:output-mute-status', event => {
    if (event.payload.revision < (outputMuteStatus.value?.revision ?? 0)) return
    outputMuteStatus.value = event.payload
    outputMuteQueryError.value = null
  })
  if (inputSettingsDisposed) { unlistenMute(); return }
  unlistenOutputMute = unlistenMute
  await refreshOutputMuteStatus()
  if (inputSettingsDisposed) return
  const unlistenRecent = await listen('input:recent-target-apps-updated', () => {
    void loadRecentTargetApps()
  })
  if (inputSettingsDisposed) { unlistenRecent(); return }
  unlistenRecentTargetApps = unlistenRecent
  window.addEventListener('focus', handleWindowFocus)
  document.addEventListener('visibilitychange', handleVisibilityChange)
  await Promise.all([refreshDevices(), loadRecentTargetApps()])
})

onBeforeUnmount(() => {
  inputSettingsDisposed = true
  if (postEditTestTimer) clearInterval(postEditTestTimer)
  if (noDevicePollTimer) clearInterval(noDevicePollTimer)
  unlistenOutputMute?.()
  if (unlistenRecentTargetApps) {
    unlistenRecentTargetApps()
    unlistenRecentTargetApps = null
  }
  window.removeEventListener('focus', handleWindowFocus)
  document.removeEventListener('visibilitychange', handleVisibilityChange)
})
</script>

<template>
  <div class="page settings-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('input.title') }}</h1>
    </div>

    <SettingsSection :title="t('input.hotkeys')">
      <template #extra>
        <ul class="gesture-legend">
          <li v-for="item in gestureLegend" :key="item.gesture" class="info-pill">
            <span class="info-pill-label">{{ item.gesture }}</span>
            <span class="info-pill-value">{{ item.action }}</span>
          </li>
        </ul>
      </template>

      <SettingRow :label="t('input.recordingHotkey')" :help="t('input.recordingHotkeyNote')">
        <HotkeyField
          :value="isRecording ? recordedKey : hotkeyDisplay"
          :recording="isRecording"
          :clearable="!!settingsStore.settings.hotkeyConfig"
          :clear-label="t('input.restoreDefaultHotkey')"
          @clear="restoreDefaultHotkey"
        />
        <NButton :disabled="isRecording" size="small" @click="startRecording">
          {{ t('input.record') }}
        </NButton>
      </SettingRow>

      <SettingRow
        :label="t('input.translationTrigger')"
        :help="t('input.translationTriggerNote')"
        :disabled="!translationEnabled"
      >
        <template v-if="!translationEnabled" #status>
          <span>{{ t('input.translationNeedsLlm') }}</span>
        </template>
        <NSelect
          v-model:value="translationTriggerMode"
          :options="translationTriggerOptions"
          size="small"
          :disabled="!translationEnabled"
        />
      </SettingRow>

      <SettingRow
        :label="t('input.doubleTapWindow')"
        :help="t('input.doubleTapWindowNote')"
        :disabled="!doubleTapActive"
      >
        <NInputNumber
          v-model:value="doubleTapWindowMs"
          class="compact"
          size="small"
          :min="250"
          :max="700"
          :step="10"
          :show-button="false"
          :disabled="!doubleTapActive"
        >
          <template #suffix>ms</template>
        </NInputNumber>
      </SettingRow>
    </SettingsSection>

    <SettingsSection :title="t('input.recording')">
      <SettingRow :label="t('input.inputDevice')">
        <template v-if="noInputDevice" #status>
          <span class="is-warning">{{ t('input.noInputDevice') }}</span>
        </template>
        <NSelect
          v-model:value="selectedDevice"
          :options="deviceOptions"
          :loading="loadingDevices"
          :placeholder="t('input.selectInputDevice')"
          size="small"
        />
        <NTooltip>
          <template #trigger>
            <NButton
              size="small"
              quaternary
              class="icon-button"
              :disabled="loadingDevices"
              :aria-label="t('input.refreshDevices')"
              @click="refreshDevices"
            >
              <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" :class="{ spinning: loadingDevices }">
                <path
                  d="M13.5 8a5.5 5.5 0 1 1-1.61-3.89M13.5 2.5v3h-3"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.5"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                />
              </svg>
            </NButton>
          </template>
          {{ t('input.refreshDevices') }}
        </NTooltip>
      </SettingRow>

      <SettingRow :label="t('input.muteWhileRecording')" :help="t('input.muteWhileRecordingNote')">
        <template
          v-if="(muteOutputWhileRecording && outputMuteStatus && 'device' in outputMuteStatus) || outputMuteHint"
          #status
        >
          <span v-if="muteOutputWhileRecording && outputMuteStatus && 'device' in outputMuteStatus">
            {{ t('input.muteOutputDevice', { device: outputMuteStatus.device }) }}
          </span>
          <span v-if="outputMuteHint" class="is-warning">{{ outputMuteHint }}</span>
        </template>
        <NSwitch v-model:value="muteOutputWhileRecording" />
      </SettingRow>

      <SettingRow :label="t('asr.maxRecordingDuration')" :help="t('asr.maxRecordingDurationNote')">
        <template v-if="maxRecordingDurationLimitHint" #status>
          <span class="is-warning">{{ maxRecordingDurationLimitHint }}</span>
        </template>
        <NSelect
          v-model:value="maxRecordingMinutes"
          :options="maxRecordingOptions"
          class="compact"
          size="small"
        />
      </SettingRow>
    </SettingsSection>

    <SettingsSection :title="t('input.textInjection')">
      <SettingRow :label="t('input.injectionMode')" :help="t('input.injectionModeNote')">
        <NSelect v-model:value="textInjectionMode" :options="injectionOptions" size="small" />
      </SettingRow>

      <SettingRow :label="t('input.postEditSwitch')" :help="t('input.postEditSwitchNote')">
        <template v-if="postEditStore.error || showPostEditDiagnostics" #status>
          <span v-if="postEditStore.error" class="is-warning">{{ postEditStore.error }}</span>
          <template v-if="showPostEditDiagnostics">
            <span v-if="postEditTest?.enabled">
              {{ t(`input.postEditStatus.${postEditTest.status}`) }}
              <template v-if="postEditTest.reason !== 'none'"> · {{ t(`input.postEditReason.${postEditTest.reason}`) }}</template>
              <template v-if="postEditTest.nativeCode !== null"> ({{ postEditTest.nativeCode }})</template>
            </span>
            <span v-if="postEditTest?.insertions !== null && postEditTest?.insertions !== undefined">
              {{ t('input.postEditCounts', { insertions: postEditTest.insertions, deletions: postEditTest.deletions, substitutions: postEditTest.substitutions }) }}
            </span>
            <span v-if="postEditTest?.strategy && postEditTest.rangeEvidence">
              {{ t('input.postEditObservationDetail', {
                strategy: t(`input.postEditStrategies.${postEditTest.strategy}`),
                evidence: t(`input.postEditEvidence.${postEditTest.rangeEvidence}`),
                seconds: ((postEditTest.lastObservedMs ?? 0) / 1000).toFixed(1)
              }) }}
            </span>
            <span v-if="postEditTestError" class="is-warning">{{ t('input.postEditError', { message: postEditTestError }) }}</span>
          </template>
        </template>
        <NSwitch v-model:value="postEditEnabled" :loading="postEditStore.saving" :disabled="!postEditStore.settings" />
      </SettingRow>

      <div class="overrides">
        <div class="overrides-head">
          <span class="overrides-title">{{ t('input.appOverrides') }}</span>
          <HelpTip :text="t('input.appOverridesNote')" />
        </div>

        <template v-if="injectionAppRows.length > 0">
          <div
            v-for="group in overrideGroups"
            :key="group.key"
            class="override-group"
          >
            <div class="override-group-title">{{ group.title }}</div>
            <ul class="override-list">
              <li v-for="app in group.rows" :key="app.key" class="override-item">
                <div class="override-badge">{{ getAppBadgeLabel(app.appName) }}</div>
                <div class="override-copy">
                  <div class="override-name">{{ app.appName }}</div>
                  <div class="override-meta">
                    {{ getAppIdentityLabel(app) }} · {{ getAppUsageLabel(app) }}
                  </div>
                  <div v-if="app.overrideMode === 'pasteboard'" class="override-restore">
                    <NCheckbox
                      size="small"
                      :checked="app.overrideSkipRestore ?? false"
                      @update:checked="(checked: boolean) => updateSkipClipboardRestore(app, checked)"
                    >
                      {{ t('input.skipClipboardRestore') }}
                    </NCheckbox>
                    <HelpTip :text="t('input.skipClipboardRestoreNote')" />
                  </div>
                </div>
                <NSelect
                  :value="app.overrideMode ?? 'default'"
                  :options="getOverrideOptions()"
                  size="small"
                  class="override-select"
                  @update:value="(value) => updateTextInjectionOverride(app, value as TextInjectionOverrideSelectValue)"
                />
              </li>
            </ul>
          </div>
        </template>

        <div v-else class="overrides-empty">
          <div class="overrides-empty-title">
            {{ loadingRecentTargetApps ? t('input.loadingRecentApps') : t('input.appOverridesEmpty') }}
          </div>
          <div>{{ t('input.appOverridesEmptyNote') }}</div>
        </div>
      </div>
    </SettingsSection>

    <SettingsSection :title="t('input.overlay')" :help="t('input.overlaySub')">
      <SettingRow :label="t('input.translucent')" :help="t('input.translucentNote')">
        <NSwitch v-model:value="hudTransparent" />
      </SettingRow>
    </SettingsSection>
  </div>
</template>

<style scoped>
.gesture-legend {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  list-style: none;
}

.overrides {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-md);
  padding: 14px 0 var(--spacing-lg);
  border-top: 1px solid var(--color-divider);
}

.overrides-head {
  display: flex;
  align-items: center;
  gap: 6px;
  font-weight: 500;
}

.override-group {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.override-group-title {
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
}

.override-list {
  list-style: none;
}

.override-item {
  display: flex;
  align-items: center;
  gap: var(--spacing-md);
  padding: 10px 0;
}

.override-item + .override-item {
  border-top: 1px solid var(--color-divider);
}

.override-badge {
  align-self: flex-start;
  margin-top: 2px;
  width: 28px;
  height: 28px;
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--radius-md);
  background: var(--color-accent-light);
  color: var(--color-accent);
  font-size: var(--font-sm);
  font-weight: 600;
}

.override-copy {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.override-name {
  font-weight: 500;
  color: var(--color-text-primary);
}

.override-meta {
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.override-restore {
  display: flex;
  align-items: center;
  gap: 2px;
  margin-top: 4px;
}

.override-select {
  width: 220px;
  flex: 0 0 220px;
}

.overrides-empty {
  padding: 6px 0 4px;
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
}

.overrides-empty-title {
  margin-bottom: 2px;
  font-size: var(--font-md);
  font-weight: 500;
  color: var(--color-text-secondary);
}

@media (max-width: 760px) {
  .override-item {
    flex-wrap: wrap;
  }

  .override-select {
    width: 100%;
    flex: 1 1 100%;
  }
}
</style>
