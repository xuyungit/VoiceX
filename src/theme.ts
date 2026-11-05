import { computed, ref, watchEffect } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'

export type UiTheme = 'system' | 'light' | 'dark'

const media = window.matchMedia('(prefers-color-scheme: dark)')
const systemDark = ref(media.matches)
const preference = ref<UiTheme>('system')

// The webview reports the OS appearance until we pin the window theme, so
// only follow it while the preference is "system".
media.addEventListener('change', (event) => {
  if (preference.value === 'system') systemDark.value = event.matches
})

export const isDark = computed(() =>
  preference.value === 'system' ? systemDark.value : preference.value === 'dark'
)

// Runs before the settings load so the first paint already matches the OS.
watchEffect(() => {
  document.documentElement.dataset.theme = isDark.value ? 'dark' : 'light'
})

export async function applyUiTheme(next: UiTheme) {
  preference.value = next
  if (next === 'system') {
    // Unpin first so the media query reports the OS appearance again.
    await getCurrentWindow().setTheme(null)
    systemDark.value = media.matches
  } else {
    // Pins the native title bar (macOS) / frame (Windows) to the app theme.
    await getCurrentWindow().setTheme(next)
  }
}
