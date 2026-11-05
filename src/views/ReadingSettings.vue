<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { NButton, NInput, NSelect, NSlider, NSwitch, NTooltip, type SelectOption } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { useSettingsStore } from '../stores/settings'
import { formatHotkey } from '../utils/hotkey'
import { buildLlmProviderOptions } from '../utils/llmOptions'
import { getDefaultPrompt } from '../utils/llmPrompts'
import { isMacOS, isWindows } from '../utils/platform'
import { buildTtsProviderOptions, type TtsProviderValue as ProviderValue } from '../utils/ttsOptions'
import SettingsSection from '../components/settings/SettingsSection.vue'
import SettingRow from '../components/settings/SettingRow.vue'
import HelpTip from '../components/settings/HelpTip.vue'
import HotkeyField from '../components/settings/HotkeyField.vue'

interface TtsVoiceOption {
  id: string
  name: string
  language: string
}

interface TtsVoiceList {
  voices: TtsVoiceOption[]
  /** The model lists nothing and takes a typed (cloned or designed) id. */
  customVoiceOnly: boolean
}

interface ReadSelectionStatus {
  bound: boolean
  enabled: boolean
  conflictsWithDictation: boolean
  /** Lost to the plain reading key. Only ever set on the translate binding. */
  conflictsWithReading: boolean
  display: string | null
}

// The engine's own default rate, i.e. where the 1x mark sits on the stored
// 0..1 scale. Everything the sliders show is a multiple of it.
const DEFAULT_RATE = 0.5

// Shown when no binding has been stored; the same defaults as
// `default_read_selection` / `default_translate_selection` in
// src-tauri/src/hotkey/config.rs. macOS: Option + Command; Windows:
// Ctrl + Alt + Win, the dictation chord, since every two-modifier pair there
// is already taken (see `reading_default_modifiers`).
const DEFAULT_READ_HOTKEY = isWindows ? '82|6400|0' : '82|2304|0'
const DEFAULT_TRANSLATE_HOTKEY = isWindows ? '84|6400|0' : '84|2304|0'

// Two different platform questions. Reading text out of other applications is
// implemented for macOS and Windows; the local system voice exists only on
// macOS, so elsewhere reading needs one of the network engines.
const selectionReadingSupported = isMacOS || isWindows
const systemVoiceSupported = isMacOS

const settingsStore = useSettingsStore()
const { t, locale } = useI18n()
const router = useRouter()

const voices = ref<TtsVoiceOption[]>([])
// Declared by the backend with the list, so a model with no presets gets a
// text field without the view having to know which models those are.
const customVoiceOnly = ref(false)
const voicesError = ref('')
const voicesLoading = ref(false)
let voiceLoadGeneration = 0
const hotkeyStatus = ref<ReadSelectionStatus | null>(null)
const isRecording = ref(false)
const translateHotkeyStatus = ref<ReadSelectionStatus | null>(null)
const isRecordingTranslate = ref(false)
const previewLoading = ref(false)
const previewError = ref('')
// Speech is in progress. Known because the backend reports the end now; before
// the delegate existed there was no such event, which is why this used to be
// two buttons instead of one toggle.
const previewSpeaking = ref(false)
let unlistenPreviewEnded: UnlistenFn | null = null

const DIAGNOSE_COUNTDOWN_S = 5
const diagnoseCountdown = ref(0)
const diagnoseReport = ref<string>('')
const diagnoseError = ref('')
let diagnoseTimer: number | null = null
const showAdvanced = ref(false)

const ttsEnabled = computed({
  get: () => settingsStore.settings.ttsEnabled,
  set: (value: boolean) => {
    settingsStore.updateSetting('ttsEnabled', value)
    void applyHotkey()
  }
})

type AliyunModel = 'qwen3-tts-flash' | 'qwen-audio-3.0-tts-flash' | 'qwen-audio-3.1-tts-flash' | 'cosyvoice-v3-flash' | 'cosyvoice-v3.5-flash'

const providerOptions = computed(() =>
  buildTtsProviderOptions(t).map((option) =>
    option.value === 'system' && !systemVoiceSupported
      ? {
          ...option,
          label: t('reading.providerSystemUnsupported'),
          // Still selectable if it already is the provider — the default
          // setting names it — so the picker keeps showing what is in effect.
          disabled: settingsStore.settings.ttsProviderType !== 'system'
        }
      : option
  )
)

// The stored default on a platform without a system voice: every read would
// stop at "no engine", so the page says so where the engine is chosen.
const showSystemVoiceUnsupported = computed(
  () => !systemVoiceSupported && settingsStore.settings.ttsProviderType === 'system'
)

const ttsProviderType = computed({
  get: () => settingsStore.settings.ttsProviderType,
  set: (value: ProviderValue) => {
    void stopPreview()
    previewError.value = ''
    settingsStore.updateSetting('ttsProviderType', value)
    // The voice list is per provider and shares nothing across them.
    void loadVoices(value)
  }
})

const isVolcengine = computed(() => settingsStore.settings.ttsProviderType === 'volcengine')
const isAliyun = computed(() => settingsStore.settings.ttsProviderType === 'aliyun')
const isMimo = computed(() => settingsStore.settings.ttsProviderType === 'mimo')
const isAzure = computed(() => settingsStore.settings.ttsProviderType === 'azure')
const isEdge = computed(() => settingsStore.settings.ttsProviderType === 'edge')
// Everything that distinguishes "speaks over the network" from "speaks through
// macOS" — voice list availability, supported controls, whether the
// controls work off macOS at all.
const isCloud = computed(
  () => isVolcengine.value || isAliyun.value || isMimo.value || isAzure.value || isEdge.value
)
// Empty id is the `say` path (Siri / Spoken Content). Compact AVSpeech voices
// are everything else in the picker; pitch and volume only exist there.
const isSystemDefaultVoice = computed(
  () => !isCloud.value && !settingsStore.settings.systemTtsVoiceId
)

// Which delivery controls the selected engine honours — the same table as
// `voice_request` in src-tauri/src/tts/controller.rs. A slider for a parameter
// the engine drops would look like a broken setting, so the row is not shown.
//   - MiMo has no speed parameter; its only delivery control is the
//     instruction text. Volume is local playback gain, so that one is real.
//   - Edge takes rate and pitch in Hz; the other cloud engines take no pitch.
//     Volume is local playback gain, not a synthesis parameter.
//   - `say` (the system default voice) takes rate only. The compact AVSpeech
//     voices take all three.
const engineControls = computed(() => {
  if (isEdge.value) return { rate: true, pitch: true, volume: true }
  if (isMimo.value) return { rate: false, pitch: false, volume: true }
  if (isCloud.value) return { rate: true, pitch: false, volume: true }
  const compact = !isSystemDefaultVoice.value
  return { rate: true, pitch: compact, volume: compact }
})

const aliyunModelOptions = computed(() => [
  { label: t('reading.aliyunModelQwen3'), value: 'qwen3-tts-flash' },
  { label: t('reading.aliyunModelQwenAudio31'), value: 'qwen-audio-3.1-tts-flash' },
  { label: t('reading.aliyunModelQwenAudio'), value: 'qwen-audio-3.0-tts-flash' },
  { label: t('reading.aliyunModelCosyVoice'), value: 'cosyvoice-v3-flash' },
  { label: t('reading.aliyunModelCosyVoice35'), value: 'cosyvoice-v3.5-flash' }
])

const aliyunTtsModel = computed({
  get: () => settingsStore.settings.aliyunTtsModel,
  set: (value: AliyunModel) => {
    settingsStore.updateSetting('aliyunTtsModel', value)
    // The models have entirely separate voice tables.
    void loadVoices('aliyun', value)
  }
})

// Which voice setting the picker is editing. The Alibaba Cloud models
// reject each other's ids, so they cannot share one key: switching model would
// otherwise leave a voice that fails on the next read.
const aliyunVoiceKey = computed(() => {
  switch (settingsStore.settings.aliyunTtsModel) {
    case 'qwen-audio-3.0-tts-flash':
      return 'aliyunTtsVoiceQwenAudio' as const
    case 'qwen-audio-3.1-tts-flash':
      return 'aliyunTtsVoiceQwenAudio31' as const
    case 'cosyvoice-v3-flash':
      return 'aliyunTtsVoiceCosyVoice' as const
    case 'cosyvoice-v3.5-flash':
      return 'aliyunTtsVoiceCosyVoiceV35' as const
    default:
      return 'aliyunTtsVoiceQwen3' as const
  }
})

const voiceOptions = computed(() => {
  const listed = voices.value.map((voice) => ({
    label: `${voice.name} · ${voice.language}`,
    value: voice.id
  }))
  // The cloud providers have no default-voice concept, and an empty speaker is
  // not a valid request — so only the local engine gets that entry.
  return isCloud.value ? listed : [{ label: t('reading.voiceDefault'), value: '' }, ...listed]
})

function filterVoice(pattern: string, option: SelectOption) {
  const query = pattern.toLocaleLowerCase()
  return `${option.label ?? ''} ${option.value ?? ''}`.toLocaleLowerCase().includes(query)
}

const voiceNote = computed(() => {
  if (isEdge.value) return t('reading.edgeVoiceNote')
  if (customVoiceOnly.value) return t('reading.aliyunCustomVoiceNote')
  if (isCloud.value) return t('reading.cloudSpeakerNote')
  return isSystemDefaultVoice.value ? t('reading.voiceNoteDefault') : t('reading.voiceNote')
})

// Voice identifiers do not carry across providers, so each keeps its own.
const ttsVoiceId = computed({
  get: () => {
    if (isVolcengine.value) return settingsStore.settings.volcTtsSpeaker
    if (isAliyun.value) return settingsStore.settings[aliyunVoiceKey.value]
    if (isMimo.value) return settingsStore.settings.mimoTtsVoice
    if (isAzure.value) return settingsStore.settings.azureTtsVoice
    if (isEdge.value) return settingsStore.settings.edgeTtsVoice
    return settingsStore.settings.systemTtsVoiceId
  },
  set: (value: string) => {
    if (isVolcengine.value) settingsStore.updateSetting('volcTtsSpeaker', value)
    else if (isAliyun.value) settingsStore.updateSetting(aliyunVoiceKey.value, value)
    else if (isMimo.value) settingsStore.updateSetting('mimoTtsVoice', value)
    else if (isAzure.value) settingsStore.updateSetting('azureTtsVoice', value)
    else if (isEdge.value) settingsStore.updateSetting('edgeTtsVoice', value)
    else settingsStore.updateSetting('systemTtsVoiceId', value)
  }
})

const volcTtsApiKey = computed({
  get: () => settingsStore.settings.volcTtsApiKey,
  set: (value: string) => settingsStore.updateSetting('volcTtsApiKey', value)
})

const volcTtsResourceId = computed({
  get: () => settingsStore.settings.volcTtsResourceId,
  set: (value: string) => settingsStore.updateSetting('volcTtsResourceId', value)
})

const aliyunTtsApiKey = computed({
  get: () => settingsStore.settings.aliyunTtsApiKey,
  set: (value: string) => settingsStore.updateSetting('aliyunTtsApiKey', value)
})

const mimoTtsApiKey = computed({
  get: () => settingsStore.settings.mimoTtsApiKey,
  set: (value: string) => settingsStore.updateSetting('mimoTtsApiKey', value)
})

const aliyunTtsInstruction = computed({
  get: () => settingsStore.settings.aliyunTtsInstruction,
  set: (value: string) => settingsStore.updateSetting('aliyunTtsInstruction', value)
})

// Mirrors `accepts_instruction` in src-tauri/src/tts/aliyun.rs. CosyVoice v3's
// system voices reject free text outright, and Qwen3-TTS has no such field.
const aliyunAcceptsInstruction = computed(() =>
  ['qwen-audio-3.1-tts-flash', 'qwen-audio-3.0-tts-flash', 'cosyvoice-v3.5-flash']
    .includes(settingsStore.settings.aliyunTtsModel)
)

const mimoTtsInstruction = computed({
  get: () => settingsStore.settings.mimoTtsInstruction,
  set: (value: string) => settingsStore.updateSetting('mimoTtsInstruction', value)
})

const azureTtsApiKey = computed({
  get: () => settingsStore.settings.azureTtsApiKey,
  set: (value: string) => settingsStore.updateSetting('azureTtsApiKey', value)
})

const azureTtsRegion = computed({
  get: () => settingsStore.settings.azureTtsRegion,
  set: (value: string) => settingsStore.updateSetting('azureTtsRegion', value)
})

// Rate and volume belong to the provider, not to the feature: engines differ
// in baseline speed and loudness, so tuning one must not move the other.
// MiMo is absent here on purpose: its API has no speed parameter, so the rate
// slider is hidden for it rather than shown doing nothing.
const rateMultiplier = computed({
  get: () => {
    const stored = isVolcengine.value
      ? settingsStore.settings.volcTtsRate
      : isAliyun.value
        ? settingsStore.settings.aliyunTtsRate
        : isAzure.value
          ? settingsStore.settings.azureTtsRate
          : isEdge.value
            ? settingsStore.settings.edgeTtsRate
            : settingsStore.settings.systemTtsRate
    return round2(stored / DEFAULT_RATE)
  },
  set: (value: number) => {
    const stored = clamp(value * DEFAULT_RATE, 0, 1)
    if (isVolcengine.value) settingsStore.updateSetting('volcTtsRate', stored)
    else if (isAliyun.value) settingsStore.updateSetting('aliyunTtsRate', stored)
    else if (isAzure.value) settingsStore.updateSetting('azureTtsRate', stored)
    else if (isEdge.value) settingsStore.updateSetting('edgeTtsRate', stored)
    else settingsStore.updateSetting('systemTtsRate', stored)
  }
})

const volumePercent = computed({
  get: () => {
    const stored = isVolcengine.value
      ? settingsStore.settings.volcTtsVolume
      : isAliyun.value
        ? settingsStore.settings.aliyunTtsVolume
        : isMimo.value
          ? settingsStore.settings.mimoTtsVolume
          : isAzure.value
            ? settingsStore.settings.azureTtsVolume
            : isEdge.value
              ? settingsStore.settings.edgeTtsVolume
              : settingsStore.settings.systemTtsVolume
    return Math.round(stored * 100)
  },
  set: (value: number) => {
    const stored = clamp(value / 100, 0, 1)
    if (isVolcengine.value) settingsStore.updateSetting('volcTtsVolume', stored)
    else if (isAliyun.value) settingsStore.updateSetting('aliyunTtsVolume', stored)
    else if (isMimo.value) settingsStore.updateSetting('mimoTtsVolume', stored)
    else if (isAzure.value) settingsStore.updateSetting('azureTtsVolume', stored)
    else if (isEdge.value) settingsStore.updateSetting('edgeTtsVolume', stored)
    else settingsStore.updateSetting('systemTtsVolume', stored)
  }
})

const edgePitchHz = computed({
  get: () => settingsStore.settings.edgeTtsPitchHz,
  set: (value: number) => settingsStore.updateSetting('edgeTtsPitchHz', Math.round(clamp(value, -100, 100)))
})

// Compact system voices use a multiplier; Edge uses a separate Hz offset.
const pitchMultiplier = computed({
  get: () => round2(settingsStore.settings.systemTtsPitch),
  set: (value: number) => settingsStore.updateSetting('systemTtsPitch', clamp(value, 0.5, 2))
})

const clipboardFallback = computed({
  get: () => settingsStore.settings.ttsClipboardFallback,
  set: (value: boolean) => settingsStore.updateSetting('ttsClipboardFallback', value)
})

const displayHotkey = computed(
  () =>
    hotkeyStatus.value?.display ??
    formatHotkey(settingsStore.settings.ttsHotkeyConfig ?? DEFAULT_READ_HOTKEY)
)

const showConflict = computed(() => hotkeyStatus.value?.conflictsWithDictation ?? false)

// What the default keys collide with, and how the compatibility mode copies,
// differ per platform; everything else on the page reads the same.
const hotkeyNote = computed(() =>
  isWindows ? t('reading.hotkeyNoteWindows') : t('reading.hotkeyNote')
)
const translateHotkeyNote = computed(() =>
  isWindows ? t('reading.translateHotkeyNoteWindows') : t('reading.translateHotkeyNote')
)
const clipboardFallbackNote = computed(() =>
  isWindows ? t('reading.clipboardFallbackNoteWindows') : t('reading.clipboardFallbackNote')
)

// --- Translate and read -----------------------------------------------------

// Mirrors `LANGUAGES` in src-tauri/src/tts/llm_stage.rs. The code is what is
// stored; the Rust side turns it into the language name the prompt receives.
const TRANSLATE_LANGUAGES = ['zh-CN', 'zh-TW', 'en', 'ja', 'ko', 'fr', 'de', 'es', 'ru', 'pt'] as const
type TranslateLanguage = (typeof TRANSLATE_LANGUAGES)[number]
const LANGUAGE_LABEL_KEYS: Record<TranslateLanguage, string> = {
  'zh-CN': 'reading.langZhCN',
  'zh-TW': 'reading.langZhTW',
  en: 'reading.langEn',
  ja: 'reading.langJa',
  ko: 'reading.langKo',
  fr: 'reading.langFr',
  de: 'reading.langDe',
  es: 'reading.langEs',
  ru: 'reading.langRu',
  pt: 'reading.langPt'
}
// `TRANSLATE_MAX_CHARS` in llm_stage.rs; shown in the note, enforced there.
const TRANSLATE_MAX_CHARS = 5000

type PromptLocale = Parameters<typeof getDefaultPrompt>[1]
const resolvedLocale = computed<PromptLocale>(() => (locale.value === 'zh-CN' ? 'zh-CN' : 'en-US'))

const languageOptions = computed(() =>
  TRANSLATE_LANGUAGES.map((code) => ({ label: t(LANGUAGE_LABEL_KEYS[code]), value: code }))
)
const sourceLanguageOptions = computed(() => [
  { label: t('reading.langAuto'), value: 'auto' },
  ...languageOptions.value
])

const clipboardWhenNoSelection = computed({
  get: () => settingsStore.settings.ttsClipboardWhenNoSelection,
  set: (value: boolean) => settingsStore.updateSetting('ttsClipboardWhenNoSelection', value)
})

const translateEnabled = computed({
  get: () => settingsStore.settings.ttsTranslateEnabled,
  set: (value: boolean) => {
    settingsStore.updateSetting('ttsTranslateEnabled', value)
    void applyTranslateHotkey()
  }
})

const translateSourceLanguage = computed({
  get: () => settingsStore.settings.ttsTranslateSourceLanguage,
  set: (value: string) => settingsStore.updateSetting('ttsTranslateSourceLanguage', value)
})

const translateTargetLanguage = computed({
  get: () => settingsStore.settings.ttsTranslateTargetLanguage,
  set: (value: string) => settingsStore.updateSetting('ttsTranslateTargetLanguage', value)
})

const translatePromptTemplate = computed({
  get: () => settingsStore.settings.ttsTranslatePromptTemplate,
  set: (value: string) => settingsStore.updateSetting('ttsTranslatePromptTemplate', value)
})

const translateCopyToClipboard = computed({
  get: () => settingsStore.settings.ttsTranslateCopyToClipboard,
  set: (value: boolean) => settingsStore.updateSetting('ttsTranslateCopyToClipboard', value)
})

const translateSaveHistory = computed({
  get: () => settingsStore.settings.ttsTranslateSaveHistory,
  set: (value: boolean) => settingsStore.updateSetting('ttsTranslateSaveHistory', value)
})

const captionsEnabled = computed({
  get: () => settingsStore.settings.ttsCaptionsEnabled,
  set: (value: boolean) => settingsStore.updateSetting('ttsCaptionsEnabled', value)
})

const ttsLlmProviderKey = computed({
  get: () => settingsStore.settings.ttsLlmProviderKey,
  set: (value: string) => settingsStore.updateSetting('ttsLlmProviderKey', value)
})

const preprocessEnabled = computed({
  get: () => settingsStore.settings.ttsPreprocessEnabled,
  set: (value: boolean) => settingsStore.updateSetting('ttsPreprocessEnabled', value)
})

const preprocessPromptTemplate = computed({
  get: () => settingsStore.settings.ttsPreprocessPromptTemplate,
  set: (value: string) => settingsStore.updateSetting('ttsPreprocessPromptTemplate', value)
})

// The same list as the LLM page, plus "follow": credentials and model names
// stay there, this page only picks which one reading uses.
const llmProviderOptions = computed(() => [
  { label: t('reading.llmFollow'), value: 'follow' },
  ...buildLlmProviderOptions(t, settingsStore.settings.llmCustomEndpoints)
])

// One override slot per provider, Aliyun per model — the scheme of
// `translate_voice_key` in src-tauri/src/tts/controller.rs.
const translateVoiceKey = computed(() =>
  isAliyun.value
    ? `aliyun:${settingsStore.settings.aliyunTtsModel}`
    : settingsStore.settings.ttsProviderType
)

const translateVoiceOverride = computed({
  get: () => settingsStore.settings.ttsTranslateVoiceOverrides[translateVoiceKey.value] ?? '',
  set: (value: string) => {
    const next = { ...settingsStore.settings.ttsTranslateVoiceOverrides }
    if (value.trim()) next[translateVoiceKey.value] = value
    else delete next[translateVoiceKey.value]
    settingsStore.updateSetting('ttsTranslateVoiceOverrides', next)
  }
})

function primaryLanguage(tag: string) {
  return tag.toLowerCase().replace('_', '-').split('-')[0]
}

function voiceSpeaks(voice: TtsVoiceOption, language: string) {
  return primaryLanguage(voice.language) === primaryLanguage(language)
}

const translateVoiceOptions = computed(() => {
  const target = translateTargetLanguage.value
  const matching = voices.value
    .filter((voice) => voiceSpeaks(voice, target))
    .map((voice) => ({ label: `${voice.name} · ${voice.language}`, value: voice.id }))
  // A selection that no longer matches the target stays visible: hiding it
  // would leave a voice in effect that the picker denies exists.
  const selected = translateVoiceOverride.value
  if (selected && !matching.some((option) => option.value === selected)) {
    const voice = voices.value.find((candidate) => candidate.id === selected)
    matching.unshift({
      label: voice ? `${voice.name} · ${voice.language}` : selected,
      value: selected
    })
  }
  return [{ label: t('reading.translateVoiceFollow'), value: '' }, ...matching]
})

// The voice a translate-and-read session will actually use.
const effectiveTranslateVoice = computed(() => translateVoiceOverride.value || ttsVoiceId.value)

const translateVoiceHint = computed(() => {
  const id = effectiveTranslateVoice.value
  // The page already says the engine is unavailable; a hint about its voice
  // would only add noise.
  if (showSystemVoiceUnsupported.value) return ''
  if (!id) {
    // Cloud engines always name a voice; only the system engine falls back to
    // `say`, whose voice follows the OS language, not the target.
    return isCloud.value ? '' : t('reading.translateVoiceSayNote')
  }
  const voice = voices.value.find((candidate) => candidate.id === id)
  // A hand-typed id has no language on record, so there is nothing to check.
  if (!voice || voiceSpeaks(voice, translateTargetLanguage.value)) return ''
  return t('reading.translateVoiceMismatch', { voice: voice.name, language: voice.language })
})

const displayTranslateHotkey = computed(
  () =>
    translateHotkeyStatus.value?.display ??
    formatHotkey(settingsStore.settings.ttsTranslateHotkeyConfig ?? DEFAULT_TRANSLATE_HOTKEY)
)

const translateConflict = computed<'dictation' | 'reading' | null>(() => {
  const status = translateHotkeyStatus.value
  if (!status) return null
  if (status.conflictsWithDictation) return 'dictation'
  if (status.conflictsWithReading) return 'reading'
  return null
})

async function applyTranslateHotkey() {
  try {
    translateHotkeyStatus.value = await invoke<ReadSelectionStatus>(
      'apply_translate_selection_hotkey',
      {
        config: settingsStore.settings.ttsTranslateHotkeyConfig,
        enabled: settingsStore.settings.ttsTranslateEnabled
      }
    )
  } catch (error) {
    console.error('Failed to apply the translate hotkey', error)
  }
}

async function refreshTranslateHotkeyStatus() {
  try {
    translateHotkeyStatus.value = await invoke<ReadSelectionStatus>(
      'translate_selection_hotkey_status'
    )
  } catch (error) {
    console.error('Failed to read the translate hotkey status', error)
  }
}

async function startRecordingTranslate() {
  isRecordingTranslate.value = true
  try {
    const result = await invoke<{ storage: string; display: string }>('record_hotkey')
    settingsStore.updateSetting('ttsTranslateHotkeyConfig', result.storage)
    await applyTranslateHotkey()
  } catch (error) {
    console.error('Hotkey record failed', error)
  } finally {
    isRecordingTranslate.value = false
  }
}

async function resetTranslateHotkey() {
  settingsStore.updateSetting('ttsTranslateHotkeyConfig', null)
  await applyTranslateHotkey()
}

function resetTranslatePrompt() {
  translatePromptTemplate.value = getDefaultPrompt('ttsTranslate', resolvedLocale.value)
}

function resetPreprocessPrompt() {
  preprocessPromptTemplate.value = getDefaultPrompt('ttsPreprocess', resolvedLocale.value)
}

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value))
}

function round2(value: number) {
  return Math.round(value * 100) / 100
}

async function applyHotkey() {
  try {
    hotkeyStatus.value = await invoke<ReadSelectionStatus>('apply_read_selection_hotkey', {
      config: settingsStore.settings.ttsHotkeyConfig,
      enabled: settingsStore.settings.ttsEnabled
    })
  } catch (error) {
    console.error('Failed to apply the reading hotkey', error)
  }
  // Reading outranks translate, so this may have taken the translate key
  // away or handed it back.
  await refreshTranslateHotkeyStatus()
}

async function startRecording() {
  isRecording.value = true
  try {
    const result = await invoke<{ storage: string; display: string }>('record_hotkey')
    settingsStore.updateSetting('ttsHotkeyConfig', result.storage)
    await applyHotkey()
  } catch (error) {
    console.error('Hotkey record failed', error)
  } finally {
    isRecording.value = false
  }
}

async function resetHotkey() {
  settingsStore.updateSetting('ttsHotkeyConfig', null)
  await applyHotkey()
}

async function loadVoices(
  provider: ProviderValue = settingsStore.settings.ttsProviderType,
  model: AliyunModel = settingsStore.settings.aliyunTtsModel
) {
  const generation = ++voiceLoadGeneration
  voicesError.value = ''
  // The cloud providers are network-only, so their voice lists work everywhere;
  // only the system voice needs macOS.
  if (!systemVoiceSupported && provider === 'system') {
    voices.value = []
    customVoiceOnly.value = false
    voicesLoading.value = false
    return
  }
  // Clear first: a slow reply must not leave the previous provider's voices on
  // screen, which is how system voices used to show up under the cloud engine.
  // Reset the custom-voice flag too: it belongs to the previous provider/model.
  voices.value = []
  customVoiceOnly.value = false
  voicesLoading.value = true
  try {
    // Both passed explicitly — the store's save is debounced, so the backend
    // would still read the previous provider and model from the database.
    const list = await invoke<TtsVoiceList>('list_tts_voices', { provider, model })
    if (generation !== voiceLoadGeneration) return
    voices.value = list.voices
    customVoiceOnly.value = list.customVoiceOnly
    voicesError.value = ''
  } catch (error) {
    if (generation !== voiceLoadGeneration) return
    voices.value = []
    customVoiceOnly.value = false
    voicesError.value = error instanceof Error ? error.message : String(error)
  } finally {
    if (generation === voiceLoadGeneration) voicesLoading.value = false
  }
}

async function togglePreview() {
  if (previewSpeaking.value) {
    await stopPreview()
    return
  }

  previewLoading.value = true
  previewSpeaking.value = true
  previewError.value = ''
  try {
    // The backend reads the voice parameters from the store, so the debounced
    // save has to land before the preview starts or it auditions stale values.
    await settingsStore.forceSaveSettings()
    await invoke('preview_tts', { text: t('reading.previewText') })
  } catch (error) {
    previewError.value = error instanceof Error ? error.message : String(error)
    previewSpeaking.value = false
  } finally {
    previewLoading.value = false
  }
}

async function stopPreview() {
  // Cleared here rather than waiting for the event: the button must respond to
  // the click, not to the round trip.
  previewSpeaking.value = false
  try {
    await invoke('stop_tts')
  } catch (error) {
    console.error('Failed to stop speech', error)
  }
}

async function runDiagnostics() {
  diagnoseError.value = ''
  diagnoseReport.value = ''
  // Clicking the button puts VoiceX in front, so a read now would only ever
  // find our own window. Count down while the user switches back.
  diagnoseCountdown.value = DIAGNOSE_COUNTDOWN_S
  diagnoseTimer = window.setInterval(() => {
    diagnoseCountdown.value -= 1
    if (diagnoseCountdown.value <= 0 && diagnoseTimer !== null) {
      clearInterval(diagnoseTimer)
      diagnoseTimer = null
    }
  }, 1000)

  try {
    const report = await invoke('diagnose_selection', {
      delayMs: DIAGNOSE_COUNTDOWN_S * 1000
    })
    diagnoseReport.value = JSON.stringify(report, null, 2)
  } catch (error) {
    diagnoseError.value = error instanceof Error ? error.message : String(error)
  } finally {
    diagnoseCountdown.value = 0
    if (diagnoseTimer !== null) {
      clearInterval(diagnoseTimer)
      diagnoseTimer = null
    }
  }
}

async function copyDiagnostics() {
  try {
    await navigator.clipboard.writeText(diagnoseReport.value)
  } catch (error) {
    console.error('Failed to copy the report', error)
  }
}

onMounted(async () => {
  // The dictation hotkey may have changed on another page since this binding
  // was applied, so ask what the state actually is rather than assuming.
  try {
    hotkeyStatus.value = await invoke<ReadSelectionStatus>('read_selection_hotkey_status')
  } catch (error) {
    console.error('Failed to read the reading hotkey status', error)
  }
  await refreshTranslateHotkeyStatus()
  unlistenPreviewEnded = await listen<string | null>('tts:preview_ended', (event) => {
    previewSpeaking.value = false
    if (event.payload) previewError.value = event.payload
  })
  await loadVoices()
})

onBeforeUnmount(() => {
  voiceLoadGeneration += 1
  unlistenPreviewEnded?.()
  unlistenPreviewEnded = null
  if (diagnoseTimer !== null) {
    clearInterval(diagnoseTimer)
    diagnoseTimer = null
  }
})
</script>

<template>
  <div class="page settings-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('reading.title') }}</h1>
    </div>

    <div v-if="!selectionReadingSupported" class="settings-callout">{{ t('reading.unsupportedPlatform') }}</div>

    <!-- The feature itself comes first — whether it is on, and which key. Both
         are independent of the engine, which follows as one card together
         with everything that belongs to it. -->
    <SettingsSection :title="t('reading.general')" :help="t('reading.generalSub')">
      <!-- Both ways of triggering a read, one row each with its own switch.
           The two keys compete with each other (reading outranks translate)
           and with dictation, so they and their conflict warnings are read
           side by side. What a translate-and-read then does with the text is
           its own card further down. -->
      <SettingRow class="wide-control" :label="t('reading.hotkey')" :help="hotkeyNote">
        <HotkeyField
          :value="isRecording ? t('reading.pressHotkey') : displayHotkey ?? ''"
          :recording="isRecording"
          :clearable="!!settingsStore.settings.ttsHotkeyConfig"
          @clear="resetHotkey"
        />
        <NButton
          :disabled="isRecording || !ttsEnabled || !selectionReadingSupported"
          size="small"
          @click="startRecording"
        >
          {{ t('reading.record') }}
        </NButton>
        <NSwitch v-model:value="ttsEnabled" :disabled="!selectionReadingSupported" />
      </SettingRow>

      <div v-if="showConflict" class="settings-callout">
        {{ t('reading.hotkeyConflict') }}
      </div>

      <SettingRow class="wide-control" :label="t('reading.translateHotkey')" :help="translateHotkeyNote">
        <HotkeyField
          :value="isRecordingTranslate ? t('reading.pressHotkey') : displayTranslateHotkey ?? ''"
          :recording="isRecordingTranslate"
          :clearable="!!settingsStore.settings.ttsTranslateHotkeyConfig"
          @clear="resetTranslateHotkey"
        />
        <NButton
          :disabled="isRecordingTranslate || !translateEnabled || !selectionReadingSupported"
          size="small"
          @click="startRecordingTranslate"
        >
          {{ t('reading.record') }}
        </NButton>
        <NSwitch v-model:value="translateEnabled" :disabled="!selectionReadingSupported" />
      </SettingRow>

      <div v-if="translateConflict === 'dictation'" class="settings-callout">
        {{ t('reading.translateHotkeyConflictDictation') }}
      </div>
      <div v-else-if="translateConflict === 'reading'" class="settings-callout">
        {{ t('reading.translateHotkeyConflictReading') }}
      </div>

      <!-- Applies to both keys above, so it sits under them rather than in
           either key's own card. -->
      <SettingRow :label="t('reading.clipboardWhenNoSelection')" :help="t('reading.clipboardWhenNoSelectionNote')">
        <NSwitch
          v-model:value="clipboardWhenNoSelection"
          :disabled="!selectionReadingSupported"
          size="small"
        />
      </SettingRow>
    </SettingsSection>

    <SettingsSection :title="t('reading.provider')" :help="t('reading.providerSub')">
      <SettingRow :label="t('reading.ttsProvider')">
        <NSelect
          v-model:value="ttsProviderType"
          :options="providerOptions"
          size="small"
        />
      </SettingRow>

      <div v-if="showSystemVoiceUnsupported" class="settings-callout">
        {{ t('reading.systemVoiceUnsupported') }}
      </div>

      <SettingRow v-if="isEdge" :label="t('reading.edgeConnection')" :help="t('reading.edgeConnectionNote')">
      </SettingRow>

      <template v-if="isVolcengine">
        <SettingRow :label="t('reading.volcApiKey')" :help="t('reading.volcApiKeyNote')">
          <NInput
            v-model:value="volcTtsApiKey"
            type="password"
            show-password-on="click"
            size="small"
            :placeholder="t('reading.volcApiKeyPlaceholder')"
          />
        </SettingRow>
        <SettingRow :label="t('reading.volcResourceId')" :help="t('reading.volcResourceIdNote')">
          <NInput
            v-model:value="volcTtsResourceId"
            size="small"
            placeholder="seed-tts-2.0"
          />
        </SettingRow>
      </template>

      <template v-if="isAliyun">
        <SettingRow :label="t('reading.aliyunApiKey')" :help="t('reading.aliyunApiKeyNote')">
          <NInput
            v-model:value="aliyunTtsApiKey"
            type="password"
            show-password-on="click"
            size="small"
            placeholder="sk-..."
          />
        </SettingRow>
        <SettingRow :label="t('reading.aliyunModel')" :help="t('reading.aliyunModelNote')">
          <NSelect
            v-model:value="aliyunTtsModel"
            :options="aliyunModelOptions"
            size="small"
          />
        </SettingRow>
        <SettingRow v-if="aliyunAcceptsInstruction" :label="t('reading.aliyunInstruction')" :help="t('reading.aliyunInstructionNote')">
          <NInput
            v-model:value="aliyunTtsInstruction"
            size="small"
            :placeholder="t('reading.aliyunInstructionPlaceholder')"
          />
        </SettingRow>
      </template>

      <template v-if="isMimo">
        <SettingRow :label="t('reading.mimoApiKey')" :help="t('reading.mimoApiKeyNote')">
          <NInput
            v-model:value="mimoTtsApiKey"
            type="password"
            show-password-on="click"
            size="small"
            :placeholder="t('reading.mimoApiKeyPlaceholder')"
          />
        </SettingRow>
        <SettingRow :label="t('reading.mimoInstruction')" :help="t('reading.mimoInstructionNote')">
          <NInput
            v-model:value="mimoTtsInstruction"
            size="small"
            :placeholder="t('reading.mimoInstructionPlaceholder')"
          />
        </SettingRow>
      </template>

      <template v-if="isAzure">
        <SettingRow :label="t('reading.azureApiKey')" :help="t('reading.azureApiKeyNote')">
          <NInput
            v-model:value="azureTtsApiKey"
            type="password"
            show-password-on="click"
            size="small"
            :placeholder="t('reading.azureApiKeyPlaceholder')"
          />
        </SettingRow>
        <SettingRow :label="t('reading.azureRegion')" :help="t('reading.azureRegionNote')">
          <NInput
            v-model:value="azureTtsRegion"
            size="small"
            placeholder="eastus"
          />
        </SettingRow>
      </template>

      <!-- The voice and its delivery parameters sit under the engine that
           owns them: the list is per engine (per model on Alibaba Cloud),
           the ids do not carry across, and which sliders exist is the
           engine's call. They used to be a card of their own two cards
           further down, which hid that the two pickers move together. -->
      <!-- Nothing below works without an engine; the warning above says
           what to do instead. -->
      <template v-if="!showSystemVoiceUnsupported">
        <h3 class="settings-subhead">
          <span>{{ t('reading.voice') }}</span>
          <HelpTip :text="t('reading.voiceSub')" />
        </h3>

        <SettingRow :label="t('reading.voiceLabel')" :help="isEdge ? `${voiceNote} ${t('reading.edgeStreamNote')}` : voiceNote">
          <NInput
            v-if="customVoiceOnly"
            v-model:value="ttsVoiceId"
            size="small"
            placeholder="cosyvoice-v3.5-flash-vd-..."
          />
          <NSelect
            v-else
            v-model:value="ttsVoiceId"
            :options="voiceOptions"
            :filter="filterVoice"
            :loading="voicesLoading"
            :disabled="!systemVoiceSupported && !isCloud"
            :tag="isCloud"
            filterable
            size="small"
          />
          <NTooltip v-if="isEdge">
            <template #trigger>
              <NButton
                size="small"
                quaternary
                class="icon-button"
                :disabled="voicesLoading"
                :aria-label="t('reading.refreshVoices')"
                @click="loadVoices()"
              >
                <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" :class="{ spinning: voicesLoading }">
                  <path
                    d="M13.5 8a5.5 5.5 0 1 1-1.61-3.89M13.5 2.5v3h-3"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.5"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                  />
                </svg>
              </NButton>
            </template>
            {{ t('reading.refreshVoices') }}
          </NTooltip>
        </SettingRow>

        <div v-if="voicesError" class="settings-callout is-error">
          {{ t('reading.voiceLoadFailed') }} — {{ voicesError }}
        </div>

        <!-- Which of the three sliders exist is the engine's call; see
             engineControls for the table. -->
        <SettingRow v-if="engineControls.rate" :label="t('reading.rate')">
          <div class="control-group">
            <NSlider
              v-model:value="rateMultiplier"
              :min="0.5"
              :max="2"
              :step="0.05"
              :disabled="!systemVoiceSupported && !isCloud"
              class="slider"
            />
            <span class="slider-value">{{ rateMultiplier.toFixed(2) }}x</span>
          </div>
        </SettingRow>

        <SettingRow v-if="engineControls.pitch && !isEdge" :label="t('reading.pitch')">
          <div class="control-group">
            <NSlider
              v-model:value="pitchMultiplier"
              :min="0.5"
              :max="2"
              :step="0.05"
              :disabled="!systemVoiceSupported"
              class="slider"
            />
            <span class="slider-value">{{ pitchMultiplier.toFixed(2) }}x</span>
          </div>
        </SettingRow>

        <SettingRow v-if="isEdge" :label="t('reading.pitch')" :help="t('reading.edgePitchNote')">
          <div class="control-group">
            <NSlider v-model:value="edgePitchHz" :min="-100" :max="100" :step="5" class="slider" />
            <span class="slider-value">{{ edgePitchHz > 0 ? '+' : '' }}{{ edgePitchHz }} Hz</span>
          </div>
        </SettingRow>

        <SettingRow v-if="engineControls.volume" :label="t('reading.volume')" :help="isCloud ? t('reading.volumeCloudNote') : undefined">
          <div class="control-group">
            <NSlider
              v-model:value="volumePercent"
              :min="0"
              :max="100"
              :step="5"
              :disabled="!systemVoiceSupported && !isCloud"
              class="slider"
            />
            <span class="slider-value">{{ volumePercent }}%</span>
          </div>
        </SettingRow>

        <SettingRow :label="t('reading.preview')" :help="t('reading.previewNote')">
            <NButton
              :loading="previewLoading"
              :disabled="!systemVoiceSupported && !isCloud"
              :type="previewSpeaking ? 'default' : 'primary'"
              secondary
              size="small"
              @click="togglePreview"
            >
              {{ previewSpeaking ? t('reading.previewStop') : t('reading.preview') }}
            </NButton>
        </SettingRow>

        <div v-if="previewError" class="settings-callout is-error">
          {{ t('reading.previewFailed') }} — {{ previewError }}
        </div>
      </template>
    </SettingsSection>

    <!-- Translate-and-read: what happens to the text between the key and the
         voice. Its switch and hotkey are in the first card next to the plain
         reading key; its LLM and prompt live here, not on the LLM page,
         because nothing but reading ever uses them. -->
    <SettingsSection :title="t('reading.translateTitle')" :help="t('reading.translateSub')">
      <SettingRow :label="t('reading.sourceLanguage')" :help="t('reading.sourceLanguageNote')">
        <NSelect
          v-model:value="translateSourceLanguage"
          :options="sourceLanguageOptions"
          size="small"
        />
      </SettingRow>

      <SettingRow :label="t('reading.targetLanguage')" :help="t('reading.targetLanguageNote')">
        <NSelect
          v-model:value="translateTargetLanguage"
          :options="languageOptions"
          size="small"
        />
      </SettingRow>

      <SettingRow :label="t('reading.llmProvider')" :help="t('reading.llmProviderNote')">
        <template #status>
          <div>
            <NButton text size="tiny" class="inline-link" @click="router.push('/llm-settings')">
              {{ t('reading.llmProviderLink') }}
            </NButton>
          </div>
        </template>
        <NSelect
          v-model:value="ttsLlmProviderKey"
          :options="llmProviderOptions"
          size="small"
        />
      </SettingRow>

      <SettingRow :label="t('reading.translateVoice')" :help="t('reading.translateVoiceNote')">
        <NInput
          v-if="customVoiceOnly"
          v-model:value="translateVoiceOverride"
          size="small"
          :placeholder="t('reading.translateVoiceFollow')"
        />
        <NSelect
          v-else
          v-model:value="translateVoiceOverride"
          :options="translateVoiceOptions"
          :filter="filterVoice"
          :disabled="!systemVoiceSupported && !isCloud"
          :tag="isCloud"
          filterable
          size="small"
        />
      </SettingRow>
      <div v-if="translateVoiceHint" class="settings-callout">{{ translateVoiceHint }}</div>

      <SettingRow :label="t('reading.translateClipboard')" :help="t('reading.translateClipboardNote')">
        <NSwitch v-model:value="translateCopyToClipboard" />
      </SettingRow>

      <SettingRow :label="t('reading.translateHistory')" :help="t('reading.translateHistoryNote')">
        <NSwitch v-model:value="translateSaveHistory" />
      </SettingRow>

      <SettingRow :label="t('reading.captions')" :help="t(isEdge ? 'reading.edgeCaptionsNote' : 'reading.captionsNote')">
        <NSwitch v-model:value="captionsEnabled" />
      </SettingRow>

      <div class="prompt-block">
        <div class="prompt-header">
          <div class="prompt-label">
            <span>{{ t('reading.translatePrompt') }}</span>
            <HelpTip :text="t('reading.translatePromptNote', { max: TRANSLATE_MAX_CHARS })" />
          </div>
          <NButton size="small" quaternary @click="resetTranslatePrompt">
            {{ t('reading.resetPrompt') }}
          </NButton>
        </div>
        <NInput v-model:value="translatePromptTemplate" type="textarea" :rows="10" />
      </div>
    </SettingsSection>

    <SettingsSection :title="t('reading.preprocessTitle')" :help="t('reading.preprocessSub')">
      <SettingRow :label="t('reading.preprocessEnabled')" :help="t('reading.preprocessEnabledNote')">
        <NSwitch v-model:value="preprocessEnabled" />
      </SettingRow>

      <div v-if="preprocessEnabled" class="prompt-block">
        <div class="prompt-header">
          <div class="prompt-label">{{ t('reading.preprocessPrompt') }}</div>
          <NButton size="small" quaternary @click="resetPreprocessPrompt">
            {{ t('reading.resetPrompt') }}
          </NButton>
        </div>
        <NInput v-model:value="preprocessPromptTemplate" type="textarea" :rows="8" />
      </div>
    </SettingsSection>

    <SettingsSection v-if="settingsStore.settings.enableDiagnostics" :title="t('reading.diagnostics')" :help="t('reading.diagnosticsSub')">
      <SettingRow :label="t('reading.diagnosticsRun')" :help="t('reading.diagnosticsHint')">
        <div class="control-group">
          <NButton
            :loading="diagnoseCountdown > 0"
            :disabled="diagnoseCountdown > 0"
            size="small"
            @click="runDiagnostics"
          >
            {{
              diagnoseCountdown > 0
                ? t('reading.diagnosticsCountdown', { seconds: diagnoseCountdown })
                : t('reading.diagnosticsRun')
            }}
          </NButton>
          <NButton
            v-if="diagnoseReport"
            quaternary
            size="small"
            @click="copyDiagnostics"
          >
            {{ t('common.copy') }}
          </NButton>
        </div>
      </SettingRow>

      <pre v-if="diagnoseReport" class="diagnostics-report">{{ diagnoseReport }}</pre>
      <div v-if="diagnoseError" class="settings-callout is-error">{{ diagnoseError }}</div>
    </SettingsSection>

    <SettingsSection :title="t('reading.advanced')" :help="t('reading.advancedSub')">
      <template #extra>
        <NButton
          size="small"
          quaternary
          :aria-expanded="showAdvanced"
          :aria-label="t('reading.advanced')"
          @click="showAdvanced = !showAdvanced"
        >
          <span class="advanced-chevron" :class="{ open: showAdvanced }" aria-hidden="true">›</span>
        </NButton>
      </template>
      <template v-if="showAdvanced">
        <SettingRow :label="t('reading.clipboardFallback')" :help="clipboardFallbackNote">
          <NSwitch v-model:value="clipboardFallback" :disabled="!selectionReadingSupported" />
        </SettingRow>
      </template>
    </SettingsSection>
  </div>
</template>

<style scoped>
.prompt-block {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-sm);
  padding-top: 12px;
  border-top: 1px solid var(--color-divider);
}

.prompt-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-md);
}

.prompt-label {
  display: flex;
  align-items: center;
  gap: 6px;
  font-weight: 500;
  color: var(--color-text-primary);
}

.inline-link {
  vertical-align: baseline;
}

.settings-subhead {
  display: flex;
  align-items: center;
  gap: 6px;
}

.slider {
  flex: 1;
}

.slider-value {
  width: 56px;
  text-align: right;
  font-size: var(--font-xs);
  color: var(--color-text-secondary);
  font-variant-numeric: tabular-nums;
}

.advanced-chevron {
  display: inline-block;
  font-size: var(--font-lg);
  line-height: 1;
  color: var(--color-text-tertiary);
  transition: transform var(--transition-fast);
}

.advanced-chevron.open {
  transform: rotate(90deg);
}

.diagnostics-report {
  margin: 0;
  padding: 12px 14px;
  border-radius: var(--radius-md);
  background: var(--color-bg-tertiary);
  border: 1px solid var(--color-border);
  font-family: var(--font-mono);
  font-size: var(--font-xs);
  line-height: 1.55;
  color: var(--color-text-secondary);
  max-height: 320px;
  overflow: auto;
  white-space: pre;
  user-select: text;
}
</style>
