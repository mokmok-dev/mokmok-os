import { test, expect, beforeEach } from 'vitest';

import { THEME_KEY, applyTheme, readTheme, writeTheme } from '../theme';

beforeEach(() => {
  window.localStorage.clear();
  delete document.documentElement.dataset.mode;
});

test('theme: names the storage key', () => {
  expect(THEME_KEY).toBe('mokmok.theme');
});

test('theme: defaults to dark', () => {
  expect(readTheme(window.localStorage)).toBe('dark');
});

test('theme: reads a stored light mode', () => {
  window.localStorage.setItem(THEME_KEY, 'light');
  expect(readTheme(window.localStorage)).toBe('light');
});

test('theme: reads a stored dark mode', () => {
  window.localStorage.setItem(THEME_KEY, 'dark');
  expect(readTheme(window.localStorage)).toBe('dark');
});

test('theme: writes the mode', () => {
  writeTheme(window.localStorage, 'light');
  expect(window.localStorage.getItem(THEME_KEY)).toBe('light');
  expect(readTheme(window.localStorage)).toBe('light');
});

test('theme: applies the mode to the document', () => {
  applyTheme('light');
  expect(document.documentElement.dataset.mode).toBe('light');
  applyTheme('dark');
  expect(document.documentElement.dataset.mode).toBe('dark');
});
