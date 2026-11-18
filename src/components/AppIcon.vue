<script setup lang="ts">
import { computed } from 'vue'
import { getAppFallbackLabel, useAppIconStore } from '../stores/appIcons'

const props = withDefaults(
  defineProps<{
    appName: string
    platform?: string | null
    appKey?: string | null
    bundleId?: string | null
    executablePath?: string | null
    appUserModelId?: string | null
    size?: 'xs' | 'sm' | 'md'
    fallback?: 'badge' | 'window-svg' | 'none'
  }>(),
  {
    platform: null,
    appKey: null,
    bundleId: null,
    executablePath: null,
    appUserModelId: null,
    size: 'md',
    fallback: 'badge',
  },
)

const appIconStore = useAppIconStore()

const query = computed(() => ({
  platform: props.platform,
  appKey: props.appKey,
  bundleId: props.bundleId,
  executablePath: props.executablePath,
  appUserModelId: props.appUserModelId,
  appName: props.appName,
}))

const iconUrl = computed(() => appIconStore.getIconUrl(query.value))
const badgeLabel = computed(() => getAppFallbackLabel(props.appName, 'initials'))

function onImageError() {
  appIconStore.markFailed(query.value)
}
</script>

<template>
  <span
    v-if="iconUrl || fallback !== 'none'"
    class="voicex-app-icon"
    :class="[
      `size-${size}`,
      { 'has-image': Boolean(iconUrl), 'is-badge': !iconUrl && fallback === 'badge' },
    ]"
    aria-hidden="true"
  >
    <img
      v-if="iconUrl"
      :src="iconUrl"
      :alt="appName"
      class="app-icon-img"
      draggable="false"
      @error="onImageError"
    />
    <svg
      v-else-if="fallback === 'window-svg'"
      viewBox="0 0 24 24"
      fill="currentColor"
      class="app-icon-svg"
    >
      <path
        d="M5 4h14a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zm0 4v10h14V8H5z"
      />
    </svg>
    <span v-else class="app-icon-initials">{{ badgeLabel }}</span>
  </span>
</template>

<style scoped>
.voicex-app-icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  user-select: none;
  overflow: hidden;
  vertical-align: middle;
}

.voicex-app-icon.size-md {
  width: 28px;
  height: 28px;
  border-radius: 7px;
  font-size: var(--font-xs);
  font-weight: 700;
}

.voicex-app-icon.size-sm {
  width: 18px;
  height: 18px;
  border-radius: 5px;
  font-size: 9px;
  font-weight: 700;
}

.voicex-app-icon.size-xs {
  width: 14px;
  height: 14px;
  border-radius: 3.5px;
  font-size: 8px;
  font-weight: 700;
}

.voicex-app-icon.is-badge {
  background: var(--color-accent-light);
  color: var(--color-accent);
}

.app-icon-img {
  width: 100%;
  height: 100%;
  object-fit: contain;
  display: block;
}

.app-icon-svg {
  width: 100%;
  height: 100%;
  display: block;
}

.app-icon-initials {
  line-height: 1;
  letter-spacing: -0.02em;
}
</style>
