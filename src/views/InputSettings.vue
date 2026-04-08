<script setup lang="ts">
import { ref, computed, watch, onMounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NSelect, NInputNumber } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'
import { formatHotkey } from '../utils/hotkey'
import {
  exceedsRecordingHardLimit,
  resolveAsrRecordingHardLimitMinutes
} from '../utils/providerOptions'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const textInjectionMode = computed({
  get: () => settingsStore.settings.textInjectionMode,
  set: (v: 'pasteboard' | 'typing') => settingsStore.updateSetting('textInjectionMode', v)
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

onMounted(() => {
  refreshDevices()
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
</style>
