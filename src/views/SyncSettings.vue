<script setup lang="ts">
import { computed, onMounted } from 'vue'
import { NInput, NSwitch, NButton } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'
import { useSyncStore } from '../stores/sync'

const settingsStore = useSettingsStore()
const syncStore = useSyncStore()
const { t, locale } = useI18n()

const syncEnabled = computed({
  get: () => settingsStore.settings.syncEnabled,
  set: (v: boolean) => settingsStore.updateSetting('syncEnabled', v)
})

const syncServerUrl = computed({
  get: () => settingsStore.settings.syncServerUrl,
  set: (v: string) => settingsStore.updateSetting('syncServerUrl', v)
})

const syncToken = computed({
  get: () => settingsStore.settings.syncToken,
  set: (v: string) => settingsStore.updateSetting('syncToken', v)
})

const syncSharedSecret = computed({
  get: () => settingsStore.settings.syncSharedSecret,
  set: (v: string) => settingsStore.updateSetting('syncSharedSecret', v)
})

const syncDeviceName = computed({
  get: () => settingsStore.settings.syncDeviceName,
  set: (v: string) => settingsStore.updateSetting('syncDeviceName', v)
})

const statusLabel = computed(() => {
  if (!syncEnabled.value) return t('sync.statusOff')
  switch (syncStore.state.status) {
    case 'live':
      return t('sync.statusLive')
    case 'connecting':
      return t('sync.statusConnecting')
    case 'reconnecting':
      return t('sync.statusReconnecting')
    case 'blocked':
      return t('sync.statusBlocked')
    default:
      return t('sync.statusPreparing')
  }
})

const statusClass = computed(() => {
  if (!syncEnabled.value) return 'pill'
  switch (syncStore.state.status) {
    case 'live':
      return 'pill success'
    case 'connecting':
      return 'pill accent'
    case 'reconnecting':
      return 'pill accent'
    case 'blocked':
      return 'pill'
    default:
      return 'pill'
  }
})

function formatTime(value: string | null) {
  if (!value) return '—'
  const date = new Date(value)
  return date.toLocaleString(locale.value, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit'
  })
}

onMounted(() => {
  syncStore.loadState()
})
</script>

<template>
  <div class="page settings-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('sync.title') }}</h1>
    </div>

    <div class="surface-card sync-card">
      <div class="card-header">
        <div class="card-title">{{ t('sync.historySync') }}</div>
        <div class="card-sub">{{ t('sync.historySyncSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('sync.enableSync') }}</div>
            <div class="field-note">{{ t('sync.enableSyncNote') }}</div>
          </div>
          <NSwitch v-model:value="syncEnabled" />
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('sync.serverUrl') }}</div>
            <div class="field-note">{{ t('sync.serverUrlExample') }}</div>
          </div>
          <NInput v-model:value="syncServerUrl" class="field-control" placeholder="http://localhost:8787" />
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('sync.syncToken') }}</div>
            <div class="field-note">{{ t('sync.syncTokenNote') }}</div>
          </div>
          <NInput v-model:value="syncToken" type="text" class="field-control monospace" :placeholder="t('sync.enterToken')" />
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('sync.sharedSecret') }}</div>
            <div class="field-note">{{ t('sync.sharedSecretNote') }}</div>
          </div>
          <NInput v-model:value="syncSharedSecret" type="text" class="field-control monospace" :placeholder="t('sync.enterSharedSecret')" />
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('sync.deviceName') }}</div>
            <div class="field-note">{{ t('sync.deviceNameNote') }}</div>
          </div>
          <NInput v-model:value="syncDeviceName" class="field-control" :placeholder="t('sync.deviceNamePlaceholder')" />
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('sync.syncStatus') }}</div>
          </div>
          <div :class="statusClass">{{ statusLabel }}</div>
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('sync.lastSync') }}</div>
          </div>
          <div class="field-value">{{ formatTime(syncStore.state.lastSyncAt) }}</div>
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('sync.currentSeq') }}</div>
          </div>
          <div class="field-value">{{ syncStore.state.lastSeq }}</div>
        </div>

        <div v-if="syncStore.state.lastError" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('sync.errorMessage') }}</div>
          </div>
          <div class="field-value error-text">{{ syncStore.state.lastError }}</div>
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('sync.deviceId') }}</div>
          </div>
          <div class="field-value monospace">{{ syncStore.deviceId || '—' }}</div>
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('sync.manualSync') }}</div>
          </div>
          <NButton size="small" quaternary @click="syncStore.syncNow">{{ t('sync.syncNow') }}</NButton>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.field-value {
  color: var(--color-text-secondary);
  font-size: var(--font-sm);
}

.card-header {
  display: flex;
  flex-direction: column;
  gap: 6px;
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
  width: 420px;
  max-width: 100%;
}

.error-text {
  color: var(--color-error);
  max-width: 420px;
  word-break: break-all;
}

.monospace {
  font-family: 'SFMono-Regular', 'SF Mono', 'Menlo', 'Consolas', monospace;
  font-size: var(--font-xs);
}
</style>
