// Drag-to-authorize helper: floats below System Settings while the user adds
// VoiceX to a privacy list. Pressing the icon starts a native file drag of
// VoiceX.app (see src-tauri/src/permissions/guide.rs).

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import zhCN from '../i18n/locales/zh-CN'
import enUS from '../i18n/locales/en-US'
import appIcon from '../../src-tauri/icons/128x128@2x.png'

type Kind = 'accessibility' | 'inputMonitoring'

interface GuideInfo {
  kind: Kind | 'microphone' | null
  appPath: string
  isAppBundle: boolean
}

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T

let strings = enUS.permissions
let dropped = false
let info: GuideInfo | null = null

function render() {
  if (!info) return
  const title = info.kind === 'inputMonitoring' ? strings.guideTitleInputMonitoring : strings.guideTitleAccessibility
  $('title').textContent = title
  $('hint').textContent = dropped ? strings.guideDropped : strings.guideHint
  $('dragLabel').textContent = strings.guideIconLabel
  $('closeButton').setAttribute('aria-label', strings.guideClose)
  $('closeButton').title = strings.guideClose
  $('dragTile').setAttribute('aria-label', `${strings.guideIconLabel}: ${info.appPath}`)
  const note = $('note')
  note.hidden = info.isAppBundle
  note.textContent = info.isAppBundle ? '' : strings.guideDevBuild
  document.querySelector<HTMLElement>('.guide')!.hidden = false
}

async function loadInfo() {
  info = await invoke<GuideInfo>('permission_guide_info')
  dropped = false
  render()
}

$<HTMLImageElement>('appIcon').src = appIcon

// The native drag must start while the button is still held, so it is
// started on mousedown rather than on an HTML drag event.
$('dragTile').addEventListener('mousedown', (event) => {
  if (event.button !== 0) return
  event.preventDefault()
  invoke('permission_guide_start_drag').catch((err) => {
    console.error('[permission-guide] drag failed:', err)
  })
})

$('closeButton').addEventListener('click', () => {
  invoke('permissions_close_guide')
})

listen('permission-guide:dropped', () => {
  dropped = true
  render()
})
listen('permission-guide:changed', loadInfo)

invoke<'zh-CN' | 'en-US'>('get_resolved_ui_locale')
  .then((locale) => {
    strings = locale === 'zh-CN' ? zhCN.permissions : enUS.permissions
    document.documentElement.lang = locale
  })
  .catch((err) => console.error('[permission-guide] locale:', err))
  .finally(loadInfo)
