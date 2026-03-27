// Platform detection - check if running on macOS
const isMacOS = navigator.platform?.toLowerCase().includes('mac') ||
  navigator.userAgent?.toLowerCase().includes('mac');

// Platform-specific display names
const ctrlDisplayName = isMacOS ? 'Control' : 'Ctrl';
const altDisplayName = isMacOS ? 'Option' : 'Alt';
const metaDisplayName = isMacOS ? 'Command' : 'Win';

export function formatHotkey(config: string | null): string | null {
  if (!config) return null;
  const parts = config.split('|');
  if (parts.length < 2) return config;

  const keyCode = Number(parts[0]);
  const modifiers = Number(parts[1]);
  const usesFn = parts[2] === '1';
  const segments: string[] = [];

  const keyIsShift = keyCode === 60 || keyCode === 56;
  const keyIsCommand = keyCode === 54 || keyCode === 55;
  const keyIsOption = keyCode === 58 || keyCode === 61;
  const keyIsControl = keyCode === 59 || keyCode === 62;

  if (usesFn) segments.push('Fn');
  if ((modifiers & 0x1000) && !keyIsControl) segments.push(ctrlDisplayName);
  if ((modifiers & 0x0800) && !keyIsOption) segments.push(altDisplayName);
  if ((modifiers & 0x0200) && !keyIsShift) segments.push('Shift');
  if ((modifiers & 0x0100) && !keyIsCommand) segments.push(metaDisplayName);
  segments.push(keyName(keyCode));
  return segments.join(' + ');
}

function keyName(code: number): string {
  if (code === 63) return 'Fn';
  if (code === 60) return 'Right Shift';
  if (code === 54) return `Right ${metaDisplayName}`;
  if (code === 62) return `Right ${ctrlDisplayName}`;
  if (code === 49) return 'Space';
  if (code === 36) return 'Return';
  if (code === 48) return 'Tab';
  if (code === 53) return 'Escape';
  if (code === 51) return 'Delete';
  if (code === 55) return metaDisplayName;
  if (code === 58) return altDisplayName;
  if (code === 61) return `Right ${altDisplayName}`;
  if (code === 59) return ctrlDisplayName;
  if (code === 56) return 'Shift';
  if (code >= 48 && code <= 57) return String.fromCharCode(code);
  if (code >= 65 && code <= 90) return String.fromCharCode(code);
  return `Key ${code}`;
}

