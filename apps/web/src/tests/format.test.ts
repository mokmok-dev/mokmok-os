import { test, expect } from 'vitest';

import { relativeTime } from '../format';

test('format: a missing date is unknown', () => {
  expect(relativeTime(null, 1_000)).toBe('unknown');
});

test('format: under a minute is just now', () => {
  expect(relativeTime(new Date(0), 59_000)).toBe('just now');
});

test('format: a future date is just now', () => {
  expect(relativeTime(new Date(10_000), 1_000)).toBe('just now');
});

test('format: measures from the date, not from zero', () => {
  expect(relativeTime(new Date(30_000), 70_000)).toBe('just now');
  expect(relativeTime(new Date(30_000), 130_000)).toBe('1m ago');
});

test('format: sixty seconds is one minute', () => {
  expect(relativeTime(new Date(0), 60_000)).toBe('1m ago');
});

test('format: fifty-nine minutes stays in minutes', () => {
  expect(relativeTime(new Date(0), 59 * 60_000)).toBe('59m ago');
});

test('format: sixty minutes is one hour', () => {
  expect(relativeTime(new Date(0), 60 * 60_000)).toBe('1h ago');
});

test('format: twenty-three hours stays in hours', () => {
  expect(relativeTime(new Date(0), 23 * 3_600_000)).toBe('23h ago');
});

test('format: twenty-four hours is one day', () => {
  expect(relativeTime(new Date(0), 24 * 3_600_000)).toBe('1d ago');
});

test('format: several days are counted in days', () => {
  expect(relativeTime(new Date(0), 3 * 24 * 3_600_000)).toBe('3d ago');
});
