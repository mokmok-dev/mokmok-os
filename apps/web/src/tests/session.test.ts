import fc from 'fast-check';
import { test, expect } from 'vitest';

import {
  FALLBACK_TITLE,
  INSTRUCTION_PREVIEW_BYTES,
  MAX_TITLE_BYTES,
  compareSessions,
  firstLineOf,
  previewInstruction,
  sessionFromTask,
  timestampFromUuidV7,
  titleFromInstruction,
} from '../domain/session';
import { makeTask, uuidV7 } from './fixtures';

const encoder = new TextEncoder();

function sessionAt(id: string, millis: number | null) {
  return {
    id,
    title: id,
    instruction: id,
    status: 'completed' as const,
    outcome: null,
    workspace: '/work/a',
    createdAt: millis === null ? null : new Date(millis),
  };
}

test('title: takes the trimmed first line', () => {
  expect(titleFromInstruction('  Fix the build  \nSecond line')).toBe(
    'Fix the build',
  );
});

test('title: falls back when the first line is blank', () => {
  expect(titleFromInstruction('   \nSecond line')).toBe(FALLBACK_TITLE);
});

test('title: falls back for an empty instruction', () => {
  expect(titleFromInstruction('')).toBe(FALLBACK_TITLE);
});

test('title: keeps a carriage return out of the title', () => {
  expect(titleFromInstruction('Hello\rWorld\r\nNext')).toBe('HelloWorld');
});

test('title: takes the whole instruction when it has no newline', () => {
  expect(firstLineOf('only line')).toBe('only line');
});

test('title: ends the first line at the newline', () => {
  expect(firstLineOf('first\nsecond')).toBe('first');
});

test('title: truncates a long multibyte line at a character boundary', () => {
  const title = titleFromInstruction('漢'.repeat(100));
  expect(encoder.encode(title).length).toBeLessThanOrEqual(MAX_TITLE_BYTES);
  expect(title).toBe('漢'.repeat(66));
});

test('title: leaves a line that exactly fills the byte budget untouched', () => {
  const line = 'a'.repeat(MAX_TITLE_BYTES);
  expect(titleFromInstruction(line)).toBe(line);
});

test('title: truncates one byte over the budget', () => {
  const line = 'a'.repeat(MAX_TITLE_BYTES + 1);
  expect(titleFromInstruction(line)).toBe('a'.repeat(MAX_TITLE_BYTES));
});

test('title: property: the title is a single bounded line', () => {
  fc.assert(
    fc.property(fc.string(), (instruction) => {
      const title = titleFromInstruction(instruction);
      expect(title.length).toBeGreaterThan(0);
      expect(encoder.encode(title).length).toBeLessThanOrEqual(MAX_TITLE_BYTES);
      expect(title).not.toContain('\n');
      expect(title).not.toContain('\r');
      const firstLine = (instruction.split('\n')[0] ?? '')
        .replaceAll('\r', '')
        .trim();
      if (
        firstLine.length > 0 &&
        encoder.encode(firstLine).length <= MAX_TITLE_BYTES
      ) {
        expect(title).toBe(firstLine);
      }
    }),
  );
});

test('timestamp: parses the first 48 bits of a v7 id', () => {
  const id = uuidV7(1_760_000_000_000);
  expect(timestampFromUuidV7(id)?.getTime()).toBe(1_760_000_000_000);
});

test('timestamp: rejects a non-v7 id', () => {
  expect(
    timestampFromUuidV7('01900000-0000-4000-8000-000000000001'),
  ).toBeNull();
});

test('timestamp: rejects a malformed id', () => {
  expect(timestampFromUuidV7('not-a-uuid')).toBeNull();
  expect(
    timestampFromUuidV7('zzzzzzzz-0000-7000-8000-000000000001'),
  ).toBeNull();
  expect(
    timestampFromUuidV7('01900000-0000-7000-8000-000000000001x'),
  ).toBeNull();
  expect(
    timestampFromUuidV7('x01900000-0000-7000-8000-000000000001'),
  ).toBeNull();
});

test('timestamp: accepts uppercase hex digits', () => {
  const id = uuidV7(1_760_000_000_000).toUpperCase();
  expect(timestampFromUuidV7(id)?.getTime()).toBe(1_760_000_000_000);
});

test('timestamp: property: a generated millisecond round-trips', () => {
  fc.assert(
    fc.property(fc.integer({ min: 0, max: 2 ** 48 - 1 }), (timestamp) => {
      const parsed = timestampFromUuidV7(uuidV7(timestamp));
      expect(parsed?.getTime()).toBe(timestamp);
    }),
  );
});

test('session: copies the listing row', () => {
  const id = uuidV7(1_700_000_000_000);
  const session = sessionFromTask(
    makeTask({
      id,
      title: 'Fix the build',
      instruction: 'Make the tests pass.',
      workspace: '/work/a',
      status: 'running',
      outcome: 'working',
    }),
  );
  expect(session).toEqual({
    id,
    title: 'Fix the build',
    instruction: 'Make the tests pass.',
    status: 'running',
    outcome: 'working',
    workspace: '/work/a',
    createdAt: new Date(1_700_000_000_000),
  });
});

test('preview: keeps a short instruction unchanged', () => {
  expect(previewInstruction('hello')).toEqual({
    text: 'hello',
    truncated: false,
  });
});

test('preview: keeps an instruction exactly at the byte cap', () => {
  const text = 'a'.repeat(INSTRUCTION_PREVIEW_BYTES);
  expect(previewInstruction(text)).toEqual({ text, truncated: false });
});

test('preview: clips one byte over the cap with a marker', () => {
  const preview = previewInstruction('a'.repeat(INSTRUCTION_PREVIEW_BYTES + 1));
  expect(preview.truncated).toBe(true);
  expect(preview.text).toBe('a'.repeat(INSTRUCTION_PREVIEW_BYTES) + '…');
});

test('preview: clips a multibyte instruction at a character boundary', () => {
  const preview = previewInstruction('漢'.repeat(2000));
  expect(preview.truncated).toBe(true);
  expect(preview.text.endsWith('…')).toBe(true);
  expect(encoder.encode(preview.text.slice(0, -1)).length).toBeLessThanOrEqual(
    INSTRUCTION_PREVIEW_BYTES,
  );
});

test('session: uses no outcome when the task has none', () => {
  expect(sessionFromTask(makeTask({ outcome: null })).outcome).toBeNull();
});

test('session: uses a null workspace for a task without one', () => {
  expect(sessionFromTask(makeTask({ workspace: null })).workspace).toBeNull();
});

test('session: derives a title from the instruction when the title is blank', () => {
  const session = sessionFromTask(
    makeTask({ title: '   ', instruction: 'Investigate the failure' }),
  );
  expect(session.title).toBe('Investigate the failure');
});

test('session: falls back to the generic title for an empty instruction', () => {
  const session = sessionFromTask(makeTask({ title: '', instruction: '' }));
  expect(session.title).toBe(FALLBACK_TITLE);
});

test('session: keeps a null timestamp for a non-v7 id', () => {
  expect(sessionFromTask(makeTask({ id: 'not-a-v7' })).createdAt).toBeNull();
});

test('compare: sorts the newest session first', () => {
  const older = sessionAt('z-old', 1_000);
  const newer = sessionAt('a-new', 2_000);
  expect([older, newer].sort(compareSessions).map((s) => s.id)).toEqual([
    'a-new',
    'z-old',
  ]);
});

test('compare: sorts a session with a timestamp before one without', () => {
  const dated = sessionAt('a-dated', 1_000);
  const undated = sessionAt('z-undated', null);
  expect([undated, dated].sort(compareSessions).map((s) => s.id)).toEqual([
    'a-dated',
    'z-undated',
  ]);
});

test('compare: breaks a timestamp tie by descending id', () => {
  const low = sessionAt(uuidV7(1_000, '8000-000000000001'), 1_000);
  const high = sessionAt(uuidV7(1_000, '8000-000000000002'), 1_000);
  expect([low, high].sort(compareSessions).map((s) => s.id)).toEqual([
    high.id,
    low.id,
  ]);
});

test('compare: returns zero for the same session', () => {
  const session = sessionAt('same', 1_000);
  expect(compareSessions(session, session)).toBe(0);
});

test('compare: treats two missing timestamps as a tie', () => {
  const session = sessionAt('same', null);
  expect(compareSessions(session, session)).toBe(0);
});
