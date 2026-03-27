<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { NInput, NButton, NDataTable, NTag, NSpace, NCard, NText, NDivider, useMessage } from 'naive-ui'
import { invoke } from '@tauri-apps/api/core'
import { useSettingsStore } from '../stores/settings'

const settingsStore = useSettingsStore()
const message = useMessage()
const isSyncing = ref(false)
const isLoadingTables = ref(false)
const remoteTables = ref<any[]>([])

const volcAccessKey = computed({
  get: () => settingsStore.settings.volcAccessKey,
  set: (v) => settingsStore.updateSetting('volcAccessKey', v)
})

const volcSecretKey = computed({
  get: () => settingsStore.settings.volcSecretKey,
  set: (v) => settingsStore.updateSetting('volcSecretKey', v)
})

const volcAppId = computed({
  get: () => settingsStore.settings.volcAppId,
  set: (v) => settingsStore.updateSetting('volcAppId', v)
})

const localUpdatedAt = computed(() => settingsStore.settings.localHotwordUpdatedAt || 'Never')
const remoteUpdatedAt = computed(() => settingsStore.settings.remoteHotwordUpdatedAt || 'Never')
const onlineHotwordId = computed(() => settingsStore.settings.onlineHotwordId || 'Not linked')
const diagnosticsEnabled = computed(() => settingsStore.settings.enableDiagnostics)
const lastSyncResult = computed(() => settingsStore.lastHotwordSyncResult)
const canCopyDiagnostics = computed(() => Boolean(lastSyncResult.value))
const diagnosticsText = computed(() => {
  if (!lastSyncResult.value) {
    return 'No sync has been run yet.'
  }
  return JSON.stringify(lastSyncResult.value, null, 2)
})

async function handleForceDownload() {
  if (isSyncing.value) return
  isSyncing.value = true
  try {
    const result = await settingsStore.forceDownloadHotwords()
    if (result?.message) {
      if (result.status === 'error') {
        message.error(result.message)
      } else {
        message.success(result.message)
      }
    }
    await loadRemoteTables()
  } catch (err: any) {
    message.error(`Force download failed: ${err}`)
  } finally {
    isSyncing.value = false
  }
}

async function copyDiagnostics() {
  try {
    await navigator.clipboard.writeText(diagnosticsText.value)
    message.success('Diagnostics copied.')
  } catch (err) {
    console.error('Failed to copy diagnostics:', err)
    message.error('Failed to copy diagnostics.')
  }
}

async function handleSync() {
  if (isSyncing.value) return
  isSyncing.value = true
  try {
    const result = await settingsStore.syncHotwords({ reason: 'manual' })
    if (result?.message) {
      if (result.status === 'error') {
        message.error(result.message)
      } else {
        message.success(result.message)
      }
    }
    await loadRemoteTables()
  } catch (err: any) {
    message.error(`Sync failed: ${err}`)
  } finally {
    isSyncing.value = false
  }
}

async function loadRemoteTables() {
  if (volcAccessKey.value && volcSecretKey.value) {
    isLoadingTables.value = true
    try {
      remoteTables.value = await invoke<any[]>('list_online_vocabularies')
    } catch (err) {
      console.error('Failed to load remote tables', err)
    } finally {
      isLoadingTables.value = false
    }
  }
}

const columns = [
  { title: 'Name', key: 'BoostingTableName' },
  { title: 'ID', key: 'BoostingTableID' },
  { title: 'Words', key: 'WordCount' },
  { title: 'Updated', key: 'UpdateTime' },
  {
    title: 'Status',
    key: 'status',
    render(row: any) {
      return row.BoostingTableID === settingsStore.settings.onlineHotwordId
        ? h(NTag, { type: 'success', size: 'small' }, { default: () => 'Linked' })
        : null
    }
  }
]

onMounted(async () => {
  await loadRemoteTables()
  // Automatically trigger sync when entering the hotwords page
  if (volcAccessKey.value && volcSecretKey.value && volcAppId.value) {
    handleSync()
  }
})

import { h } from 'vue'
</script>

<template>
  <div class="page hotwords-page">
    <div class="page-header">
      <h1 class="page-title">Hotwords Sync</h1>
      <p class="page-subtitle text-tertiary">同步本地词库与火山引擎在线热词表</p>
    </div>

    <div class="surface-card">
      <div class="section-header">
        <div class="section-title">Volcengine Online Management</div>
        <div class="section-hint">用于同步热词表的 AK/SK（若与 ASR 不同）。</div>
      </div>

      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Access Key</div>
          </div>
          <NInput v-model:value="volcAccessKey" type="password" show-password-on="click" placeholder="Enter Access Key" class="field-control" />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Secret Key</div>
          </div>
          <NInput v-model:value="volcSecretKey" type="password" show-password-on="click" placeholder="Enter Secret Key" class="field-control" />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">App ID</div>
          </div>
          <NInput v-model:value="volcAppId" placeholder="Enter App ID" class="field-control" />
        </div>
      </div>
    </div>

    <div class="sync-status-container">
      <NCard title="Synchronization Status" size="small">
        <template #header-extra>
          <NButton type="primary" :loading="isSyncing" @click="handleSync">
            {{ isSyncing ? 'Syncing...' : 'Sync Now' }}
          </NButton>
        </template>
        
        <NSpace vertical>
          <div class="status-item">
            <NText depth="3">Local Modification:</NText>
            <NText style="margin-left: 8px">{{ localUpdatedAt }}</NText>
          </div>
          <div class="status-item">
            <NText depth="3">Remote Last Synced:</NText>
            <NText style="margin-left: 8px">{{ remoteUpdatedAt }}</NText>
          </div>
          <div class="status-item">
            <NText depth="3">Linked Table ID:</NText>
            <NText code style="margin-left: 8px">{{ onlineHotwordId }}</NText>
          </div>
        </NSpace>
      </NCard>
    </div>

    <div v-if="diagnosticsEnabled" class="surface-card diagnostics-card">
      <div class="section-header">
        <div>
          <div class="section-title">Diagnostics</div>
          <div class="section-hint">最近一次热词同步的诊断信息，仅用于调试。</div>
        </div>
        <div class="actions">
          <NButton size="small" quaternary :disabled="!canCopyDiagnostics" @click="copyDiagnostics">
            Copy Report
          </NButton>
          <NButton size="small" quaternary :disabled="isSyncing" @click="handleForceDownload">
            Force Download
          </NButton>
        </div>
      </div>
      <pre class="diagnostics-block">{{ diagnosticsText }}</pre>
    </div>

    <div class="tables-section" style="margin-top: 24px">
      <div class="section-header">
        <div class="section-title">Remote Vocabularies</div>
        <div class="actions">
           <NButton size="small" quaternary @click="loadRemoteTables" :loading="isLoadingTables">Refresh List</NButton>
        </div>
      </div>
      
      <NDataTable
        :columns="columns"
        :data="remoteTables"
        :loading="isLoadingTables"
        size="small"
        placeholder="Configure AK/SK to list remote tables"
      />
    </div>

    <NDivider />
    
    <div class="sync-logic-note text-tertiary" style="font-size: 12px">
      <p><strong>Sync Logic:</strong></p>
      <ul>
        <li>如果服务器的时间戳更新，则从服务器下载覆盖本地。</li>
        <li>如果本地有修改且晚于最近一次同步时间，则上传至服务器。</li>
        <li>如果服务器端还没有创建过 <code>voicex_hotwords</code> 词表，则会自动创建。</li>
      </ul>
    </div>
  </div>
</template>

<style scoped>
.hotwords-page {
  max-width: 1000px;
}

.sync-status-container {
  margin-top: 24px;
}

.diagnostics-card {
  margin-top: 24px;
}

.diagnostics-block {
  background: var(--color-bg-primary);
  border: 1px solid var(--color-border);
  border-radius: var(--radius-md);
  padding: var(--spacing-md);
  color: var(--color-text-secondary);
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace;
  font-size: var(--font-xs);
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 280px;
  overflow: auto;
}

.actions {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
}

.status-item {
  display: flex;
  align-items: center;
}

.field-list {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-md);
  margin-top: 16px;
}

.field-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-lg);
}

.field-label {
  font-weight: 600;
  color: var(--color-text-primary);
}

.field-control {
  width: 420px;
  max-width: 100%;
}
</style>
