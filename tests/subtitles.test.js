import test from 'node:test';
import assert from 'node:assert/strict';
import { parseSubtitles } from '../src/subtitles.js';

test('reads SRT and VTT timing and keeps multiline text', () => {
  const srt = '1\r\n00:00:01,500 --> 00:00:03,000\r\nBonjour\r\nle monde\r\n\r\n2\r\n00:00:04,000 --> 00:00:05,000\r\nFin';
  assert.deepEqual(parseSubtitles(srt), [
    { start: 1.5, end: 3, text: 'Bonjour\nle monde' },
    { start: 4, end: 5, text: 'Fin' },
  ]);
  const vtt = 'WEBVTT\n\n00:01.000 --> 00:02.500 align:center\n<c>Bonjour</c>';
  assert.deepEqual(parseSubtitles(vtt), [{ start: 1, end: 2.5, text: 'Bonjour' }]);
});

test('rejects invalid or reversed cues', () => {
  assert.deepEqual(parseSubtitles('00:00:05,000 --> 00:00:04,000\nInvalid'), []);
});
