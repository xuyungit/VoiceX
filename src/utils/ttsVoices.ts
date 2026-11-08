import type { SelectOption } from 'naive-ui'
import type { TtsProviderValue } from './ttsOptions'

export interface TtsVoice {
  id: string
  name: string
  language: string
}

export interface TtsVoiceSelectOption extends SelectOption {
  label: string
  value: string
  language: string
  searchText: string
}

export function primaryVoiceLanguage(tag: string) {
  return tag.toLowerCase().replace(/_/g, '-').split('-')[0]
}

export function buildTtsVoiceOptions(voices: TtsVoice[], provider: TtsProviderValue, locale: string): TtsVoiceSelectOption[] {
  const languageNames = new Intl.DisplayNames(locale, { type: 'language', style: 'short' })
  const englishNames = new Intl.DisplayNames('en', { type: 'language' })
  const options = voices.map((voice) => {
    const language = primaryVoiceLanguage(voice.language)
    const localizedLanguage = languageNames.of(voice.language)
    const name = provider === 'edge'
      ? voice.name.replace(/^Microsoft\s+/, '').replace(/\s+Online\s+\(Natural\)(?:\s*-.*)?$/, '')
      : voice.name
    return {
      label: provider === 'edge' ? `${localizedLanguage} · ${name}` : `${name} · ${voice.language}`,
      value: voice.id,
      language,
      searchText: [voice.id, voice.name, voice.language, localizedLanguage,
        languageNames.of(language), englishNames.of(voice.language), englishNames.of(language)]
        .join(' ').toLocaleLowerCase()
    }
  })
  if (provider === 'edge') {
    const collator = new Intl.Collator(locale)
    options.sort((a, b) => collator.compare(a.label, b.label))
  }
  return options
}

export function buildVoiceLanguageOptions(voices: TtsVoice[], locale: string) {
  const languageNames = new Intl.DisplayNames(locale, { type: 'language' })
  const englishNames = new Intl.DisplayNames('en', { type: 'language' })
  const collator = new Intl.Collator(locale)
  return [...new Set(voices.map((voice) => primaryVoiceLanguage(voice.language)))]
    .map((language) => ({
      label: languageNames.of(language)!,
      value: language,
      searchText: `${language} ${languageNames.of(language)} ${englishNames.of(language)}`.toLocaleLowerCase()
    }))
    .sort((a, b) => collator.compare(a.label, b.label))
}

export function filterTtsVoiceOption(pattern: string, option: SelectOption) {
  const searchable = String(option.searchText ?? `${option.label ?? ''} ${option.value ?? ''}`).toLocaleLowerCase()
  return pattern.trim().toLocaleLowerCase().split(/\s+/).every((word) => searchable.includes(word))
}
