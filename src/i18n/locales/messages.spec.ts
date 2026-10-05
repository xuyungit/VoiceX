import { createI18n } from 'vue-i18n'
import zhCN from './zh-CN'
import enUS from './en-US'

const catalogs = { 'zh-CN': zhCN, 'en-US': enUS }

// Assert the same pipeline rejects bad syntax. A development compiler that
// merely logs the error must not let this build check pass silently.
function checkProductionCompiler() {
  const { global: composer } = createI18n({
    legacy: false,
    locale: 'en-US',
    messages: { 'en-US': { invalid: '{"enable_thinking": false}' } }
  })
  let rejected = false
  try { composer.t('invalid') } catch { rejected = true }
  if (!rejected) throw new Error('The locale check must reject invalid messages in production mode.')
}

function messageKeys(messages: object, prefix = ''): string[] {
  return Object.entries(messages).flatMap(([key, value]) => {
    const path = prefix ? `${prefix}.${key}` : key
    return typeof value === 'string' ? [path] : messageKeys(value, path)
  })
}

export function checkLocaleMessages() {
  checkProductionCompiler()
  const failures: string[] = []
  let checked = 0
  for (const [locale, messages] of Object.entries(catalogs)) {
    const { global: composer } = createI18n({ legacy: false, locale, messages: { [locale]: messages } })
    for (const key of messageKeys(messages)) {
      try {
        composer.t(key)
      } catch (error) {
        failures.push(`${locale}:${key}: ${String(error)}`)
      }
      checked++
    }
    const literals: Record<string, string[]> = {
      'llm.extraBodySub': ['{"enable_thinking": false}', '{"thinking": {"type": "disabled"}}', '{"max_tokens": 8192}'],
      'llm.extraBodyPlaceholder': ['{"enable_thinking": false}'],
      'llm.extraBodyNotObject': ['{ ... }'],
      'llm.promptHint': ['{DICTIONARY}', '{INPUT_HISTORY}']
    }
    for (const [key, expected] of Object.entries(literals)) {
      try {
        const rendered = composer.t(key)
        for (const literal of expected) {
          if (!rendered.includes(literal)) failures.push(`${locale}:${key}: missing ${literal}`)
        }
      } catch {
        // Syntax failures have already been collected above.
      }
    }
  }
  if (failures.length) throw new Error(failures.join('\n'))
  return checked
}
