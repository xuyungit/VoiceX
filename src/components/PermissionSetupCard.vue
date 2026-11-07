<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { NButton, useMessage } from 'naive-ui'
import { usePermissionsStore, type PermissionKind } from '../stores/permissions'

// Shown at the top of Overview while VoiceX lacks an OS permission it needs.
// It has no close button on purpose: it goes away when the cause is fixed.

const { t } = useI18n()
const message = useMessage()
const store = usePermissionsStore()
const busy = ref<PermissionKind | 'restart' | null>(null)

const ICONS: Record<PermissionKind, string> = {
  microphone: 'M12 14c1.66 0 3-1.34 3-3V5c0-1.66-1.34-3-3-3S9 3.34 9 5v6c0 1.66 1.34 3 3 3zm5.91-3c-.49 0-.9.36-.98.85C16.52 14.2 14.47 16 12 16s-4.52-1.8-4.93-4.15a.998.998 0 0 0-.98-.85c-.61 0-1.09.54-1 1.14.49 3 2.89 5.35 5.91 5.78V20c0 .55.45 1 1 1s1-.45 1-1v-2.08a6.993 6.993 0 0 0 5.91-5.78c.1-.6-.39-1.14-1-1.14z',
  accessibility: 'M12 2c1.1 0 2 .9 2 2s-.9 2-2 2-2-.9-2-2 .9-2 2-2zm9 7h-6v13h-2v-6h-2v6H9V9H3V7h18v2z',
  inputMonitoring: 'M20 5H4c-1.1 0-1.99.9-1.99 2L2 17c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm-9 3h2v2h-2V8zm0 3h2v2h-2v-2zM8 8h2v2H8V8zm0 3h2v2H8v-2zm-1 2H5v-2h2v2zm0-3H5V8h2v2zm9 7H8v-2h8v2zm0-4h-2v-2h2v2zm0-3h-2V8h2v2zm3 3h-2v-2h2v2zm0-3h-2V8h2v2z',
}
const RESTART_ICON = 'M17.65 6.35A7.958 7.958 0 0 0 12 4c-4.42 0-7.99 3.58-7.99 8s3.57 8 7.99 8c3.73 0 6.84-2.55 7.73-6h-2.08A5.99 5.99 0 0 1 12 18c-3.31 0-6-2.69-6-6s2.69-6 6-6c1.66 0 3.14.69 4.22 1.78L13 11h7V4l-2.35 2.35z'

const isWindows = computed(() => store.snapshot?.platform === 'windows')

const rows = computed(() => store.missing.map((kind) => {
  const state = store.snapshot?.[kind]
  const description = kind === 'microphone' && isWindows.value
    ? t('permissions.microphoneDescWindows')
    : t(`permissions.${kind}Desc`)
  let action: string
  if (kind === 'microphone') {
    action = state === 'notDetermined' && !isWindows.value ? t('permissions.allow') : t('permissions.openSettings')
  } else {
    action = t('permissions.grant')
  }
  return {
    kind,
    icon: ICONS[kind],
    title: t(`permissions.${kind}`),
    state: state === 'notDetermined' ? t('permissions.stateNotDetermined') : t('permissions.stateDenied'),
    description,
    action,
  }
}))

const subtitle = computed(() => isWindows.value ? t('permissions.subtitleWindows') : t('permissions.subtitle'))

async function run(target: PermissionKind | 'restart', action: () => Promise<void>) {
  busy.value = target
  try {
    await action()
  } catch (err) {
    message.error(t('permissions.actionFailed', { error: String(err) }))
  } finally {
    busy.value = null
  }
}
</script>

<template>
  <section v-if="store.needsAttention" class="surface-card permission-card" role="region" :aria-label="t('permissions.title')">
    <header class="card-header">
      <svg class="warning-icon" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
        <path d="M1 21h22L12 2 1 21zm12-3h-2v-2h2v2zm0-4h-2v-4h2v4z" />
      </svg>
      <div>
        <h2 class="card-title">{{ store.missing.length ? t('permissions.title') : t('permissions.restartTitle') }}</h2>
        <p v-if="store.missing.length" class="card-subtitle">{{ subtitle }}</p>
      </div>
    </header>

    <ul class="rows">
      <li v-for="row in rows" :key="row.kind" class="row">
        <svg class="row-icon" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path :d="row.icon" /></svg>
        <div class="row-text">
          <div class="row-title">
            {{ row.title }}
            <span class="row-state">{{ row.state }}</span>
          </div>
          <div class="row-desc">{{ row.description }}</div>
        </div>
        <NButton
          class="row-action"
          type="primary"
          size="small"
          :loading="busy === row.kind"
          :disabled="busy !== null && busy !== row.kind"
          @click="run(row.kind, () => store.grant(row.kind))"
        >
          {{ row.action }}
        </NButton>
      </li>

      <li v-if="store.needsRestart" class="row">
        <svg class="row-icon" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path :d="RESTART_ICON" /></svg>
        <div class="row-text">
          <div class="row-desc">{{ t('permissions.restartDesc') }}</div>
        </div>
        <NButton class="row-action" type="primary" size="small" :loading="busy === 'restart'" @click="run('restart', store.restartApp)">
          {{ t('permissions.restart') }}
        </NButton>
      </li>
    </ul>
  </section>
</template>

<style scoped>
.permission-card {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 14px 16px;
  border-left: 3px solid var(--color-warning);
}

.card-header {
  display: flex;
  align-items: flex-start;
  gap: 10px;
}

.warning-icon {
  flex: none;
  width: 18px;
  height: 18px;
  margin-top: 1px;
  color: var(--color-warning);
}

.card-title {
  font-size: var(--font-lg);
  font-weight: 600;
  line-height: 20px;
}

.card-subtitle {
  margin-top: 2px;
  font-size: var(--font-sm);
  color: var(--color-text-secondary);
}

.rows {
  display: flex;
  flex-direction: column;
  list-style: none;
  margin: 0;
  padding: 0;
}

.row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 8px 0 8px 28px;
  border-top: 1px solid var(--color-divider);
}

.row-icon {
  flex: none;
  width: 18px;
  height: 18px;
  color: var(--color-text-secondary);
}

.row-text {
  flex: 1;
  min-width: 0;
}

.row-title {
  font-size: var(--font-md);
  font-weight: 600;
}

.row-state {
  margin-left: 6px;
  font-size: var(--font-xs);
  font-weight: 500;
  color: var(--color-warning);
}

.row-action {
  flex: none;
  min-width: 72px;
}

.row-desc {
  font-size: var(--font-sm);
  color: var(--color-text-secondary);
}
</style>
