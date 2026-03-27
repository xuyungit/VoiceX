<script setup lang="ts">
import { ref, computed, watch, onMounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NSelect, NInputNumber } from 'naive-ui'
import { useSettingsStore } from '../stores/settings'
import { formatHotkey } from '../utils/hotkey'

const settingsStore = useSettingsStore()

const textInjectionMode = computed({
  get: () => settingsStore.settings.textInjectionMode,
  set: (v: 'pasteboard' | 'typing') => settingsStore.updateSetting('textInjectionMode', v)
})

const injectionOptions = [
  { label: 'Clipboard paste', value: 'pasteboard' },
  { label: 'Simulated typing', value: 'typing' }
]

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

const translationTriggerOptions = [
  { label: 'Double tap hotkey', value: 'double_tap' },
  { label: 'Off', value: 'off' }
]

const isRecording = ref(false)
const recordedKey = ref('')
const displayHotkey = ref('点击录制')

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
    displayHotkey.value = formatHotkey(config) ?? '点击录制'
  },
  { immediate: true }
)

async function startRecording() {
  isRecording.value = true
  recordedKey.value = '按下热键...'
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
  displayHotkey.value = '点击录制'
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
      <h1 class="page-title">Input</h1>
    </div>

    <div class="surface-card input-card">
      <div class="card-header">
        <div class="card-title">Hotkeys</div>
        <div class="card-sub">单个热键支持单击、双击与长按</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Recording hotkey</div>
            <div class="field-note">One key for both push-to-talk and hands-free.</div>
          </div>
          <div class="field-control">
            <div class="hotkey-display" :class="{ recording: isRecording }">
              {{ isRecording ? recordedKey : hotkeyDisplay }}
            </div>
            <div class="hotkey-actions">
              <NButton :disabled="isRecording" @click="startRecording" size="small">
                录制
              </NButton>
              <NButton
                v-if="settingsStore.settings.hotkeyConfig && !isRecording"
                @click="clearHotkey"
                quaternary
                size="small"
              >
                清除
              </NButton>
            </div>
          </div>
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Translation trigger</div>
            <div class="field-note">Single tap: Assistant hands-free, double tap: Translate to English, long press: push-to-talk.</div>
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
            <div class="field-label">Double-tap window</div>
            <div class="field-note">Adjust if double tap feels too strict or too sensitive.</div>
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
        <div class="card-title">Microphone</div>
        <div class="card-sub">选择录音输入设备</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Input device</div>
          </div>
          <div class="field-control">
            <NSelect
              v-model:value="selectedDevice"
              :options="devices"
              :loading="loadingDevices"
              placeholder="选择输入设备"
              class="device-select"
              size="small"
            />
            <NButton @click="refreshDevices" :disabled="loadingDevices" size="small" quaternary>
              刷新设备
            </NButton>
          </div>
        </div>
      </div>
    </div>

    <div class="surface-card input-card">
      <div class="card-header">
        <div class="card-title">Text Injection</div>
        <div class="card-sub">选择文字注入方式</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Injection Mode</div>
            <div class="field-note">
              Clipboard paste is fastest but temporarily overwrites your clipboard. Simulated typing avoids touching the clipboard.
            </div>
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

.field-control {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
  width: 420px;
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

.device-select {
  flex: 1;
}
</style>
