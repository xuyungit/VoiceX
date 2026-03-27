<script setup lang="ts">
import { computed, onMounted } from 'vue'
import { NInput, NSwitch, NButton } from 'naive-ui'
import { useSettingsStore } from '../stores/settings'
import { useSyncStore } from '../stores/sync'

const settingsStore = useSettingsStore()
const syncStore = useSyncStore()

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
  if (!syncEnabled.value) return '已关闭'
  switch (syncStore.state.status) {
    case 'live':
      return '在线'
    case 'connecting':
      return '连接中'
    case 'reconnecting':
      return '重连中'
    case 'blocked':
      return '配置缺失'
    default:
      return '准备中'
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
  return date.toLocaleString('zh-CN', {
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
      <h1 class="page-title">Sync</h1>
    </div>

    <div class="surface-card sync-card">
      <div class="card-header">
        <div class="card-title">History Sync</div>
        <div class="card-sub">在多端同步文本历史与统计数据（不包含录音文件）</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">启用同步</div>
            <div class="field-note">需要配置服务器地址、Token、共享密钥与设备名</div>
          </div>
          <NSwitch v-model:value="syncEnabled" />
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">服务器地址</div>
            <div class="field-note">例如 http://127.0.0.1:8787</div>
          </div>
          <NInput v-model:value="syncServerUrl" class="field-control" placeholder="http://localhost:8787" />
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">同步 Token</div>
            <div class="field-note">同一账号共享同一个 Token</div>
          </div>
          <NInput v-model:value="syncToken" type="text" class="field-control monospace" placeholder="输入 Token" />
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">共享密钥</div>
            <div class="field-note">需与服务端配置一致，用于验证客户端请求</div>
          </div>
          <NInput v-model:value="syncSharedSecret" type="text" class="field-control monospace" placeholder="输入共享密钥" />
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">设备名称</div>
            <div class="field-note">将显示在历史记录中</div>
          </div>
          <NInput v-model:value="syncDeviceName" class="field-control" placeholder="例如：MacBook Pro" />
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">同步状态</div>
          </div>
          <div :class="statusClass">{{ statusLabel }}</div>
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">最近同步</div>
          </div>
          <div class="field-value">{{ formatTime(syncStore.state.lastSyncAt) }}</div>
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">当前序号</div>
          </div>
          <div class="field-value">{{ syncStore.state.lastSeq }}</div>
        </div>

        <div v-if="syncStore.state.lastError" class="field-row">
          <div class="field-text">
            <div class="field-label">错误信息</div>
          </div>
          <div class="field-value error-text">{{ syncStore.state.lastError }}</div>
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">设备 ID</div>
          </div>
          <div class="field-value monospace">{{ syncStore.deviceId || '—' }}</div>
        </div>

        <div class="field-row">
          <div class="field-text">
            <div class="field-label">手动同步</div>
          </div>
          <NButton size="small" quaternary @click="syncStore.syncNow">立即同步</NButton>
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
