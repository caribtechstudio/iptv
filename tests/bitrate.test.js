import test from 'node:test';
import assert from 'node:assert/strict';
import { RateMeter, formatBitrate, formatBytes } from '../src/bitrate.js';

test('averages a byte counter over the window, bursts included', () => {
  const meter = new RateMeter(12000);
  meter.counter(0, 0);
  assert.equal(meter.bitsPerSecond, null);
  // A 3 MB segment every 6 s: 4 Mb/s on average although most samples see nothing.
  let bytes = 0;
  for (let second = 1; second <= 24; second += 1) {
    if (second % 6 === 0) bytes += 3_000_000;
    meter.counter(bytes, second * 1000);
  }
  // Measured between bursts: exact whatever the phase of the window.
  for (let second = 25; second <= 36; second += 1) {
    if (second % 6 === 0) bytes += 3_000_000;
    meter.counter(bytes, second * 1000);
    assert.equal(Math.round(meter.bitsPerSecond), 4e6, `at ${second} s`);
  }
  assert.equal(meter.total, 18_000_000);
});

test('a counter that restarts keeps the total and does not go negative', () => {
  const meter = new RateMeter();
  meter.counter(1_000_000, 0);
  meter.counter(2_000_000, 1000);
  meter.counter(500_000, 2000);
  meter.counter(1_500_000, 3000);
  assert.equal(meter.total, 2_000_000);
  assert.ok(meter.bitsPerSecond > 0);
});

test('integrates instantaneous rates', () => {
  const meter = new RateMeter();
  for (let second = 0; second <= 10; second += 1) meter.rate(8e6, second * 1000);
  assert.equal(Math.round(meter.bitsPerSecond), 8e6);
  assert.equal(Math.round(meter.total), 10_000_000);
  meter.reset();
  assert.equal(meter.bitsPerSecond, null);
  assert.equal(meter.total, 0);
});

test('formats rates and volumes in French units', () => {
  assert.equal(formatBitrate(0), '—');
  assert.equal(formatBitrate(850_000), '850 kb/s');
  assert.equal(formatBitrate(4_240_000), '4,2 Mb/s');
  assert.equal(formatBitrate(25_400_000), '25 Mb/s');
  assert.equal(formatBytes(2_500_000), '2,5 Mo');
  assert.equal(formatBytes(312_000_000), '312 Mo');
  assert.equal(formatBytes(1_250_000_000), '1,25 Go');
});
