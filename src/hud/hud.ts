// VoiceX HUD Logic (TypeScript source)
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import zhCN from "../i18n/locales/zh-CN";
import enUS from "../i18n/locales/en-US";
import { trimCaptionPunctuation } from "../utils/text";

const hudCapsule = document.getElementById("hudCapsule");
const statusIcon = document.getElementById("statusIcon");
const miniWave = document.getElementById("miniWave");
const countdown = document.getElementById("countdown");
const textArea = document.getElementById("textArea");
const intentChip = document.getElementById("intentChip");
const waveformBars = document.getElementById("waveformBars");
const waveformHybridCanvas = document.getElementById(
  "waveformHybridCanvas",
) as HTMLCanvasElement | null;
const titleElement = document.querySelector("title");

if (!hudCapsule || !statusIcon || !countdown || !textArea || !intentChip || !waveformBars) {
  console.error("[HUD] Missing required DOM elements", {
    hudCapsule: !!hudCapsule,
    statusIcon: !!statusIcon,
    countdown: !!countdown,
    textArea: !!textArea,
    intentChip: !!intentChip,
    waveformBars: !!waveformBars,
  });
}

const BATCH_WAVEFORM_STYLE: "timeline" | "hybrid" = "timeline";
const WAVEFORM_BAR_COUNT = 15;
const HYBRID_BAND_COUNT = 8;
const HYBRID_HISTORY_COLUMNS = 26;
/// Stream dictation's mini waveform: the level spread over five bars, tallest
/// in the middle.
const MINI_WAVE_SHAPE = [0.55, 0.85, 1, 0.8, 0.6];
const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

/// The setting makes the capsule's fill translucent. On every platform the
/// window itself is transparent at full opacity, so text and bars stay solid.
function setHudTranslucent(enabled: boolean) {
  document.body.classList.toggle("hud-translucent", enabled);
}

if (waveformBars && waveformBars.childElementCount === 0) {
  const fragment = document.createDocumentFragment();
  for (let i = 0; i < WAVEFORM_BAR_COUNT; i += 1) {
    fragment.appendChild(document.createElement("span"));
  }
  waveformBars.appendChild(fragment);
}

const waveformBarNodes = waveformBars
  ? (Array.from(waveformBars.querySelectorAll("span")) as HTMLSpanElement[])
  : [];

if (miniWave && miniWave.childElementCount === 0) {
  for (let i = 0; i < MINI_WAVE_SHAPE.length; i += 1) {
    miniWave.appendChild(document.createElement("span"));
  }
}
const miniWaveNodes = miniWave
  ? (Array.from(miniWave.querySelectorAll("span")) as HTMLSpanElement[])
  : [];

let currentMode:
  | "idle"
  | "push_to_talk"
  | "hands_free"
  | "recognizing"
  | "correcting"
  // Selected-text reading. Two modes, not one, because a cloud engine takes
  // 300-700 ms to produce its first audio and that silence otherwise looks
  // exactly like the hotkey having done nothing.
  | "reading_prepare"
  // The LLM stage before speech: translation, or preprocessing of a plain
  // read. The wand is the only signal in compact presentation, so it gets a
  // mode of its own rather than a flag on prepare.
  | "reading_translate"
  | "reading_speak"
  | "error" = "idle";
/// What the current read is. Decides the chip: a translate-and-read session
/// keeps its green "翻译朗读" label through every phase, a plain read (even
/// one that is being preprocessed by the LLM) stays "朗读".
let readingKind: "read" | "translate" = "read";
/// Where the current read's text came from. A read that fell back to the
/// clipboard says so on the chip: it looks exactly like a read of the
/// selection otherwise, and the two can hold different text. Can change once
/// mid-session, when the selection turns out to be unreadable.
let readingSource: "selection" | "clipboard" = "selection";
/// When a real output level last arrived. Reading only draws the waveform
/// while one is flowing: the macOS voice never reports a level, and bars
/// standing still read as a broken widget rather than as silence.
let lastLevelAt = 0;
/// The selection was longer than the engine accepts and is only partly read.
// Stable error code from the backend, mapped to display text at render time.
let currentErrorCode: string | null = null;
let currentIntent: "assistant" | "translate_en" = "assistant";
/// Fixed for a session by the backend: the window is sized to match.
let hudPresentation: "stream" | "batch" | "caption" = "stream";
/// The sentence being spoken, shown in place of the transcript while a read
/// runs with captions. Empty until the first sentence is heard; the last one
/// stays through the linger and is cleared once the window has hidden.
let captionText = "";
let lastActiveIcon: keyof typeof icons = "mic";
let partialText = "";
let lastNonEmptyText = "";
/// How much of `partialText` / `lastNonEmptyText` the recognizer has
/// committed, in UTF-16 units. What follows may still change and is drawn
/// dimmer.
let partialCommitted = 0;
let lastNonEmptyCommitted = 0;
/// The text was typed. The check mark stays until the next session starts,
/// whatever the session's reset reports in the meantime.
let inserted = false;
let currentLocale: "zh-CN" | "en-US" = "en-US";
let currentAudioLevel = 0;
let currentAudioSpectrum = Array.from({ length: HYBRID_BAND_COUNT }, () => 0);
let smoothedAudioLevel = 0;
let waveformFrameId = 0;
let waveformLastFrameTime = 0;
let waveformShiftAccumulator = 0;
const MAX_LINES = 2;
// A caption is a film subtitle: at most two lines, as wide as 80% of the
// screen. A piece that wraps past two lines loses its head to the ellipsis.
const CAPTION_MAX_LINES = 2;
const CAPTION_SCREEN_WIDTH_FRACTION = 0.8;
/// A streaming transcript's capsule grows with the text up to this share of
/// the screen, then wraps to its second line.
const STREAM_SCREEN_WIDTH_FRACTION = 0.5;
/// Transparent margin around the capsule, for its shadow. Must match
/// `--hud-pad` in hud.css and `HUD_SHADOW_PAD` in window.rs.
const HUD_PAD = 20;
const ELLIPSIS = "\u2026";
const WAVEFORM_ATTACK = 0.4;
const WAVEFORM_RELEASE = 0.15;
const WAVEFORM_GAIN = 6.5;
/// Display gain for reading, whose output is far hotter than a microphone's.
/// Derived from measured output RMS rather than picked by eye — see
/// `handleAudioLevelUpdate`.
const READING_WAVEFORM_GAIN = 2.4;
/// Seconds of level history the bars span, newest on the right.
const WAVEFORM_HISTORY_SECONDS = 1.1;
/// Seconds a processing pulse takes to cross the bars.
const WAVEFORM_PROCESSING_SWEEP_SECONDS = 0.8;
const WAVEFORM_HISTORY_SCROLL_SPEED = WAVEFORM_BAR_COUNT / WAVEFORM_HISTORY_SECONDS;
const WAVEFORM_PROCESSING_SCROLL_SPEED =
  WAVEFORM_BAR_COUNT / WAVEFORM_PROCESSING_SWEEP_SECONDS;
const WAVEFORM_IDLE_FLOOR = 0.04;
const WAVEFORM_MIN_SCALE = 0.16;

const waveformHistory = Array.from(
  { length: waveformBarNodes.length },
  () => 0,
);
const smoothedSpectrum = Array.from({ length: HYBRID_BAND_COUNT }, () => 0);
const hybridSpectrumHistory = Array.from(
  { length: HYBRID_HISTORY_COLUMNS },
  () => Array.from({ length: HYBRID_BAND_COUNT }, () => 0),
);
let textAreaMaxWidth = 0;
let textAreaLineHeight = 12 * 1.4;
let textMeasureEl: HTMLDivElement | null = null;

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

function getTextMeasureEl() {
  if (textMeasureEl) return textMeasureEl;
  const el = document.createElement("div");
  el.setAttribute("aria-hidden", "true");
  Object.assign(el.style, {
    position: "fixed",
    left: "-100000px",
    top: "0",
    visibility: "hidden",
    pointerEvents: "none",
    whiteSpace: "normal",
    padding: "0",
    margin: "0",
    border: "0",
    boxSizing: "content-box",
    maxHeight: "none",
    minHeight: "0",
    overflow: "visible",
  });
  document.body.appendChild(el);
  textMeasureEl = el;
  return el;
}

/// The widest the capsule may become. Stream and caption windows are a share
/// of the screen, sized by window.rs (`HudWidth::ScreenShare`) before they are
/// shown; the text is wrapped against that same share rather than the
/// window's width, which can reach the page after the first render does. A
/// batch window has a fixed width.
function capsuleMaxWidth() {
  if (isCaptionMode()) {
    return Math.floor(window.screen.width * CAPTION_SCREEN_WIDTH_FRACTION);
  }
  if (hudPresentation === "stream") {
    return Math.floor(window.screen.width * STREAM_SCREEN_WIDTH_FRACTION);
  }
  return window.innerWidth - 2 * HUD_PAD;
}

/// What the capsule spends on everything beside the text: its padding, and
/// whichever of icon, waveform, countdown and chip are showing, a gap each.
function capsuleChromeWidth() {
  if (!hudCapsule) return 0;
  const style = getComputedStyle(hudCapsule);
  const gap = parseFloat(style.columnGap) || 0;
  let width =
    (parseFloat(style.paddingLeft) || 0) + (parseFloat(style.paddingRight) || 0);
  for (const child of Array.from(hudCapsule.children) as HTMLElement[]) {
    if (child === textArea) continue;
    // offsetWidth: layout size, unaffected by the icon's breathing scale.
    if (child.offsetWidth > 0) width += child.offsetWidth + gap;
  }
  return width;
}

function updateTextAreaMaxWidth() {
  if (!textArea) return;
  const style = getComputedStyle(textArea);
  // offsetWidth rounds to whole pixels; one pixel of slack keeps the text
  // wrapping where it was measured instead of a hair earlier.
  textAreaMaxWidth = Math.floor(capsuleMaxWidth() - capsuleChromeWidth()) - 1;
  if (textAreaMaxWidth <= 0) textAreaMaxWidth = 230;
  textArea.style.maxWidth = `${textAreaMaxWidth}px`;

  const parsedLineHeight = parseFloat(style.lineHeight);
  if (Number.isFinite(parsedLineHeight) && parsedLineHeight > 0) {
    textAreaLineHeight = parsedLineHeight;
  } else {
    const fontSize = parseFloat(style.fontSize) || 15;
    textAreaLineHeight = fontSize * 1.4;
  }

  const measureEl = getTextMeasureEl();
  measureEl.style.width = `${Math.max(1, textAreaMaxWidth)}px`;
  measureEl.style.font = style.font;
  measureEl.style.fontFamily = style.fontFamily;
  measureEl.style.fontSize = style.fontSize;
  measureEl.style.fontWeight = style.fontWeight;
  measureEl.style.fontStyle = style.fontStyle;
  measureEl.style.lineHeight = style.lineHeight;
  measureEl.style.letterSpacing = style.letterSpacing;
  measureEl.style.wordSpacing = style.wordSpacing;
  measureEl.style.wordBreak = style.wordBreak;
  measureEl.style.overflowWrap = style.overflowWrap;
  measureEl.style.textTransform = style.textTransform;
}

function maxLines() {
  return isCaptionMode() ? CAPTION_MAX_LINES : MAX_LINES;
}

function fitText(text: string): string {
  if (!text) return text;
  const measureEl = getTextMeasureEl();
  const maxHeight = textAreaLineHeight * maxLines() + 1;
  const fits = (candidate: string) => {
    measureEl.textContent = candidate;
    return measureEl.scrollHeight <= maxHeight;
  };

  if (fits(text)) return text;

  const keepPrefix = currentMode === "error";
  let lo = 0;
  let hi = text.length;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    const candidate = keepPrefix
      ? `${text.slice(0, mid).trimEnd()}${ELLIPSIS}`
      : `${ELLIPSIS}${text.slice(-mid)}`;
    if (fits(candidate)) {
      lo = mid;
    } else {
      hi = mid - 1;
    }
  }
  if (lo <= 0) return ELLIPSIS;
  return keepPrefix
    ? `${text.slice(0, lo).trimEnd()}${ELLIPSIS}`
    : `${ELLIPSIS}${text.slice(-lo)}`;
}

window.addEventListener("resize", () => {
  renderTranscript();
});

const icons: Record<string, Element | null | undefined> = {
  mic: statusIcon?.querySelector(".icon-mic") ?? null,
  waveform: statusIcon?.querySelector(".icon-waveform") ?? null,
  cloud: statusIcon?.querySelector(".icon-cloud") ?? null,
  wand: statusIcon?.querySelector(".icon-wand") ?? null,
  speaking: statusIcon?.querySelector(".icon-speaking") ?? null,
  check: statusIcon?.querySelector(".icon-check") ?? null,
  alert: statusIcon?.querySelector(".icon-alert") ?? null,
};

const hudMessages = {
  "zh-CN": zhCN.hud,
  "en-US": enUS.hud,
};

function t(key: keyof typeof zhCN.hud) {
  return hudMessages[currentLocale][key];
}

function setHudLocale(locale: string | undefined) {
  currentLocale = locale === "zh-CN" ? "zh-CN" : "en-US";
  document.documentElement.lang = currentLocale;
  if (titleElement) {
    titleElement.textContent = "VoiceX HUD";
  }
  updateIntent(currentIntent);
  renderTranscript();
}

/// Errors reported by a stable code rather than a message: reading's
/// selection failures the user can act on, and dictation's failed typing.
/// Anything else is shown as sent.
const errorCodeMessages: Record<string, keyof typeof zhCN.hud> = {
  // The final text did not reach the app (session/handlers/finalize.rs).
  injection_failed: "injectionFailed",
  no_selection: "readingNoSelection",
  permission_denied: "readingPermissionDenied",
  secure_input: "readingSecureInput",
  // Windows: UIPI keeps a normal process out of an elevated one.
  target_elevated: "readingTargetElevated",
  focus_is_self: "readingFocusIsSelf",
  unsupported_control: "readingUnsupported",
  copy_timeout: "readingUnsupported",
  backend: "readingBackendError",
  // No engine to speak with at all — the system voice off macOS.
  unsupported: "readingEngineUnavailable",
  start_timeout: "readingBackendError",
  // LLM stage failures. The read is refused outright — never a fallback to
  // the untranslated text.
  llm_not_configured: "readingLlmNotConfigured",
  llm_failed: "readingLlmFailed",
  llm_timeout: "readingLlmTimeout",
  text_too_long: "readingTextTooLong",
  // No readable selection, and the clipboard read in its place failed too.
  clipboard_empty: "readingClipboardEmpty",
  clipboard_not_text: "readingClipboardNotText",
  clipboard_concealed: "readingClipboardConcealed",
  clipboard_unavailable: "readingClipboardUnavailable",
};

function errorCodeText() {
  const key = currentErrorCode ? errorCodeMessages[currentErrorCode] : undefined;
  return key ? t(key) : null;
}

function showIcon(name: keyof typeof icons) {
  Object.values(icons).forEach((icon) => icon?.classList.remove("active"));
  icons[name]?.classList.add("active");
}

function isBatchWaveMode() {
  return (
    hudPresentation === "batch" &&
    (currentMode === "push_to_talk" || currentMode === "hands_free")
  );
}

/// A read showing its captions uses the text layout, not the compact card.
function isCaptionMode() {
  return hudPresentation === "caption";
}

function isCompactBatchMode() {
  return hudPresentation === "batch" || (isReadingMode() && !isCaptionMode());
}

function isReadingMode() {
  return (
    currentMode === "reading_prepare" ||
    currentMode === "reading_translate" ||
    currentMode === "reading_speak"
  );
}

function hasRecentLevel() {
  return lastLevelAt > 0 && performance.now() - lastLevelAt < 500;
}

function currentCompactBatchMode() {
  return isCompactBatchMode() && currentMode !== "error";
}

/// Whether the bars are drawn right now.
///
/// Called on every level tick as well as on render, because whether a level is
/// flowing is precisely what decides this — and levels start arriving *after*
/// the render that switches to the speaking state. Computing it only at render
/// time left the bars hidden for the whole read and revealed them for the last
/// 400 ms, once the closing render happened to run with a level in hand.
function applyWaveformVisibility(compactBatchMode: boolean) {
  if (!waveformBars) return;
  const readingWithoutLevel = isReadingMode() && !hasRecentLevel();
  const wasHidden = waveformBars.hidden;
  waveformBars.hidden =
    !compactBatchMode ||
    BATCH_WAVEFORM_STYLE !== "timeline" ||
    readingWithoutLevel ||
    inserted;
  if (wasHidden !== waveformBars.hidden) fitCapsuleSize();
}

/// Stream dictation leads with a mini waveform rather than an icon: it shows
/// the microphone is hearing you, and its colour carries the stage.
function isStreamWaveMode() {
  return (
    hudPresentation === "stream" &&
    !isReadingMode() &&
    currentMode !== "error" &&
    !inserted
  );
}

function setBatchLayoutMode(batchWaveMode: boolean, compactBatchMode: boolean) {
  const batchErrorMode = isCompactBatchMode() && currentMode === "error";
  document.body.classList.toggle("batch-wave-mode", batchWaveMode);
  document.body.classList.toggle("compact-batch-mode", compactBatchMode);
  document.body.classList.toggle("batch-error-mode", batchErrorMode);
  document.body.classList.toggle("caption-mode", isCaptionMode());

  // Batch dictation is the waveform alone: its colour and motion carry the
  // stage. A read keeps its icon beside the bars, because the system voice
  // reports no level and its bars never appear. Once the text is typed, a
  // check mark takes the place of either.
  document.body.classList.toggle("inserted", inserted);
  if (inserted) showIcon("check");
  if (statusIcon) {
    statusIcon.hidden =
      !inserted && ((compactBatchMode && !isReadingMode()) || isStreamWaveMode());
  }
  if (miniWave) {
    miniWave.hidden = !isStreamWaveMode() || isCaptionMode();
  }
  applyWaveformVisibility(compactBatchMode);
  if (waveformHybridCanvas) {
    waveformHybridCanvas.hidden =
      !compactBatchMode || BATCH_WAVEFORM_STYLE !== "hybrid";
  }
}

function updateStatus(mode: typeof currentMode) {
  const wasReading = isReadingMode();
  currentMode = mode;
  if (wasReading !== isReadingMode()) {
    renderIntentChip();
  }
  document.body.classList.remove("recording", "recognizing", "correcting", "error");

  switch (mode) {
    case "push_to_talk":
      document.body.classList.add("recording");
      showIcon(isCompactBatchMode() ? "waveform" : "mic");
      statusIcon?.classList.add("animating");
      lastActiveIcon = isCompactBatchMode() ? "waveform" : "mic";
      break;
    case "hands_free":
      document.body.classList.add("recording");
      showIcon("waveform");
      statusIcon?.classList.add("animating");
      lastActiveIcon = "waveform";
      break;
    case "recognizing":
      document.body.classList.add("recognizing");
      showIcon("cloud");
      statusIcon?.classList.add("animating");
      lastActiveIcon = "cloud";
      break;
    case "correcting":
      document.body.classList.add("correcting");
      showIcon("wand");
      statusIcon?.classList.add("animating");
      lastActiveIcon = "wand";
      break;
    // Both reading phases share the icon; the waveform appearing is what
    // separates them, and in compact presentation there is no text to read.
    case "reading_prepare":
      document.body.classList.add("recognizing");
      showIcon("speaking");
      statusIcon?.classList.add("animating");
      lastActiveIcon = "speaking";
      break;
    // Same wand and colour as dictation correction: it is the same kind of
    // wait, on the same kind of model.
    case "reading_translate":
      document.body.classList.add("correcting");
      showIcon("wand");
      statusIcon?.classList.add("animating");
      lastActiveIcon = "wand";
      break;
    case "reading_speak":
      document.body.classList.add("recording");
      showIcon("speaking");
      statusIcon?.classList.add("animating");
      lastActiveIcon = "speaking";
      break;
    case "error":
      document.body.classList.add("error");
      showIcon("alert");
      statusIcon?.classList.remove("animating");
      break;
    case "idle":
    default:
      showIcon(lastActiveIcon);
      statusIcon?.classList.remove("animating");
      break;
  }

  renderTranscript();
}

function resetTranscript() {
  partialText = "";
  lastNonEmptyText = "";
  partialCommitted = 0;
  lastNonEmptyCommitted = 0;
  currentAudioLevel = 0;
  smoothedAudioLevel = 0;
  waveformLastFrameTime = 0;
  waveformShiftAccumulator = 0;
  for (let i = 0; i < waveformHistory.length; i += 1) {
    waveformHistory[i] = 0;
  }
  for (let i = 0; i < currentAudioSpectrum.length; i += 1) {
    currentAudioSpectrum[i] = 0;
    smoothedSpectrum[i] = 0;
  }
  for (let i = 0; i < hybridSpectrumHistory.length; i += 1) {
    hybridSpectrumHistory[i].fill(0);
  }
  renderWaveformBars();
  renderHybridSpectrum();
  renderTranscript();
}

function renderWaveformBars() {
  const lastIndex = Math.max(1, waveformHistory.length - 1);
  const processingMotion = isProcessingWaveMode();
  const time = performance.now() / 1000;
  const pulseIndexA = processingMotion
    ? ((time * 1.35) % 1) * lastIndex
    : (1 - ((time * 1.35) % 1)) * lastIndex;
  const pulseIndexB = processingMotion
    ? (((time * 1.35) + 0.42) % 1) * lastIndex
    : (1 - (((time * 1.35) + 0.42) % 1)) * lastIndex;

  waveformBarNodes.forEach((bar, index) => {
    const level = waveformHistory[index] ?? 0;
    const age = index / lastIndex;
    const lift = 0.84 + age * 0.24;
    let normalized = clamp(level * lift, 0, 1);

    if (processingMotion) {
      const distanceA = Math.abs(index - pulseIndexA);
      const distanceB = Math.abs(index - pulseIndexB);
      const pulseA = Math.max(0, 1 - distanceA / 2.6);
      const pulseB = Math.max(0, 1 - distanceB / 2.2);
      const packetLift = Math.max(pulseA * 0.34, pulseB * 0.24);
      normalized = clamp(normalized + packetLift, 0, 1);
    }

    const scale = clamp(
      WAVEFORM_MIN_SCALE + normalized * 0.9,
      WAVEFORM_MIN_SCALE,
      1,
    );
    const opacity = clamp(0.18 + normalized * 0.72 + age * 0.08, 0.18, 1);
    const glow = clamp(0.2 + normalized * 0.8, 0.2, 1);
    bar.style.setProperty("--bar-scale", scale.toFixed(3));
    bar.style.setProperty("--bar-opacity", opacity.toFixed(3));
    bar.style.setProperty("--bar-glow", glow.toFixed(3));
  });
}

function shiftWaveformHistory(count: number, value: number) {
  if (!waveformHistory.length || count <= 0) return;

  const clampedValue = clamp(value, 0, 1);
  const actual = Math.min(count, waveformHistory.length);
  if (isProcessingWaveMode()) {
    waveformHistory.splice(waveformHistory.length - actual, actual);
    for (let i = 0; i < actual; i += 1) {
      waveformHistory.unshift(clampedValue);
    }
  } else {
    waveformHistory.splice(0, actual);
    for (let i = 0; i < actual; i += 1) {
      waveformHistory.push(clampedValue);
    }
  }
}

function hasWaveformEnergy() {
  return waveformHistory.some((value) => value > 0.01);
}

function hasHybridSpectrumEnergy() {
  return hybridSpectrumHistory.some((column) =>
    column.some((value) => value > 0.01),
  );
}

function isProcessingWaveMode() {
  return (
    isCompactBatchMode() &&
    (currentMode === "recognizing" || currentMode === "correcting")
  );
}

function getProcessingWaveLevel(time: number) {
  const t = time / 1000;
  const beatA = Math.pow(Math.max(0, Math.sin(t * 8.4)), 6);
  const beatB = Math.pow(Math.max(0, Math.sin(t * 5.2 + 0.7)), 8);
  const kick = Math.pow(Math.max(0, Math.sin(t * 2.8 + 1.4)), 10);
  const chatter = Math.pow(Math.max(0, Math.sin(t * 13.5 + 0.2)), 16);

  if (currentMode === "correcting") {
    return clamp(
      0.08 + beatA * 0.42 + beatB * 0.26 + kick * 0.18 + chatter * 0.12,
      0,
      1,
    );
  }

  return clamp(
    0.06 + beatA * 0.34 + beatB * 0.24 + kick * 0.16 + chatter * 0.08,
    0,
    1,
  );
}

function getProcessingSpectrum(time: number) {
  const t = time / 1000;
  return Array.from({ length: HYBRID_BAND_COUNT }, (_, index) => {
    const bandBias = index / Math.max(1, HYBRID_BAND_COUNT - 1);
    const phase = index * 0.72;
    const primary = Math.sin(t * (1.6 + bandBias * 0.8) + phase) * 0.5 + 0.5;
    const harmonic =
      Math.sin(t * (3.1 + bandBias * 1.1) + phase * 1.4 + 0.8) * 0.5 + 0.5;
    const combined = primary * 0.65 + harmonic * 0.35;
    const floor = currentMode === "correcting" ? 0.12 : 0.08;
    const range = currentMode === "correcting" ? 0.34 : 0.28;
    return clamp(floor + combined * range, 0, 1);
  });
}

function updateHybridSpectrumHistory(nextSpectrum: number[]) {
  if (!hybridSpectrumHistory.length) return;

  const newest = hybridSpectrumHistory[hybridSpectrumHistory.length - 1];
  for (let i = 0; i < newest.length; i += 1) {
    newest[i] += (nextSpectrum[i] - newest[i]) * 0.58;
  }
}

function shiftHybridSpectrumHistory(count: number, nextSpectrum: number[]) {
  if (count <= 0 || !hybridSpectrumHistory.length) return;

  const actual = Math.min(count, hybridSpectrumHistory.length);
  if (isProcessingWaveMode()) {
    hybridSpectrumHistory.splice(hybridSpectrumHistory.length - actual, actual);
    for (let i = 0; i < actual; i += 1) {
      hybridSpectrumHistory.unshift([...nextSpectrum]);
    }
  } else {
    hybridSpectrumHistory.splice(0, actual);
    for (let i = 0; i < actual; i += 1) {
      hybridSpectrumHistory.push([...nextSpectrum]);
    }
  }
}

function renderHybridSpectrum() {
  if (!waveformHybridCanvas) return;

  const ctx = waveformHybridCanvas.getContext("2d");
  if (!ctx) return;

  const cssWidth = waveformHybridCanvas.clientWidth || 118;
  const cssHeight = waveformHybridCanvas.clientHeight || 26;
  const dpr = window.devicePixelRatio || 1;
  const displayWidth = Math.max(1, Math.round(cssWidth * dpr));
  const displayHeight = Math.max(1, Math.round(cssHeight * dpr));

  if (
    waveformHybridCanvas.width !== displayWidth ||
    waveformHybridCanvas.height !== displayHeight
  ) {
    waveformHybridCanvas.width = displayWidth;
    waveformHybridCanvas.height = displayHeight;
  }

  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, cssWidth, cssHeight);

  const columns = hybridSpectrumHistory.length;
  const bands = HYBRID_BAND_COUNT;
  const gapX = 1;
  const gapY = 1;
  const cellWidth = Math.max(
    2,
    Math.floor((cssWidth - gapX * (columns - 1)) / columns),
  );
  const cellHeight = Math.max(
    2,
    Math.floor((cssHeight - gapY * (bands - 1)) / bands),
  );
  const startX = Math.max(
    0,
    cssWidth - (cellWidth * columns + gapX * (columns - 1)),
  );

  for (let col = 0; col < columns; col += 1) {
    const age = col / Math.max(1, columns - 1);
    const x = startX + col * (cellWidth + gapX);
    const column = hybridSpectrumHistory[col];

    for (let band = 0; band < bands; band += 1) {
      const y = cssHeight - (band + 1) * cellHeight - band * gapY;
      const intensity = clamp(column[band] ?? 0, 0, 1);
      if (intensity <= 0.01) continue;

      const hue = 198 - band * 8 + age * 10;
      const alpha = 0.12 + intensity * (0.28 + age * 0.34);
      ctx.fillStyle = `hsla(${hue}, 92%, ${68 - band * 2}%, ${alpha})`;
      ctx.fillRect(x, y, cellWidth, cellHeight);
    }
  }
}

function startWaveformLoop() {
  if (waveformFrameId) return;

  const tick = (time: number) => {
    waveformFrameId = 0;
    const dt =
      waveformLastFrameTime > 0
        ? Math.min((time - waveformLastFrameTime) / 1000, 0.05)
        : 1 / 60;
    waveformLastFrameTime = time;
    const target =
      isCompactBatchMode() || isStreamWaveMode() ? currentAudioLevel : 0;
    const easing =
      target > smoothedAudioLevel ? WAVEFORM_ATTACK : WAVEFORM_RELEASE;
    smoothedAudioLevel += (target - smoothedAudioLevel) * easing;
    const liveLevel = clamp(
      Math.max(0, smoothedAudioLevel - 0.015) / 0.92,
      0,
      1,
    );
    const presentationLevel = isProcessingWaveMode()
      ? getProcessingWaveLevel(time)
      : liveLevel;
    const liveSpectrum = currentAudioSpectrum.map((value) =>
      clamp(value, 0, 1),
    );
    const presentationSpectrum = isProcessingWaveMode()
      ? getProcessingSpectrum(time)
      : liveSpectrum;

    for (let i = 0; i < smoothedSpectrum.length; i += 1) {
      const targetSpectrum = presentationSpectrum[i] ?? 0;
      const easingSpectrum = targetSpectrum > smoothedSpectrum[i] ? 0.42 : 0.16;
      smoothedSpectrum[i] +=
        (targetSpectrum - smoothedSpectrum[i]) * easingSpectrum;
      smoothedSpectrum[i] = clamp(smoothedSpectrum[i], 0, 1);
    }

    const scrollSpeed = isProcessingWaveMode()
      ? WAVEFORM_PROCESSING_SCROLL_SPEED
      : WAVEFORM_HISTORY_SCROLL_SPEED;
    waveformShiftAccumulator += dt * scrollSpeed;
    const shift = Math.floor(waveformShiftAccumulator);
    if (shift > 0) {
      waveformShiftAccumulator -= shift;
      shiftWaveformHistory(shift, presentationLevel);
      shiftHybridSpectrumHistory(shift, smoothedSpectrum);
    } else if (waveformHistory.length > 0) {
      const tailIndex = waveformHistory.length - 1;
      waveformHistory[tailIndex] +=
        (presentationLevel - waveformHistory[tailIndex]) * 0.55;
      updateHybridSpectrumHistory(smoothedSpectrum);
    }

    for (let i = 0; i < waveformHistory.length; i += 1) {
      waveformHistory[i] *= 0.996;
      if (waveformHistory[i] < 0.0015) {
        waveformHistory[i] = 0;
      }
    }
    for (let i = 0; i < hybridSpectrumHistory.length; i += 1) {
      const column = hybridSpectrumHistory[i];
      for (let j = 0; j < column.length; j += 1) {
        column[j] *= 0.997;
        if (column[j] < 0.0015) {
          column[j] = 0;
        }
      }
    }

    renderWaveformBars();
    renderHybridSpectrum();
    renderMiniWave(time, liveLevel);

    if (
      isCompactBatchMode() ||
      isStreamWaveMode() ||
      smoothedAudioLevel > 0.01 ||
      hasWaveformEnergy() ||
      hasHybridSpectrumEnergy()
    ) {
      waveformFrameId = window.requestAnimationFrame(tick);
    } else {
      waveformLastFrameTime = 0;
      waveformShiftAccumulator = 0;
    }
  };

  waveformFrameId = window.requestAnimationFrame(tick);
}

/// Recording: the live level across the five bars. Recognizing and
/// polishing: a pulse travelling through them, like the batch bars.
function renderMiniWave(time: number, liveLevel: number) {
  if (!miniWave || miniWave.hidden) return;
  const t = time / 1000;
  const processing = currentMode === "recognizing" || currentMode === "correcting";
  const speed = currentMode === "correcting" ? 8 : 6;
  miniWaveNodes.forEach((bar, index) => {
    const level = processing
      ? 0.22 + 0.42 * Math.pow(Math.max(0, Math.sin(t * speed - index * 0.9)), 1.5)
      : liveLevel *
        MINI_WAVE_SHAPE[index] *
        (0.85 + 0.15 * Math.sin(t * 17 + index * 2.1)) *
        1.25;
    bar.style.setProperty("--bar-scale", clamp(level, 0.18, 1).toFixed(3));
  });
}

function handleAudioLevelUpdate(level: number | undefined) {
  const rawLevel = clamp(level ?? 0, 0, 1);
  // Both sources report RMS of the normalized waveform, but they sit in very
  // different parts of the range: microphone input is quiet, synthesized output
  // is close to full scale. Measured over a real read, its RMS runs to a median
  // of 0.061 and a peak of 0.146 — under the microphone's gain that pins the
  // bars at full deflection for the entire utterance, which is what this fixes.
  // 2.4 puts the peak near the top and the median a little above half.
  const gain = isReadingMode() ? READING_WAVEFORM_GAIN : WAVEFORM_GAIN;
  currentAudioLevel = clamp(
    Math.sqrt(rawLevel) * gain + WAVEFORM_IDLE_FLOOR,
    0,
    1,
  );
  startWaveformLoop();
}

function handleAudioSpectrumUpdate(bands: number[] | undefined) {
  const next = Array.from({ length: HYBRID_BAND_COUNT }, (_, index) =>
    clamp(bands?.[index] ?? 0, 0, 1),
  );
  currentAudioSpectrum = next;
  startWaveformLoop();
}

function renderTranscript() {
  const showError = currentMode === "error";
  // An error is carried in the transcript slots whatever the layout.
  // A read caption is a subtitle: the mark that closes it closes nothing, so
  // it goes before the line is laid out (and before it is measured for the
  // ellipsis). Only here — what reaches the engine keeps its punctuation.
  const rawText =
    isCaptionMode() && !showError
      ? trimCaptionPunctuation(captionText)
      : partialText || lastNonEmptyText;
  // Only dictation has words still in flux.
  const pendingLength =
    isCaptionMode() || showError
      ? 0
      : partialText
        ? partialText.length - partialCommitted
        : lastNonEmptyText.length - lastNonEmptyCommitted;
  const batchWaveMode = isBatchWaveMode();
  const compactBatchMode = isCompactBatchMode() && !showError;

  setBatchLayoutMode(batchWaveMode, compactBatchMode);

  if (compactBatchMode) {
    startWaveformLoop();
    setTextAreaContent("", false);
    return;
  }

  // After the layout mode: what is showing beside the text decides how much
  // room the text has.
  updateTextAreaMaxWidth();
  const display = fitText(rawText);
  if (display) {
    // fitText keeps the end, so the pending words are still the last ones
    // shown, behind the ellipsis it may have put in front.
    const shownText = display === rawText ? display.length : display.length - 1;
    setTextAreaContent(display, false, Math.min(pendingLength, shownText));
  } else {
    setTextAreaContent(placeholderText(), true);
  }
}

/// What the text area says when there is no text. Dictation says nothing:
/// the icon is the state, and the capsule stays a small pill until words
/// arrive. A read's caption names its phase, because before the first
/// sentence there is nothing else on screen.
function placeholderText() {
  if (currentMode === "error") return t("error");
  if (!isCaptionMode()) return "";
  switch (currentMode) {
    case "reading_prepare":
      return t("readingPrepare");
    case "reading_translate":
      return readingKind === "translate"
        ? t("readingTranslating")
        : t("readingPreprocessing");
    case "reading_speak":
      return t("reading");
    // The read is over with no caption to keep: it was stopped before its
    // first sentence, or its error just cleared. The window is on its way
    // out and there is nothing to announce.
    default:
      return "";
  }
}

/// `pendingLength`: how many characters at the end the recognizer may still
/// revise.
function setTextAreaContent(text: string, placeholder: boolean, pendingLength = 0) {
  if (!textArea) return;
  textArea.hidden = !text;
  if (placeholder && text) {
    const span = document.createElement("span");
    span.className = "placeholder";
    span.textContent = text;
    textArea.replaceChildren(span);
  } else if (pendingLength > 0) {
    const pending = document.createElement("span");
    pending.className = "pending";
    pending.textContent = text.slice(text.length - pendingLength);
    textArea.replaceChildren(text.slice(0, text.length - pendingLength), pending);
  } else {
    textArea.textContent = text;
  }
  updateCapsuleShape();
}

/// A dictation capsule with nothing but its icon is an even pill; a caption
/// with nothing to say is not drawn at all.
function updateCapsuleShape() {
  if (!hudCapsule || !textArea) return;
  const iconOnly =
    !isCaptionMode() &&
    !document.body.classList.contains("compact-batch-mode") &&
    textArea.hidden &&
    !countdown?.textContent &&
    !intentChip?.textContent;
  hudCapsule.classList.toggle("icon-only", iconOnly);
  hudCapsule.style.visibility = isCaptionMode() && textArea.hidden ? "hidden" : "";
  fitCapsuleSize();
}

/// The widest the capsule has been in this utterance. While words stream in
/// it only grows: a revision that drops a word would otherwise shrink it and
/// grow it back a moment later.
let capsuleFloorWidth = 0;
/// The next size is set, not eased: the window has just been shown, and the
/// capsule comes in at its size.
let capsuleSnapNext = true;

/// Ease the capsule's size toward its content, so it grows the way a sentence
/// does rather than stepping word by word. CSS cannot transition to an
/// automatic size: the target is measured at auto, then both ends are set.
/// Captions keep automatic sizing — a new sentence replaces the old one
/// whole, and a box easing around it would cut it off.
function fitCapsuleSize() {
  if (!hudCapsule) return;
  if (isCaptionMode()) {
    hudCapsule.style.width = "";
    hudCapsule.style.height = "";
    return;
  }
  const current = getComputedStyle(hudCapsule);
  const fromWidth = parseFloat(current.width);
  const fromHeight = parseFloat(current.height);

  hudCapsule.classList.add("sizing");
  hudCapsule.style.width = "";
  hudCapsule.style.height = "";
  let toWidth = hudCapsule.offsetWidth;
  const toHeight = hudCapsule.offsetHeight;

  const streaming =
    isStreamWaveMode() && currentMode !== "idle" && !!textArea && !textArea.hidden;
  if (streaming) toWidth = Math.max(toWidth, capsuleFloorWidth);
  capsuleFloorWidth = streaming ? toWidth : 0;

  if (!capsuleSnapNext && !reduceMotion) {
    hudCapsule.style.width = `${fromWidth}px`;
    hudCapsule.style.height = `${fromHeight}px`;
    // Commit the start before easing away from it.
    void hudCapsule.offsetWidth;
  }
  capsuleSnapNext = false;
  hudCapsule.classList.remove("sizing");
  hudCapsule.style.width = `${toWidth}px`;
  hudCapsule.style.height = `${toHeight}px`;
}

function handleTranscriptUpdate(
  text: string | undefined,
  isFinal: boolean,
  clear = false,
  committedLength = 0,
) {
  if (clear) {
    partialText = "";
    lastNonEmptyText = "";
    partialCommitted = 0;
    lastNonEmptyCommitted = 0;
    renderTranscript();
    return;
  }

  const raw = text || "";
  const trimmed = raw.trim();
  // The committed length counts from the start of the untrimmed text.
  const leading = raw.length - raw.trimStart().length;
  const committed = isFinal
    ? trimmed.length
    : clamp(committedLength - leading, 0, trimmed.length);

  if (isCompactBatchMode()) {
    partialText = "";
    lastNonEmptyText = "";
    renderTranscript();
    return;
  }

  if (!trimmed && partialText) {
    renderTranscript();
    return;
  }

  if (currentMode === "correcting" && !trimmed) {
    renderTranscript();
    return;
  }

  partialText = trimmed;
  partialCommitted = committed;
  if (trimmed) {
    lastNonEmptyText = trimmed;
    lastNonEmptyCommitted = committed;
  }
  renderTranscript();
}

function handleErrorUpdate(message: string | undefined) {
  const trimmed = (message || "").trim();

  if (!trimmed) {
    if (currentMode === "error") {
      updateStatus("idle");
    }
    partialText = "";
    lastNonEmptyText = "";
    partialCommitted = 0;
    lastNonEmptyCommitted = 0;
    renderTranscript();
    return;
  }

  partialText = trimmed;
  lastNonEmptyText = trimmed;
  partialCommitted = trimmed.length;
  lastNonEmptyCommitted = trimmed.length;
  updateStatus("error");
  renderTranscript();
}

function updateCountdown(seconds?: number | null) {
  if (!countdown) return;
  if (seconds === null || seconds === undefined) {
    countdown.textContent = "";
  } else {
    const minutes = Math.floor(seconds / 60);
    const secs = seconds % 60;
    countdown.textContent = `${minutes}:${secs.toString().padStart(2, "0")}`;
  }
  countdown.classList.toggle("urgent", seconds != null && seconds <= 10);
  // Appearing or going, it changes the room the text has.
  renderTranscript();
}

function updateIntent(intent?: string) {
  currentIntent = intent === "translate_en" ? "translate_en" : "assistant";
  renderIntentChip();
  renderTranscript();
}

/// The chip labels what the HUD is currently doing.
///
/// While reading it must not show a dictation intent: "助手" is left over from
/// whatever the last dictation did, and reading has no intent of that kind.
/// Dictation's default intent shows no chip at all; only translation, which
/// changes what gets typed, is worth the space.
function renderIntentChip() {
  if (!intentChip) return;

  if (isReadingMode()) {
    if (readingKind === "translate") {
      intentChip.textContent = t(
        readingSource === "clipboard" ? "translateClipboardChip" : "translateReadChip",
      );
      intentChip.classList.add("translate");
    } else {
      intentChip.textContent = t(
        readingSource === "clipboard" ? "readingClipboardChip" : "readingChip",
      );
      intentChip.classList.remove("translate");
    }
    return;
  }

  if (currentIntent === "translate_en") {
    intentChip.textContent = t("translateEn");
    intentChip.classList.add("translate");
  } else {
    intentChip.textContent = "";
    intentChip.classList.remove("translate");
  }
}

showIcon("mic");
updateTextAreaMaxWidth();
renderTranscript();
updateIntent("assistant");
handleAudioLevelUpdate(0);

/// The capsule is fading out (`hud:exit`). The session behind it has already
/// reset, and what it reports now belongs to no visible capsule: a failed
/// recording's error would turn back into a waveform as it fades. Those
/// events are held, the latest of each kind, and applied when the window is
/// next shown, before its presentation.
let exiting = false;
const heldEvents = new Map<string, () => void>();
/// Events that start or end an exit are never held.
const EXIT_EVENTS = new Set(["hud:exit", "state:hud_presentation"]);

/// The window has just been shown: bring the capsule in from below. The
/// class comes off on the next frame, which is the first one painted.
function enterHud() {
  document.body.classList.remove("hud-exit");
  if (reduceMotion) return;
  document.body.classList.add("hud-enter");
  requestAnimationFrame(() => {
    requestAnimationFrame(() => document.body.classList.remove("hud-enter"));
  });
}

async function initListeners() {
  const unsubs: Array<() => void> = [];
  const add = async <T>(
    name: string,
    handler: (event: { payload: T }) => void,
  ) => {
    const unsub = await listen<T>(name, (event) => {
      if (exiting && !EXIT_EVENTS.has(name)) {
        heldEvents.delete(name);
        heldEvents.set(name, () => handler(event));
        return;
      }
      handler(event);
    });
    unsubs.push(unsub);
  };

  await add(
    "state:recording_style",
    (event: { payload?: { style?: string; batch?: boolean } }) => {
      const style = event.payload?.style;
      const wasIdle = currentMode === "idle";

      if (style === "push_to_talk") {
        updateStatus("push_to_talk");
      } else if (style === "hands_free") {
        updateStatus("hands_free");
      } else {
        if (!isCompactBatchMode()) {
          updateStatus("idle");
        } else {
          renderTranscript();
        }
      }

      if (wasIdle && (style === "push_to_talk" || style === "hands_free")) {
        resetTranscript();
      }
    },
  );

  await add(
    "state:recognizing",
    (event: { payload?: { is_recognizing?: boolean } }) => {
      if (event.payload?.is_recognizing) {
        updateStatus("recognizing");
      } else if (currentMode === "recognizing") {
        updateStatus("idle");
      }
    },
  );

  await add(
    "state:correcting",
    (event: { payload?: { is_correcting?: boolean } }) => {
      if (event.payload?.is_correcting) {
        updateStatus("correcting");
      } else if (currentMode === "correcting") {
        updateStatus("idle");
      }
    },
  );

  await add("state:intent", (event: { payload?: { intent?: string } }) => {
    updateIntent(event.payload?.intent);
  });

  await add("state:error", (event: { payload?: { message?: string } }) => {
    currentErrorCode = event.payload?.message ?? null;
    // Reading reports a stable code; dictation reports a human message. Map
    // what we recognise and pass anything else through untouched.
    handleErrorUpdate(errorCodeText() ?? event.payload?.message);
  });

  await add(
    "state:reading",
    (event: { payload?: { phase?: string; kind?: string; source?: string } }) => {
    const phase = event.payload?.phase;
    // The kind arrives with every phase of a session; it is set before the
    // status so the chip drawn by updateStatus is already the right one.
    readingKind = event.payload?.kind === "translate" ? "translate" : "read";
    readingSource = event.payload?.source === "clipboard" ? "clipboard" : "selection";
    if (phase === "preparing") {
      updateStatus("reading_prepare");
    } else if (phase === "translating") {
      updateStatus("reading_translate");
    } else if (phase === "speaking") {
      updateStatus("reading_speak");
    } else if (isReadingMode()) {
      // Only clear a mode we own. A read ending after dictation already took
      // over must not wipe the dictation state off the HUD.
      updateStatus("idle");
    }
    renderIntentChip();
    renderTranscript();
    },
  );

  await add(
    "state:caption",
    (event: { payload?: { text?: string | null } }) => {
      captionText = event.payload?.text ?? "";
      if (isCaptionMode()) {
        renderTranscript();
      }
    },
  );

  await add(
    "asr:event",
    (event: {
      payload?: { text?: string; isFinal?: boolean; clear?: boolean; committedLength?: number };
    }) => {
      const { text, isFinal, clear, committedLength } = event.payload || {};
      handleTranscriptUpdate(text, !!isFinal, !!clear, committedLength ?? 0);
    },
  );

  await add("state:inserted", () => {
    inserted = true;
    statusIcon?.classList.remove("animating");
    renderTranscript();
  });

  // window.rs orders the window out once the capsule has faded.
  await add("hud:exit", () => {
    exiting = true;
    document.body.classList.add("hud-exit");
  });

  await add("state:countdown", (event: { payload?: { seconds?: number } }) => {
    updateCountdown(event.payload?.seconds);
  });

  await add("state:audio_level", (event: { payload?: { level?: number } }) => {
    const wasFlowing = hasRecentLevel();
    lastLevelAt = performance.now();
    handleAudioLevelUpdate(event.payload?.level);
    // The first level of a read is what makes the bars appear.
    if (!wasFlowing && isReadingMode()) {
      applyWaveformVisibility(currentCompactBatchMode());
    }
  });

  await add(
    "state:audio_spectrum",
    (event: { payload?: { bands?: number[] } }) => {
      handleAudioSpectrumUpdate(event.payload?.bands);
    },
  );

  await add("recognition:stopped", () => {
    if (!isCompactBatchMode()) {
      updateStatus("idle");
    } else {
      renderTranscript();
    }
  });

  await add(
    "state:hud_presentation",
    (event: { payload?: { mode?: string } }) => {
      // Sent with every show: a new session starts here.
      exiting = false;
      const held = [...heldEvents.values()];
      heldEvents.clear();
      held.forEach((apply) => apply());
      const mode = event.payload?.mode;
      hudPresentation =
        mode === "batch" ? "batch" : mode === "caption" ? "caption" : "stream";
      if (!isCompactBatchMode()) {
        currentAudioLevel = 0;
        smoothedAudioLevel = 0;
      }
      inserted = false;
      capsuleSnapNext = true;
      capsuleFloorWidth = 0;
      enterHud();
      renderTranscript();
    },
  );

  await add<{ locale?: "zh-CN" | "en-US" }>("ui:locale-changed", (event) => {
    setHudLocale(event.payload?.locale);
  });

  await add<{ transparent?: boolean }>("hud:appearance", (event) => {
    setHudTranslucent(Boolean(event.payload?.transparent));
  });

  try {
    const settings = await invoke<{ hudTransparent?: boolean }>("get_settings");
    setHudTranslucent(Boolean(settings?.hudTransparent));
  } catch (err: unknown) {
    console.error("[HUD] Failed to load appearance:", err);
  }

  window.addEventListener("beforeunload", () => {
    if (waveformFrameId) {
      window.cancelAnimationFrame(waveformFrameId);
    }
    textMeasureEl?.remove();
    unsubs.forEach((fn) => fn && fn());
  });

  await invoke("hud_ready");

  // State snapshot has been replayed; reveal the HUD.
  document.body.classList.remove("hud-loading");
}

invoke<"zh-CN" | "en-US">("get_resolved_ui_locale")
  .then((locale: "zh-CN" | "en-US") => {
    setHudLocale(locale);
  })
  .catch((err: unknown) => {
    console.error("[HUD] Failed to resolve locale:", err);
    setHudLocale("en-US");
  })
  .finally(() => {
    initListeners().catch((err) => {
      console.error("[HUD] Failed to initialize:", err);
    });
  });
