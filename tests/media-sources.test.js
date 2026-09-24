import test from 'node:test';
import assert from 'node:assert/strict';
import { isWebUrl, isYouTubePage } from '../src/media-sources.js';

test('routes YouTube channel pages to the official player, but keeps streams in Fluxo', () => {
  assert.equal(isYouTubePage('https://www.youtube.com/euronewsfr/live'), true);
  assert.equal(isYouTubePage('https://youtu.be/abc123'), true);
  assert.equal(isYouTubePage('https://youtube.com.evil.example/live'), false);
  assert.equal(isYouTubePage('https://example.org/live.m3u8'), false);
  assert.equal(isWebUrl('xtream://private-stream'), false);
});
