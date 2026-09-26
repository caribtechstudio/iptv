// Bitrate of the stream being played. The sources differ by engine: mpv reports its network
// speed, the local relay counts the bytes it serves, WebKit counts the bytes it decodes. HLS
// arrives in bursts (a segment every few seconds): the rate is measured from one burst to the
// last one of a sliding window, so it neither jumps with each segment nor drops between them.

export const WINDOW_MS = 12000;

export class RateMeter {
  constructor(windowMs = WINDOW_MS) {
    this.windowMs = windowMs;
    this.reset();
  }

  reset() {
    /** Samples of the cumulative byte count: [time in ms, bytes]. */
    this.samples = [];
    this.total = 0;
    this.lastCounter = null;
    this.lastTime = null;
  }

  #push(now) {
    this.samples.push([now, this.total]);
    const limit = now - this.windowMs;
    // One sample older than the window is kept as the starting point of the average.
    while (this.samples.length > 2 && this.samples[1][0] <= limit) this.samples.shift();
  }

  /** Feeds a cumulative byte counter (relay session, decoded bytes). A counter that goes
   * back (new session, reloaded element) starts over from its new value. */
  counter(bytes, now) {
    if (!Number.isFinite(bytes) || bytes < 0) return;
    if (this.lastCounter !== null && bytes >= this.lastCounter) this.total += bytes - this.lastCounter;
    this.lastCounter = bytes;
    this.#push(now);
  }

  /** Feeds a rate in bits per second measured at `now` (mpv's network speed). */
  rate(bitsPerSecond, now) {
    if (!Number.isFinite(bitsPerSecond) || bitsPerSecond < 0) return;
    if (this.lastTime !== null && now > this.lastTime) {
      this.total += bitsPerSecond / 8 * Math.min(now - this.lastTime, 5000) / 1000;
    }
    this.lastTime = now;
    this.#push(now);
  }

  /** Average over the window in bits per second, or null before two samples a second apart. */
  get bitsPerSecond() {
    const { samples } = this;
    if (samples.length < 2) return null;
    // Samples where data arrived: between the first and the last of them, the bytes of every
    // burst but the first were received.
    const arrivals = [];
    for (let index = 1; index < samples.length; index += 1) {
      if (samples[index][1] > samples[index - 1][1]) arrivals.push(samples[index]);
    }
    if (arrivals.length >= 2) {
      const [firstTime, firstBytes] = arrivals[0];
      const [lastTime, lastBytes] = arrivals[arrivals.length - 1];
      if (lastTime - firstTime >= 900) return (lastBytes - firstBytes) * 8 / ((lastTime - firstTime) / 1000);
    }
    const [startTime, startBytes] = samples[0];
    const [endTime, endBytes] = samples[samples.length - 1];
    const elapsed = endTime - startTime;
    return elapsed >= 900 ? (endBytes - startBytes) * 8 / (elapsed / 1000) : null;
  }
}

const number = (value, digits) => value.toLocaleString('fr-FR', { maximumFractionDigits: digits, minimumFractionDigits: digits });

export function formatBitrate(bitsPerSecond) {
  if (!Number.isFinite(bitsPerSecond) || bitsPerSecond <= 0) return '—';
  if (bitsPerSecond >= 10e6) return `${number(bitsPerSecond / 1e6, 0)} Mb/s`;
  if (bitsPerSecond >= 1e6) return `${number(bitsPerSecond / 1e6, 1)} Mb/s`;
  return `${Math.max(1, Math.round(bitsPerSecond / 1e3))} kb/s`;
}

export function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 Mo';
  if (bytes >= 1e9) return `${number(bytes / 1e9, 2)} Go`;
  if (bytes >= 1e6) return `${number(bytes / 1e6, bytes >= 1e8 ? 0 : 1)} Mo`;
  return `${Math.max(1, Math.round(bytes / 1e3))} Ko`;
}
