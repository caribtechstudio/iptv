// Filmstrip (« plan » of a video), in the manner of QuickTime: a bar over the bottom of the
// picture showing evenly spaced frames, with a playhead. Clicking or dragging along it seeks.
// Frames come from FFmpeg (`video_thumbnails`) and appear one by one; they are kept per
// source so reopening the strip, or hovering the seek bar, shows them at once.

import { iconButton, setIcon } from './icons.js';

export const FRAME_HEIGHT = 46;

/** Positions of `count` frames: the middle of each slice of the video. */
export function stripTimes(duration, count) {
  if (!Number.isFinite(duration) || duration <= 0 || !(count >= 1)) return [];
  return Array.from({ length: count }, (_, index) => Math.max(0, Math.min(duration - 0.5, (index + 0.5) * duration / count)));
}

/** Number of frames filling `width` pixels with frames of the video's shape. */
export function frameCount(width, height = FRAME_HEIGHT, aspect = 16 / 9) {
  const ratio = Number.isFinite(aspect) && aspect > 0.3 && aspect < 4 ? aspect : 16 / 9;
  return Math.max(4, Math.min(40, Math.round(width / (height * ratio))));
}

/** The loaded frame closest to `time`, or null. */
export function nearestFrame(frames, time) {
  let best = null;
  for (const frame of frames) {
    if (!frame?.image) continue;
    if (!best || Math.abs(frame.time - time) < Math.abs(best.time - time)) best = frame;
  }
  return best;
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

/**
 * host: the video area. deps: { invoke, listen, media(), source() → { source, headers, local }
 * or null, timeText(seconds), onToggle(open), onError() when no picture could be taken }.
 */
export function createFilmstrip(host, deps) {
  const bar = el('div', 'filmstrip');
  bar.hidden = true;
  bar.setAttribute('role', 'group');
  bar.setAttribute('aria-label', 'Plan de la vidéo');
  const play = iconButton('play', 'Lecture / pause', 'filmstrip-play', () => {
    const media = deps.media();
    if (media.paused) media.play()?.catch?.(() => {}); else media.pause();
  });
  const track = el('div', 'filmstrip-track');
  track.tabIndex = 0;
  track.setAttribute('role', 'slider');
  track.setAttribute('aria-label', 'Position dans la vidéo');
  const frames = el('div', 'filmstrip-frames');
  const playhead = el('div', 'filmstrip-playhead');
  const hover = el('div', 'filmstrip-hover');
  const hoverTime = el('span', 'filmstrip-time');
  hover.append(hoverTime);
  track.append(frames, playhead, hover);
  const done = el('button', 'filmstrip-done', 'OK');
  done.title = 'Fermer le plan (Échap)';
  bar.append(play, track, done);
  host.append(bar);

  const cache = new Map();
  let current = null; // { key, frames: [{ time, image }] }
  let request = 0;
  let dragging = false;

  deps.listen('thumbnail', (event) => {
    const picture = event.payload;
    for (const entry of cache.values()) {
      if (entry.request !== picture.request) continue;
      entry.frames[picture.index] = { time: picture.time, image: picture.image };
      if (picture.done) entry.complete = true;
      if (entry === current) {
        drawFrame(picture.index);
        if (entry.complete && !entry.frames.some((frame) => frame?.image)) deps.onError?.();
      }
    }
  }).catch(() => {});

  function duration() { const value = deps.media().duration; return Number.isFinite(value) && value > 0 ? value : 0; }

  function drawFrame(index) {
    const cell = frames.children[index];
    const frame = current?.frames[index];
    if (!cell || !frame) return;
    cell.classList.remove('loading');
    if (frame.image) cell.style.backgroundImage = `url("${frame.image}")`;
    else cell.classList.add('missing');
  }

  function aspect() {
    const media = deps.media();
    return media.videoWidth && media.videoHeight ? media.videoWidth / media.videoHeight : 16 / 9;
  }

  function countFor(width) { return frameCount(width, FRAME_HEIGHT, aspect()); }

  /** Frames of `info.source` for `count` slices: cached, or requested from FFmpeg. */
  function entryFor(info, count) {
    const key = `${info.source}|${count}`;
    const cached = cache.get(key);
    // An unfinished entry whose request was replaced by a newer one will never complete.
    if (cached && (cached.complete || cached.request === request)) return cached;
    const times = stripTimes(duration(), count);
    request = Date.now() + Math.floor(Math.random() * 1000);
    const entry = { key, request, frames: times.map((time) => ({ time, image: undefined })), complete: false, source: info.source };
    cache.set(key, entry);
    while (cache.size > 6) cache.delete(cache.keys().next().value);
    deps.invoke('video_thumbnails', { request: { request, source: info.source, times, height: Math.round(FRAME_HEIGHT * Math.min(2, window.devicePixelRatio || 1)), headers: info.headers || {} } })
      .catch(() => {
        entry.complete = true;
        entry.frames.forEach((frame) => { if (frame.image === undefined) frame.image = null; });
        if (entry === current) entry.frames.forEach((_, index) => drawFrame(index));
      });
    return entry;
  }

  /** Shows the frames of the current source for the strip width. */
  function load() {
    const info = deps.source();
    if (!info || !duration()) return false;
    const entry = entryFor(info, countFor(track.clientWidth || host.clientWidth * 0.75));
    current = entry;
    frames.replaceChildren(...entry.frames.map(() => el('div', 'filmstrip-frame loading')));
    entry.frames.forEach((frame, index) => { if (frame.image !== undefined) drawFrame(index); });
    return true;
  }

  function timeAt(clientX) {
    const rect = track.getBoundingClientRect();
    const ratio = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
    return ratio * duration();
  }

  function seek(time) {
    const media = deps.media();
    if (!duration()) return;
    media.currentTime = Math.max(0, Math.min(duration() - 0.1, time));
    update();
  }

  function update() {
    if (bar.hidden) return;
    const total = duration();
    const time = deps.media().currentTime || 0;
    playhead.style.left = `${total ? Math.min(100, time / total * 100) : 0}%`;
    track.setAttribute('aria-valuetext', `${deps.timeText(time)} sur ${deps.timeText(total)}`);
    setIcon(play, deps.media().paused ? 'play' : 'pause');
  }

  track.addEventListener('pointerdown', (event) => {
    if (event.button !== 0) return;
    dragging = true;
    track.setPointerCapture(event.pointerId);
    seek(timeAt(event.clientX));
  });
  track.addEventListener('pointermove', (event) => {
    const time = timeAt(event.clientX);
    const rect = track.getBoundingClientRect();
    hover.style.left = `${Math.max(0, Math.min(rect.width, event.clientX - rect.left))}px`;
    hoverTime.textContent = deps.timeText(time);
    hover.classList.add('visible');
    if (dragging) seek(time);
  });
  const stopDrag = () => { dragging = false; };
  track.addEventListener('pointerup', stopDrag);
  track.addEventListener('pointercancel', stopDrag);
  track.addEventListener('pointerleave', () => hover.classList.remove('visible'));
  track.addEventListener('keydown', (event) => {
    const step = event.shiftKey ? 30 : 5;
    if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
      event.preventDefault(); event.stopPropagation();
      seek((deps.media().currentTime || 0) + (event.key === 'ArrowRight' ? step : -step));
    } else if (event.key === 'Home' || event.key === 'End') {
      event.preventDefault(); event.stopPropagation();
      seek(event.key === 'Home' ? 0 : duration());
    }
  });

  function open() {
    if (!deps.source() || !duration()) return false;
    bar.hidden = false;
    host.classList.add('filmstrip-open');
    if (!load()) { close(); return false; }
    update();
    track.focus({ preventScroll: true });
    deps.onToggle?.(true);
    return true;
  }
  function close() {
    if (bar.hidden) return;
    bar.hidden = true;
    host.classList.remove('filmstrip-open');
    if (current && !current.complete) deps.invoke('cancel_thumbnails').catch(() => {});
    // An unfinished strip is loaded again next time.
    if (current && !current.complete) cache.delete(current.key);
    current = null;
    deps.onToggle?.(false);
  }
  done.onclick = close;

  let resizeTimer = 0;
  new ResizeObserver(() => {
    if (bar.hidden) return;
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(() => { if (!bar.hidden) load(); }, 250);
  }).observe(host);

  return {
    open,
    close,
    toggle() { return bar.hidden ? open() : (close(), false); },
    update,
    get isOpen() { return !bar.hidden; },
    /** Loaded frame nearest to `time` for `source`, for the preview above the seek bar. */
    preview(source, time) {
      let best = null;
      for (const entry of cache.values()) {
        if (entry.source !== source) continue;
        const frame = nearestFrame(entry.frames, time);
        if (frame && (!best || Math.abs(frame.time - time) < Math.abs(best.time - time))) best = frame;
      }
      return best;
    },
    /** Loads frames without showing the strip (local files only: it costs no connection). */
    prefetch() {
      const info = deps.source();
      if (!bar.hidden || !info?.local || !duration()) return;
      entryFor(info, countFor(host.clientWidth * 0.75 - 120));
    },
  };
}
