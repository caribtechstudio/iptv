import test from 'node:test';
import assert from 'node:assert/strict';
import { MpvMedia, parseTracks } from '../src/mpv-media.js';

function fakeEngine() {
  const calls = [];
  let handler = null;
  return {
    calls,
    emit: (payload) => handler({ payload }),
    deps: {
      invoke: async (command, args) => { calls.push([command, args]); return null; },
      listen: (name, callback) => { handler = callback; return Promise.resolve(() => {}); },
    },
  };
}

test('parses mpv track lists', () => {
  const tracks = parseTracks('[{"id":1,"type":"video"},{"id":1,"type":"audio","lang":"fr","selected":true},{"id":2,"type":"sub","title":"Forcés"},{"type":"other"}]');
  assert.deepEqual(tracks.map((track) => track.type), ['video', 'audio', 'sub']);
  assert.deepEqual(parseTracks('not json'), []);
});

test('behaves like a media element for the player', async () => {
  const engine = fakeEngine();
  const media = new MpvMedia('main', engine.deps);
  const events = [];
  for (const type of ['playing', 'timeupdate', 'ended', 'error', 'volumechange']) media.addEventListener(type, () => events.push(type));
  await media.open('https://h/film.mkv', { headers: { userAgent: 'X' }, start: 42 });
  assert.equal(media.getAttribute('src'), 'https://h/film.mkv');
  assert.deepEqual(engine.calls.find(([command]) => command === 'mpv_load')[1], { surface: 'main', url: 'https://h/film.mkv', headers: { userAgent: 'X' }, start: 42 });

  // The previous file's failure, reported before this one starts, is ignored.
  engine.emit({ surface: 'main', kind: 'end', value: false, error: 'loading failed' });
  engine.emit({ surface: 'main', kind: 'start' });
  engine.emit({ surface: 'other', kind: 'restart' });
  engine.emit({ surface: 'main', kind: 'restart' });
  engine.emit({ surface: 'main', kind: 'property', name: 'time-pos', value: 43.5 });
  engine.emit({ surface: 'main', kind: 'property', name: 'duration', value: 5400 });
  engine.emit({ surface: 'main', kind: 'property', name: 'track-list', value: '[{"id":3,"type":"audio","lang":"en"},{"id":4,"type":"sub","lang":"fr","selected":true}]' });
  assert.equal(media.currentTime, 43.5);
  assert.equal(media.duration, 5400);
  assert.equal(media.audioTracks.length, 1);
  assert.deepEqual(media.subtitleTracks(), [{ id: 4, label: 'FR', selected: true }]);

  media.volume = 0.5;
  media.audioTracks[0].enabled = true;
  media.currentTime = 100;
  const sets = engine.calls.filter(([command]) => command === 'mpv_set').map(([, args]) => `${args.name}=${args.value}`);
  assert.ok(sets.includes('volume=50') && sets.includes('aid=3'));
  assert.deepEqual(engine.calls.at(-1), ['mpv_command', { surface: 'main', args: ['seek', '100', 'absolute'] }]);

  engine.emit({ surface: 'main', kind: 'property', name: 'eof-reached', value: true });
  engine.emit({ surface: 'main', kind: 'log', name: 'error', error: 'ffmpeg: HTTP error 403 Forbidden' });
  engine.emit({ surface: 'main', kind: 'end', value: false, error: 'loading failed' });
  assert.equal(media.error.message, 'loading failed (ffmpeg: HTTP error 403 Forbidden)');
  assert.deepEqual(events.filter((type) => type !== 'timeupdate' && type !== 'volumechange'), ['playing', 'ended', 'error']);

  media.stop();
  assert.equal(media.getAttribute('src'), null);
  assert.equal(engine.calls.at(-1)[0], 'mpv_detach');
  engine.emit({ surface: 'main', kind: 'restart' });
  assert.equal(events.filter((type) => type === 'playing').length, 1);
});

test('live streams have no duration', async () => {
  const engine = fakeEngine();
  const media = new MpvMedia('mv-0', engine.deps);
  await media.open('https://h/live.m3u8', { live: true });
  engine.emit({ surface: 'mv-0', kind: 'start' });
  engine.emit({ surface: 'mv-0', kind: 'property', name: 'duration', value: 30 });
  engine.emit({ surface: 'mv-0', kind: 'property', name: 'eof-reached', value: true });
  assert.equal(media.duration, Infinity);
  assert.equal(media.ended, false);
});
