<script setup lang="ts">
import { computed, onMounted } from 'vue'
import { NInput, NSwitch, NButton } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'
import { useSyncStore } from '../stores/sync'
import SettingsSection from '../components/settings/SettingsSection.vue'
import SettingRow from '../components/settings/SettingRow.vue'

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

    <SettingsSection :title="t('sync.historySync')" :help="t('sync.historySyncSub')">
      <SettingRow :label="t('sync.enableSync')" :help="t('sync.enableSyncNote')">
        <NSwitch v-model:value="syncEnabled" />
      </SettingRow>

      <SettingRow :label="t('sync.serverUrl')" :help="t('sync.serverUrlExample')">
        <NInput v-model:value="syncServerUrl" placeholder="http://localhost:8787" />
      </SettingRow>

      <SettingRow :label="t('sync.syncToken')" :help="t('sync.syncTokenNote')">
        <NInput v-model:value="syncToken" type="text" class="monospace" :placeholder="t('sync.enterToken')" />
      </SettingRow>

      <SettingRow :label="t('sync.sharedSecret')" :help="t('sync.sharedSecretNote')">
        <NInput v-model:value="syncSharedSecret" type="text" class="monospace" :placeholder="t('sync.enterSharedSecret')" />
      </SettingRow>

      <SettingRow :label="t('sync.deviceName')" :help="t('sync.deviceNameNote')">
        <NInput v-model:value="syncDeviceName" :placeholder="t('sync.deviceNamePlaceholder')" />
      </SettingRow>
    </SettingsSection>

    <SettingsSection :title="t('sync.syncStatus')">
      <template #extra>
        <span :class="statusClass">{{ statusLabel }}</span>
        <NButton size="small" quaternary @click="syncStore.syncNow">{{ t('sync.syncNow') }}</NButton>
      </template>

      <SettingRow v-if="syncStore.state.lastError" :label="t('sync.errorMessage')">
        <div class="field-value error-text">{{ syncStore.state.lastError }}</div>
      </SettingRow>

      <SettingRow :label="t('sync.lastSync')">
        <div class="field-value">{{ formatTime(syncStore.state.lastSyncAt) }}</div>
      </SettingRow>

      <SettingRow :label="t('sync.currentSeq')">
        <div class="field-value">{{ syncStore.state.lastSeq }}</div>
      </SettingRow>

      <SettingRow :label="t('sync.deviceId')">
        <div class="field-value monospace">{{ syncStore.deviceId || '—' }}</div>
      </SettingRow>
    </SettingsSection>
  </div>
</template>

<style scoped>
.field-value {
  color: var(--color-text-secondary);
  font-size: var(--font-sm);
  text-align: right;
  min-width: 0;
  user-select: text;
}

.error-text {
  color: color-mix(in srgb, var(--color-error) 75%, var(--color-text-secondary));
  word-break: break-all;
}

.monospace {
  font-family: var(--font-mono);
  font-size: var(--font-xs);
}
</style>
