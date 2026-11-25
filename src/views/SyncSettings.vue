<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { NInput, NSwitch, NButton, useMessage } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'
import { useSyncStore } from '../stores/sync'
import SettingsSection from '../components/settings/SettingsSection.vue'
import SettingRow from '../components/settings/SettingRow.vue'

const settingsStore = useSettingsStore()
const syncStore = useSyncStore()
const message = useMessage()
const { t, locale } = useI18n()

const serverUrlInput = ref('')
const pairingCodeInput = ref('')
const recoveryKeyInput = ref('')
const deviceNameInput = ref('')
const initialScopeHistory = ref(true)
const initialScopeEdits = ref(false)
const pairingBusy = ref(false)
const syncBusy = ref(false)
const importBusy = ref(false)
const pairError = ref<string | null>(null)
const generatedRecoveryKeyBanner = ref<string | null>(null)
const revealedRecoveryKey = ref<string | null>(null)

const v2 = computed(() => syncStore.v2Status)
const pendingTotal = computed(() => (v2.value.pendingOpsCount || 0) + (v2.value.pendingStatsCount || 0))

const statusLabel = computed(() => {
  if (!v2.value.configured || v2.value.status === 'disabled') return t('sync.statusOff')
  if (v2.value.status === 'revoked') return t('sync.statusRevoked')
  if (v2.value.status === 'error') return t('sync.statusError')
  if (v2.value.status === 'syncing') return t('sync.statusSyncing')
  if (pendingTotal.value > 0) return t('sync.statusPending', { count: pendingTotal.value })
  return t('sync.statusSynced')
})

const statusClass = computed(() => {
  if (!v2.value.configured || v2.value.status === 'disabled') return 'pill'
  if (v2.value.status === 'revoked' || v2.value.status === 'error') return 'pill error'
  if (v2.value.status === 'syncing' || pendingTotal.value > 0) return 'pill accent'
  return 'pill success'
})

function formatTime(value: string | null) {
  if (!value) return '—'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '—'
  return date.toLocaleString(locale.value, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

async function copyString(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    message.success(t('common.copied'))
  } catch {
    message.error(t('common.copyFailed'))
  }
}

async function handlePair() {
  pairError.value = null
  generatedRecoveryKeyBanner.value = null
  if (!serverUrlInput.value.trim() || !pairingCodeInput.value.trim() || !deviceNameInput.value.trim()) {
    pairError.value = t('sync.statusBlocked')
    return
  }
  pairingBusy.value = true
  try {
    const res = await syncStore.pairV2({
      serverUrl: serverUrlInput.value.trim(),
      pairingCode: pairingCodeInput.value.trim(),
      deviceName: deviceNameInput.value.trim(),
      recoveryKey: recoveryKeyInput.value.trim() || undefined,
      scopeHistory: initialScopeHistory.value,
      scopeEdits: initialScopeHistory.value && initialScopeEdits.value,
    })
    pairingCodeInput.value = ''
    recoveryKeyInput.value = ''
    if (res.generatedNewRecoveryKey) {
      generatedRecoveryKeyBanner.value = res.recoveryKey
    }
    message.success(t('sync.statusSynced'))
  } catch (err) {
    pairError.value = String(err)
  } finally {
    pairingBusy.value = false
  }
}

async function handleScopeHistoryChange(enabled: boolean) {
  try {
    await syncStore.updateV2Scope({
      syncHistory: enabled,
      syncEdits: enabled ? v2.value.scopeEdits : false,
      deviceName: v2.value.deviceName,
    })
  } catch (err) {
    message.error(String(err))
  }
}

async function handleScopeEditsChange(enabled: boolean) {
  try {
    await syncStore.updateV2Scope({
      syncHistory: v2.value.scopeHistory,
      syncEdits: enabled,
      deviceName: v2.value.deviceName,
    })
  } catch (err) {
    message.error(String(err))
  }
}

async function handleImportExistingEdits() {
  importBusy.value = true
  try {
    const count = await syncStore.importV2ExistingEdits()
    message.success(t('sync.importExistingEditsDone', { count }))
  } catch (err) {
    message.error(String(err))
  } finally {
    importBusy.value = false
  }
}

async function handleToggleRecoveryKey() {
  if (revealedRecoveryKey.value) {
    revealedRecoveryKey.value = null
    return
  }
  try {
    revealedRecoveryKey.value = await syncStore.getV2RecoveryKey()
  } catch (err) {
    message.error(String(err))
  }
}

async function handleSyncNow() {
  syncBusy.value = true
  try {
    await syncStore.syncV2Now()
  } catch (err) {
    message.error(String(err))
  } finally {
    syncBusy.value = false
  }
}

async function handleDisconnect() {
  try {
    await syncStore.disconnectV2(false)
    revealedRecoveryKey.value = null
    generatedRecoveryKeyBanner.value = null
  } catch (err) {
    message.error(String(err))
  }
}

onMounted(async () => {
  await syncStore.loadV2Status()
  serverUrlInput.value = v2.value.serverUrl || settingsStore.settings.syncServerUrl || ''
  deviceNameInput.value = v2.value.deviceName || settingsStore.settings.syncDeviceName || ''
})
</script>

<template>
  <div class="page settings-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('sync.title') }}</h1>
    </div>

    <div v-if="generatedRecoveryKeyBanner" class="surface-card recovery-banner">
      <div class="recovery-banner-title">{{ t('sync.newRecoveryKeyWarning') }}</div>
      <div class="recovery-banner-row">
        <code class="monospace recovery-key-code">{{ generatedRecoveryKeyBanner }}</code>
        <NButton size="small" @click="copyString(generatedRecoveryKeyBanner)">
          {{ t('common.copy') }}
        </NButton>
      </div>
    </div>

    <SettingsSection
      v-if="!v2.configured || v2.status === 'disabled' || v2.status === 'revoked'"
      :title="t('sync.v2SectionTitle')"
      :help="t('sync.v2SectionSub')"
    >
      <SettingRow :label="t('sync.serverUrl')" :help="t('sync.serverUrlExample')">
        <NInput v-model:value="serverUrlInput" placeholder="https://sync.example.com" />
      </SettingRow>

      <SettingRow :label="t('sync.pairingCode')" :help="t('sync.pairingCodeNote')">
        <NInput v-model:value="pairingCodeInput" :placeholder="t('sync.pairingCodePlaceholder')" />
      </SettingRow>

      <SettingRow :label="t('sync.deviceName')" :help="t('sync.deviceNameNote')">
        <NInput v-model:value="deviceNameInput" :placeholder="t('sync.deviceNamePlaceholder')" />
      </SettingRow>

      <SettingRow :label="t('sync.recoveryKey')" :help="t('sync.recoveryKeyPairNote')">
        <NInput v-model:value="recoveryKeyInput" :placeholder="t('sync.recoveryKeyPlaceholder')" />
      </SettingRow>

      <SettingRow :label="t('sync.scopeHistoryLabel')" :help="t('sync.scopeHistoryNote')">
        <NSwitch v-model:value="initialScopeHistory" />
      </SettingRow>

      <SettingRow :label="t('sync.scopeEditsLabel')" :help="t('sync.scopeEditsNote')">
        <NSwitch v-model:value="initialScopeEdits" :disabled="!initialScopeHistory" />
      </SettingRow>

      <SettingRow v-if="pairError" :label="t('sync.errorMessage')">
        <div class="field-value error-text">{{ pairError }}</div>
      </SettingRow>

      <SettingRow label="">
        <NButton type="primary" :loading="pairingBusy" @click="handlePair">
          {{ t('sync.pairAndConnect') }}
        </NButton>
      </SettingRow>
    </SettingsSection>

    <template v-else>
      <SettingsSection :title="t('sync.scopeSectionTitle')" :help="t('sync.scopeSectionSub')">
        <SettingRow :label="t('sync.scopeHistoryLabel')" :help="t('sync.scopeHistoryNote')">
          <NSwitch :value="v2.scopeHistory" @update:value="handleScopeHistoryChange" />
        </SettingRow>

        <SettingRow :label="t('sync.scopeEditsLabel')" :help="t('sync.scopeEditsNote')">
          <NSwitch
            :value="v2.scopeEdits"
            :disabled="!v2.scopeHistory"
            @update:value="handleScopeEditsChange"
          />
        </SettingRow>

        <SettingRow
          v-if="v2.scopeHistory && v2.scopeEdits"
          :label="t('sync.importExistingEditsLabel')"
          :help="t('sync.importExistingEditsNote')"
        >
          <NButton size="small" :loading="importBusy" @click="handleImportExistingEdits">
            {{ t('sync.importExistingEditsButton') }}
          </NButton>
        </SettingRow>
      </SettingsSection>

      <SettingsSection :title="t('sync.recoveryKeySection')">
        <SettingRow :label="t('sync.recoveryKey')">
          <div class="recovery-actions">
            <code v-if="revealedRecoveryKey" class="monospace recovery-key-code">
              {{ revealedRecoveryKey }}
            </code>
            <NButton v-if="revealedRecoveryKey" size="small" quaternary @click="copyString(revealedRecoveryKey)">
              {{ t('common.copy') }}
            </NButton>
            <NButton size="small" quaternary @click="handleToggleRecoveryKey">
              {{ revealedRecoveryKey ? t('sync.hideRecoveryKey') : t('sync.showRecoveryKey') }}
            </NButton>
          </div>
        </SettingRow>

        <SettingRow label="">
          <NButton size="small" quaternary type="error" @click="handleDisconnect">
            {{ t('sync.disconnect') }}
          </NButton>
        </SettingRow>
      </SettingsSection>
    </template>

    <SettingsSection :title="t('sync.syncStatus')">
      <template #extra>
        <span :class="statusClass">{{ statusLabel }}</span>
        <NButton
          v-if="v2.configured && v2.status !== 'disabled'"
          size="small"
          quaternary
          :loading="syncBusy"
          @click="handleSyncNow"
        >
          {{ t('sync.retrySync') }}
        </NButton>
      </template>

      <SettingRow v-if="v2.lastError" :label="t('sync.errorMessage')">
        <div class="field-value error-text">{{ v2.lastError }}</div>
      </SettingRow>

      <SettingRow :label="t('sync.lastSync')">
        <div class="field-value">{{ formatTime(v2.lastSyncAt) }}</div>
      </SettingRow>

      <SettingRow v-if="v2.configured" :label="t('sync.pendingQueue')">
        <div class="field-value">
          {{ t('sync.pendingQueueSummary', { ops: v2.pendingOpsCount, stats: v2.pendingStatsCount }) }}
        </div>
      </SettingRow>

      <SettingRow :label="t('sync.deviceId')">
        <div class="field-value monospace">{{ v2.deviceId || syncStore.deviceId || '—' }}</div>
      </SettingRow>

      <SettingRow v-if="v2.devices.length > 0" :label="t('sync.pairedDevices')">
        <div class="device-list">
          <div v-for="d in v2.devices" :key="d.deviceId" class="device-item">
            <span>{{ d.deviceName }}</span>
            <span class="device-meta">{{ formatTime(d.lastSyncedAt) }}</span>
          </div>
        </div>
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

.recovery-banner {
  padding: 14px 16px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  border-color: color-mix(in srgb, var(--color-accent) 45%, var(--color-border));
}

.recovery-banner-title {
  font-size: var(--font-sm);
  font-weight: 500;
  color: var(--color-text-primary);
}

.recovery-banner-row,
.recovery-actions {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
  flex-wrap: wrap;
}

.recovery-key-code {
  word-break: break-all;
  user-select: text;
  color: var(--color-text-primary);
}

.device-list {
  display: flex;
  flex-direction: column;
  gap: 4px;
  align-items: flex-end;
}

.device-item {
  display: flex;
  gap: 8px;
  font-size: var(--font-xs);
  color: var(--color-text-primary);
}

.device-meta {
  color: var(--color-text-tertiary);
}
</style>
