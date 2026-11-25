import PROMPTS from '../shared/defaultPrompts.json'
import type { ResolvedLocale } from '../i18n'

export type PromptKind = 'assistant' | 'translation' | 'ttsTranslate' | 'ttsPreprocess'

export function getDefaultPrompt(kind: PromptKind, locale: ResolvedLocale): string {
  const pair = PROMPTS[kind]
  return locale === 'zh-CN' ? pair.zh : pair.en
}

export function isBuiltInDefaultPrompt(kind: PromptKind, value: string | null | undefined): boolean {
  const current = (value || '').trim()
  if (!current) return true

  const pair = PROMPTS[kind]
  return [pair.zh, pair.en].some((item) => item.trim() === current)
}
