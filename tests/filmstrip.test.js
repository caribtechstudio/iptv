import test from 'node:test';
import assert from 'node:assert/strict';
import { frameCount, nearestFrame, stripTimes } from '../src/filmstrip.js';

test('frames are taken in the middle of equal slices', () => {
  assert.deepEqual(stripTimes(100, 4), [12.5, 37.5, 62.5, 87.5]);
  assert.deepEqual(stripTimes(0, 4), []);
  assert.deepEqual(stripTimes(Infinity, 4), [], "a live stream has no plan");
  const short = stripTimes(1, 10);
  assert.ok(short.every((time) => time >= 0 && time <= 0.5));
});

test('the number of frames follows the strip width and the video shape', () => {
  assert.equal(frameCount(820, 46, 16 / 9), 10);
  assert.equal(frameCount(820, 46, 4 / 3), 13);
  assert.equal(frameCount(50), 4);
  assert.equal(frameCount(20000), 40);
  assert.equal(frameCount(820, 46, NaN), 10);
});

test('the preview uses the closest loaded frame', () => {
  const frames = [{ time: 10, image: 'a' }, { time: 30, image: null }, { time: 50, image: 'c' }, undefined];
  assert.equal(nearestFrame(frames, 28).image, 'a');
  assert.equal(nearestFrame(frames, 41).image, 'c');
  assert.equal(nearestFrame([], 5), null);
});
