import { invoke } from '@tauri-apps/api/core'
import { createI18n } from 'vue-i18n'
import zhCN from './locales/zh-CN'
import enUS from './locales/en-US'

export type UiLanguage = 'system' | 'zh-CN' | 'en-US'
export type ResolvedLocale = 'zh-CN' | 'en-US'

const messages = {
  'zh-CN': zhCN,
  'en-US': enUS
}

export function normalizeLocale(locale: string | null | undefined): ResolvedLocale {
  const normalized = (locale || '').toLowerCase()
  return normalized.startsWith('zh') ? 'zh-CN' : 'en-US'
}

export function setLocale(locale: ResolvedLocale) {
  i18n.global.locale.value = locale
  document.documentElement.lang = locale
}

export async function resolveLocale(preferred: UiLanguage): Promise<ResolvedLocale> {
  if (preferred === 'zh-CN' || preferred === 'en-US') {
    return preferred
  }

  try {
    const locale = await invoke<ResolvedLocale>('get_resolved_ui_locale', {
      preferred
    })
    return normalizeLocale(locale)
  } catch (error) {
    console.error('Failed to resolve ui locale from backend:', error)
    return normalizeLocale(window.navigator.language)
  }
}

const initialLocale = normalizeLocale(window.navigator.language)

export const i18n = createI18n({
  legacy: false,
  locale: initialLocale,
  fallbackLocale: 'en-US',
  messages
})
