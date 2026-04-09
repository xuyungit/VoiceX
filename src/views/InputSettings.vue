<script setup lang="ts">
import { ref, computed, watch, onMounted, onBeforeUnmount } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { NButton, NSelect, NInputNumber } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore, type AppSettings } from '../stores/settings'
import { formatHotkey } from '../utils/hotkey'
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
}

const settingsStore = useSettingsStore()
const { t } = useI18n()
const recentTargetApps = ref<RecentTargetApp[]>([])
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

interface DeviceOption {
  label: string
  value: string
  isDefault: boolean
}

const devices = ref<DeviceOption[]>([])
const selectedDevice = ref<string | null>(null)
const loadingDevices = ref(false)

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
    rows.set(key, {
      ...app,
      key,
      overrideMode: textInjectionOverrideMap.value.get(key)?.mode ?? null
    })
  }

  for (const overrideItem of settingsStore.settings.textInjectionOverrides) {
    const key = getAppRuleKey(overrideItem)
    const existing = rows.get(key)
    if (existing) {
      existing.overrideMode = overrideItem.mode
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
      overrideMode: overrideItem.mode
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

watch(
  () => settingsStore.settings.hotkeyConfig,
  (config) => {
    displayHotkey.value = formatHotkey(config) ?? t('input.clickToRecord')
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

async function clearHotkey() {
  settingsStore.updateSetting('hotkeyConfig', null)
  displayHotkey.value = t('input.clickToRecord')
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
    const result = await invoke<Array<{ uid: string; name: string; isDefault: boolean }>>('get_input_devices')
    devices.value = result.map((d) => ({
      label: d.isDefault ? `${d.name}（默认）` : d.name,
      value: d.uid,
      isDefault: d.isDefault
    }))

    if (devices.value.length > 0) {
      const preferred = settingsStore.settings.inputDeviceUid
      const fromSettings = devices.value.find((d) => d.value === preferred)
      const fallback = devices.value.find((d) => d.isDefault) ?? devices.value[0]
      selectedDevice.value = (fromSettings ?? fallback).value
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

function handleWindowFocus() {
  void loadRecentTargetApps()
}

function handleVisibilityChange() {
  if (!document.hidden) {
    void loadRecentTargetApps()
  }
}

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
  const nextOverrides = settingsStore.settings.textInjectionOverrides.filter(
    (overrideItem) => getAppRuleKey(overrideItem) !== key
  )

  if (value === 'pasteboard' || value === 'typing') {
    nextOverrides.push({
      platform: app.platform,
      appName: app.appName,
      matchKind: app.matchKind,
      matchValue: app.matchValue,
      mode: value
    })
  }

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
  unlistenRecentTargetApps = await listen('input:recent-target-apps-updated', () => {
    void loadRecentTargetApps()
  })
  window.addEventListener('focus', handleWindowFocus)
  document.addEventListener('visibilitychange', handleVisibilityChange)
  await Promise.all([refreshDevices(), loadRecentTargetApps()])
})

onBeforeUnmount(() => {
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

    <div class="surface-card input-card">
      <div class="card-header">
        <div class="card-title">{{ t('input.hotkeys') }}</div>
        <div class="card-sub">{{ t('input.hotkeysSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('input.recordingHotkey') }}</div>
            <div class="field-note">{{ t('input.recordingHotkeyNote') }}</div>
          </div>
          <div class="field-control">
            <div class="hotkey-display" :class="{ recording: isRecording }">
              {{ isRecording ? recordedKey : hotkeyDisplay }}
            </div>
            <div class="hotkey-actions">
              <NButton :disabled="isRecording" @click="startRecording" size="small">
                {{ t('input.record') }}
              </NButton>
              <NButton
                v-if="settingsStore.settings.hotkeyConfig && !isRecording"
                @click="clearHotkey"
                quaternary
                size="small"
              >
                {{ t('input.clear') }}
              </NButton>
            </div>
          </div>
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('input.translationTrigger') }}</div>
            <div class="field-note">{{ t('input.translationTriggerNote') }}</div>
          </div>
          <NSelect
            v-model:value="translationTriggerMode"
            :options="translationTriggerOptions"
            size="small"
            class="field-control"
            :disabled="!translationEnabled"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('input.doubleTapWindow') }}</div>
            <div class="field-note">{{ t('input.doubleTapWindowNote') }}</div>
          </div>
          <div class="field-control">
            <NInputNumber
              v-model:value="doubleTapWindowMs"
              size="small"
              :min="250"
              :max="700"
              :step="10"
              :show-button="false"
            />
            <span class="field-note">{{ doubleTapWindowMs }}ms</span>
          </div>
        </div>
      </div>
    </div>

    <div class="surface-card input-card">
      <div class="card-header">
        <div class="card-title">{{ t('input.microphone') }}</div>
        <div class="card-sub">{{ t('input.microphoneSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('input.inputDevice') }}</div>
          </div>
          <div class="field-control">
            <NSelect
              v-model:value="selectedDevice"
              :options="devices"
              :loading="loadingDevices"
              :placeholder="t('input.selectInputDevice')"
              class="device-select"
              size="small"
            />
            <NButton @click="refreshDevices" :disabled="loadingDevices" size="small" quaternary>
              {{ t('input.refreshDevices') }}
            </NButton>
          </div>
        </div>
      </div>
    </div>

    <div class="surface-card input-card">
      <div class="card-header">
        <div class="card-title">{{ t('asr.recognition') }}</div>
        <div class="card-sub">{{ t('asr.recognitionSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.maxRecordingDuration') }}</div>
            <div class="field-note">{{ t('asr.maxRecordingDurationNote') }}</div>
            <div v-if="maxRecordingDurationLimitHint" class="field-note limit-hint">
              {{ maxRecordingDurationLimitHint }}
            </div>
          </div>
          <NSelect
            v-model:value="maxRecordingMinutes"
            :options="maxRecordingOptions"
            size="small"
            class="field-control short"
          />
        </div>
      </div>
    </div>

    <div class="surface-card input-card">
      <div class="card-header">
        <div class="card-title">{{ t('input.textInjection') }}</div>
        <div class="card-sub">{{ t('input.textInjectionSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('input.injectionMode') }}</div>
            <div class="field-note">{{ t('input.injectionModeNote') }}</div>
          </div>
          <NSelect
            v-model:value="textInjectionMode"
            :options="injectionOptions"
            size="small"
            class="field-control"
          />
        </div>

        <div class="app-overrides-section">
          <div class="field-text">
            <div class="field-label">{{ t('input.appOverrides') }}</div>
            <div class="field-note">{{ t('input.appOverridesNote') }}</div>
          </div>

          <div v-if="injectionAppRows.length > 0" class="app-overrides-groups">
            <div v-if="configuredInjectionAppRows.length > 0" class="app-overrides-group">
              <div class="app-overrides-group-header">
                <div class="app-overrides-group-title">{{ t('input.configuredApps') }}</div>
                <div class="field-note">{{ t('input.configuredAppsNote') }}</div>
              </div>
              <div class="app-overrides-list">
                <div
                  v-for="app in configuredInjectionAppRows"
                  :key="app.key"
                  class="app-override-item"
                >
                  <div class="app-override-meta">
                    <div class="app-override-badge">{{ getAppBadgeLabel(app.appName) }}</div>
                    <div class="app-override-copy">
                      <div class="app-override-title-row">
                        <div class="app-override-name">{{ app.appName }}</div>
                        <span class="pill app-override-pill">
                          {{ getInjectionModeLabel(app.overrideMode ?? textInjectionMode) }}
                        </span>
                      </div>
                      <div class="field-note app-override-note">
                        <span>{{ getAppIdentityLabel(app) }}</span>
                        <span class="app-override-separator">·</span>
                        <span>{{ getAppUsageLabel(app) }}</span>
                      </div>
                    </div>
                  </div>

                  <NSelect
                    :value="app.overrideMode ?? 'default'"
                    :options="getOverrideOptions()"
                    size="small"
                    class="app-override-select"
                    @update:value="(value) => updateTextInjectionOverride(app, value as TextInjectionOverrideSelectValue)"
                  />
                </div>
              </div>
            </div>

            <div v-if="recentUnconfiguredAppRows.length > 0" class="app-overrides-group">
              <div class="app-overrides-group-header">
                <div class="app-overrides-group-title">{{ t('input.recentApps') }}</div>
                <div class="field-note">{{ t('input.recentAppsNote') }}</div>
              </div>
              <div class="app-overrides-list">
                <div
                  v-for="app in recentUnconfiguredAppRows"
                  :key="app.key"
                  class="app-override-item"
                >
                  <div class="app-override-meta">
                    <div class="app-override-badge">{{ getAppBadgeLabel(app.appName) }}</div>
                    <div class="app-override-copy">
                      <div class="app-override-title-row">
                        <div class="app-override-name">{{ app.appName }}</div>
                        <span class="pill app-override-pill">
                          {{ getInjectionModeLabel(textInjectionMode) }}
                        </span>
                      </div>
                      <div class="field-note app-override-note">
                        <span>{{ getAppIdentityLabel(app) }}</span>
                        <span class="app-override-separator">·</span>
                        <span>{{ getAppUsageLabel(app) }}</span>
                      </div>
                    </div>
                  </div>

                  <NSelect
                    :value="'default'"
                    :options="getOverrideOptions()"
                    size="small"
                    class="app-override-select"
                    @update:value="(value) => updateTextInjectionOverride(app, value as TextInjectionOverrideSelectValue)"
                  />
                </div>
              </div>
            </div>
          </div>

          <div v-else class="app-overrides-empty">
            <div class="app-overrides-empty-title">
              {{ loadingRecentTargetApps ? t('input.loadingRecentApps') : t('input.appOverridesEmpty') }}
            </div>
            <div class="field-note">{{ t('input.appOverridesEmptyNote') }}</div>
          </div>
        </div>
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

.input-card {
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

.field-note.limit-hint {
  color: color-mix(in srgb, var(--color-warning) 72%, var(--color-text-secondary));
}

.field-control {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
  width: 420px;
  max-width: 100%;
  justify-content: flex-end;
}

.field-control.short {
  width: 200px;
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

.device-select {
  flex: 1;
}

.app-overrides-section {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-sm);
  padding-top: 2px;
}

.app-overrides-groups {
  display: flex;
  flex-direction: column;
  gap: 18px;
}

.app-overrides-group {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.app-overrides-group-header {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding-left: 2px;
}

.app-overrides-group-title {
  font-size: var(--font-sm);
  font-weight: 600;
  color: var(--color-text-secondary);
}

.app-overrides-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.app-override-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-lg);
  padding: 14px 16px;
  border-radius: var(--radius-lg);
  border: 1px solid color-mix(in srgb, var(--color-border) 82%, var(--color-accent) 18%);
  background:
    linear-gradient(180deg, color-mix(in srgb, var(--color-bg-tertiary) 82%, transparent), transparent),
    var(--color-bg-primary);
}

.app-override-meta {
  display: flex;
  align-items: flex-start;
  gap: 12px;
  flex: 1;
  min-width: 0;
}

.app-override-badge {
  width: 36px;
  height: 36px;
  border-radius: 12px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex: 0 0 auto;
  font-size: var(--font-sm);
  font-weight: 700;
  color: var(--color-accent);
  background: color-mix(in srgb, var(--color-accent-light) 72%, var(--color-bg-tertiary));
  border: 1px solid color-mix(in srgb, var(--color-accent) 20%, var(--color-border));
}

.app-override-copy {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.app-override-title-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.app-override-name {
  font-weight: 600;
  color: var(--color-text-primary);
}

.app-override-pill {
  white-space: nowrap;
}

.app-override-note {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.app-override-separator {
  color: var(--color-text-tertiary);
}

.app-override-select {
  width: 240px;
  flex: 0 0 240px;
}

.app-overrides-empty {
  padding: 18px;
  border-radius: var(--radius-lg);
  border: 1px dashed var(--color-border);
  background: color-mix(in srgb, var(--color-bg-tertiary) 70%, transparent);
}

.app-overrides-empty-title {
  font-weight: 600;
  color: var(--color-text-primary);
  margin-bottom: 4px;
}

@media (max-width: 900px) {
  .app-override-item {
    flex-direction: column;
    align-items: flex-start;
  }

  .app-override-select {
    width: 100%;
    flex: none;
  }
}
</style>
