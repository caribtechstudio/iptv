import test from 'node:test';
import assert from 'node:assert/strict';
import { canCopyVideo, failureMessage, initialPlan, nextPlan } from '../src/playback.js';

test('chooses the first engine from the source', () => {
  assert.deepEqual(initialPlan({ url: 'https://h/live.m3u8', live: true }), { mode: 'native', live: true });
  assert.equal(initialPlan({ url: 'https://h/live.m3u8', headers: { userAgent: 'X' } }).mode, 'relay');
  assert.equal(initialPlan({ url: 'http://h/live/u/p/12.ts', live: true }).mode, 'segmenter');
  assert.equal(initialPlan({ url: 'http://h/udp/239.0.0.1:1234' }).mode, 'segmenter');
  assert.equal(initialPlan({ url: 'file:///Movies/rec.ts' }).mode, 'segmenter');
  assert.equal(initialPlan({ url: 'file:///Movies/film.mkv', ffmpeg: true }).mode, 'transcode');
  assert.equal(initialPlan({ url: 'file:///Movies/film.mkv', ffmpeg: false }).mode, 'native');
});

test('escalates according to the probe and stops on dead sources', () => {
  const tried = new Set(['native']);
  assert.equal(nextPlan({ probe: { engine: 'none' }, tried, url: 'https://h/a.m3u8' }), null);
  assert.equal(nextPlan({ probe: { engine: 'segmenter' }, tried, url: 'https://h/a' }).mode, 'segmenter');
  assert.equal(nextPlan({ probe: { engine: 'native' }, tried, url: 'https://h/a.m3u8' }).mode, 'relay');
  assert.equal(nextPlan({ probe: { engine: 'transcode' }, tried: new Set(['native', 'relay']), url: 'https://h/a', ffmpeg: false }), null);
  assert.equal(nextPlan({ probe: { engine: 'transcode' }, tried: new Set(['native', 'relay']), url: 'https://h/a', ffmpeg: true }).mode, 'transcode');
  assert.match(failureMessage({ engine: 'transcode', message: 'MPEG-2.' }, false), /brew install ffmpeg/);
  assert.equal(canCopyVideo({ video: ['h264'], unsupported: ['aac-latm'] }), true);
  assert.equal(canCopyVideo({ video: ['hevc'], unsupported: ['hevc'] }), false);
});
