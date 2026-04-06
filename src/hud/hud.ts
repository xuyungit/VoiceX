// VoiceX HUD Logic (TypeScript source)
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import zhCN from "../i18n/locales/zh-CN";
import enUS from "../i18n/locales/en-US";

const statusIcon = document.getElementById("statusIcon");
const countdown = document.getElementById("countdown");
const textArea = document.getElementById("textArea");
const intentChip = document.getElementById("intentChip");
const waveformBars = document.getElementById("waveformBars");
const waveformHybridCanvas = document.getElementById(
  "waveformHybridCanvas",
) as HTMLCanvasElement | null;
const titleElement = document.querySelector("title");

if (!statusIcon || !countdown || !textArea || !intentChip || !waveformBars) {
  console.error("[HUD] Missing required DOM elements", {
    statusIcon: !!statusIcon,
    countdown: !!countdown,
    textArea: !!textArea,
    intentChip: !!intentChip,
    waveformBars: !!waveformBars,
  });
}

const BATCH_WAVEFORM_STYLE: "timeline" | "hybrid" = "timeline";
const WAVEFORM_BAR_COUNT = 27;
const HYBRID_BAND_COUNT = 8;
const HYBRID_HISTORY_COLUMNS = 26;

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

let currentMode:
  | "idle"
  | "push_to_talk"
  | "hands_free"
  | "recognizing"
  | "correcting"
  | "error" = "idle";
let currentIntent: "assistant" | "translate_en" = "assistant";
let hudPresentation: "stream" | "batch" = "stream";
let lastActiveIcon: "mic" | "waveform" | "cloud" | "wand" = "mic";
let partialText = "";
let lastNonEmptyText = "";
let currentLocale: "zh-CN" | "en-US" = "en-US";
let currentAudioLevel = 0;
let currentAudioSpectrum = Array.from({ length: HYBRID_BAND_COUNT }, () => 0);
let smoothedAudioLevel = 0;
let waveformFrameId = 0;
let waveformLastFrameTime = 0;
let waveformShiftAccumulator = 0;
const MAX_LINES = 2;
const ELLIPSIS = "\u2026";
const WAVEFORM_ATTACK = 0.4;
const WAVEFORM_RELEASE = 0.15;
const WAVEFORM_GAIN = 6.5;
const WAVEFORM_HISTORY_SCROLL_SPEED = 24;
const WAVEFORM_PROCESSING_SCROLL_SPEED = 34;
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

function updateTextAreaMaxWidth() {
  if (!textArea) return;
  const style = getComputedStyle(textArea);
  const pl = parseFloat(style.paddingLeft) || 0;
  const pr = parseFloat(style.paddingRight) || 0;
  textAreaMaxWidth = textArea.clientWidth - pl - pr;
  if (textAreaMaxWidth <= 0) textAreaMaxWidth = 230;

  const parsedLineHeight = parseFloat(style.lineHeight);
  if (Number.isFinite(parsedLineHeight) && parsedLineHeight > 0) {
    textAreaLineHeight = parsedLineHeight;
  } else {
    const fontSize = parseFloat(style.fontSize) || 12;
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

function fitText(text: string): string {
  if (!text) return text;
  const measureEl = getTextMeasureEl();
  const maxHeight = textAreaLineHeight * MAX_LINES + 1;
  const fits = (candidate: string) => {
    measureEl.textContent = candidate;
    return measureEl.scrollHeight <= maxHeight;
  };

  if (fits(text)) return text;

  let lo = 0;
  let hi = text.length;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (fits(`${ELLIPSIS}${text.slice(-mid)}`)) {
      lo = mid;
    } else {
      hi = mid - 1;
    }
  }
  return lo > 0 ? ELLIPSIS + text.slice(-lo) : ELLIPSIS;
}

window.addEventListener("resize", () => {
  updateTextAreaMaxWidth();
  renderTranscript();
});

const icons: Record<string, Element | null | undefined> = {
  mic: statusIcon?.querySelector(".icon-mic") ?? null,
  waveform: statusIcon?.querySelector(".icon-waveform") ?? null,
  cloud: statusIcon?.querySelector(".icon-cloud") ?? null,
  wand: statusIcon?.querySelector(".icon-wand") ?? null,
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

function isCompactBatchMode() {
  return hudPresentation === "batch";
}

function setBatchLayoutMode(batchWaveMode: boolean, compactBatchMode: boolean) {
  document.body.classList.toggle("batch-wave-mode", batchWaveMode);
  document.body.classList.toggle("compact-batch-mode", compactBatchMode);

  if (textArea) {
    textArea.hidden = compactBatchMode;
  }
  if (waveformBars) {
    waveformBars.hidden =
      !compactBatchMode || BATCH_WAVEFORM_STYLE !== "timeline";
  }
  if (waveformHybridCanvas) {
    waveformHybridCanvas.hidden =
      !compactBatchMode || BATCH_WAVEFORM_STYLE !== "hybrid";
  }
}

function updateStatus(mode: typeof currentMode) {
  currentMode = mode;
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
    case "error":
      document.body.classList.add("error");
      showIcon(lastActiveIcon);
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
    const target = isCompactBatchMode() ? currentAudioLevel : 0;
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

    if (
      isCompactBatchMode() ||
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

function handleAudioLevelUpdate(level: number | undefined) {
  const rawLevel = clamp(level ?? 0, 0, 1);
  currentAudioLevel = clamp(
    Math.sqrt(rawLevel) * WAVEFORM_GAIN + WAVEFORM_IDLE_FLOOR,
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
  const rawText = partialText.trim() || lastNonEmptyText.trim();
  const showError = currentMode === "error";
  const batchWaveMode = isBatchWaveMode();
  const compactBatchMode = isCompactBatchMode() && !showError;

  setBatchLayoutMode(batchWaveMode, compactBatchMode);

  if (compactBatchMode) {
    startWaveformLoop();
  }

  if (batchWaveMode) {
    return;
  }

  if (compactBatchMode) {
    return;
  }

  let display = fitText(rawText);

  if (!display) {
    textArea?.classList.add("is-placeholder");
    const placeholder =
      currentMode === "correcting"
        ? t("processingText")
        : currentMode === "error"
          ? t("error")
        : currentMode === "recognizing"
          ? t("recognizing")
          : currentMode === "push_to_talk" || currentMode === "hands_free"
            ? t("recording")
            : t("startRecording");
    if (textArea) {
      textArea.innerHTML = `<span class="placeholder">${placeholder}</span>`;
    }
  } else {
    textArea?.classList.remove("is-placeholder");
    if (textArea) {
      textArea.textContent = display;
    }
  }
}

function handleTranscriptUpdate(
  text: string | undefined,
  _isFinal: boolean,
  clear = false,
) {
  if (clear) {
    partialText = "";
    lastNonEmptyText = "";
    renderTranscript();
    return;
  }

  const trimmed = (text || "").trim();

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
  if (trimmed) {
    lastNonEmptyText = trimmed;
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
    renderTranscript();
    return;
  }

  partialText = trimmed;
  lastNonEmptyText = trimmed;
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
}

function updateIntent(intent?: string) {
  currentIntent = intent === "translate_en" ? "translate_en" : "assistant";
  if (!intentChip) return;

  if (currentIntent === "translate_en") {
    intentChip.textContent = t("translateEn");
    intentChip.classList.add("translate");
  } else {
    intentChip.textContent = t("assistant");
    intentChip.classList.remove("translate");
  }
}

showIcon("mic");
updateTextAreaMaxWidth();
renderTranscript();
updateIntent("assistant");
handleAudioLevelUpdate(0);

async function initListeners() {
  const unsubs: Array<() => void> = [];
  const add = async <T>(
    name: string,
    handler: (event: { payload: T }) => void,
  ) => {
    const unsub = await listen<T>(name, handler);
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
    handleErrorUpdate(event.payload?.message);
  });

  await add(
    "asr:event",
    (event: { payload?: { text?: string; isFinal?: boolean; clear?: boolean } }) => {
      const { text, isFinal, clear } = event.payload || {};
      handleTranscriptUpdate(text, !!isFinal, !!clear);
    },
  );

  await add("state:countdown", (event: { payload?: { seconds?: number } }) => {
    updateCountdown(event.payload?.seconds);
  });

  await add("state:audio_level", (event: { payload?: { level?: number } }) => {
    handleAudioLevelUpdate(event.payload?.level);
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
      hudPresentation = event.payload?.mode === "batch" ? "batch" : "stream";
      if (!isCompactBatchMode()) {
        currentAudioLevel = 0;
        smoothedAudioLevel = 0;
      }
      renderTranscript();
    },
  );

  await add<{ locale?: "zh-CN" | "en-US" }>("ui:locale-changed", (event) => {
    setHudLocale(event.payload?.locale);
  });

  window.addEventListener("beforeunload", () => {
    if (waveformFrameId) {
      window.cancelAnimationFrame(waveformFrameId);
    }
    textMeasureEl?.remove();
    unsubs.forEach((fn) => fn && fn());
  });
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
