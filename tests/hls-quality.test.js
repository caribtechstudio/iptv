import test from 'node:test';
import assert from 'node:assert/strict';
import { isHlsMediaUrl, parseHlsQualities } from '../src/hls-quality.js';

test('lists only the resolutions announced by an HLS master playlist', () => {
  const master = `#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=2200000,CODECS="mp4a.40.2,avc1.64001f",RESOLUTION=1280x720
low/playlist.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=6200000,RESOLUTION=1920x1080
hi/playlist.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=14000000,RESOLUTION=3840x2160
4k/playlist.m3u8`;
  const variants = parseHlsQualities(master, 'https://cdn.example.com/live/master.m3u8?token=abc');
  assert.deepEqual(variants.map(({ label, selectable }) => [label, selectable]), [
    ['4K (2160p)', true], ['1080p', true], ['720p', true],
  ]);
  assert.equal(variants[1].url, 'https://cdn.example.com/live/hi/playlist.m3u8');
  assert.equal(isHlsMediaUrl('https://cdn.example.com/live/master.m3u8?token=abc'), true);
  assert.equal(isHlsMediaUrl('https://cdn.example.com/video.mp4'), false);
});

test('keeps automatic quality when a variant needs a separate audio rendition', () => {
  const master = `#EXTM3U
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="stereo",NAME="French",URI="audio/fr.m3u8"
#EXT-X-STREAM-INF:BANDWIDTH=6200000,RESOLUTION=1920x1080,AUDIO="stereo"
video-only/1080.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2200000,RESOLUTION=1280x720
muxed/720.m3u8`;
  const variants = parseHlsQualities(master, 'https://cdn.example.com/master.m3u8');
  assert.equal(variants[0].selectable, false);
  assert.equal(variants[1].selectable, true);
});

test('ignores media playlists and unsafe variant addresses', () => {
  assert.deepEqual(parseHlsQualities('#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nsegment.ts', 'https://example.com/live.m3u8'), []);
  assert.deepEqual(parseHlsQualities('#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=1280x720\nfile:///private/video.m3u8', 'https://example.com/live.m3u8'), []);
  assert.deepEqual(parseHlsQualities('#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=96000,CODECS="mp4a.40.2"\naudio.m3u8', 'https://example.com/live.m3u8'), []);
});
