import test from 'node:test';
import assert from 'node:assert/strict';
import { gridWindow, programBox, PX_PER_MINUTE } from '../src/guide-grid.js';

test('places programmes on the timeline and clips them to the window', () => {
  const now = 1_790_272_900; // 18:01:40 UTC
  const { from, to } = gridWindow(now, 6);
  assert.equal(from % 1800, 0);
  assert.ok(from < now && now - from < 3600);
  assert.equal(to - from, 6 * 3600);
  assert.deepEqual(programBox({ start: from + 600, stop: from + 2400 }, from, to), { left: 10 * PX_PER_MINUTE, width: 30 * PX_PER_MINUTE });
  assert.deepEqual(programBox({ start: from - 600, stop: from + 600 }, from, to), { left: 0, width: 10 * PX_PER_MINUTE });
});
