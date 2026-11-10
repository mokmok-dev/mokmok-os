export type ThemeMode = 'dark' | 'light';

export const THEME_KEY = 'mokmok.theme';

export function readTheme(storage: Storage): ThemeMode {
  return storage.getItem(THEME_KEY) === 'light' ? 'light' : 'dark';
}

export function writeTheme(storage: Storage, mode: ThemeMode): void {
  storage.setItem(THEME_KEY, mode);
}

export function applyTheme(mode: ThemeMode): void {
  document.documentElement.dataset.mode = mode;
}
