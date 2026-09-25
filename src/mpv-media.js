// The mpv engine seen through the part of the HTMLMediaElement API that Fluxo uses, so the
// controls, subtitles, resume and watchdog logic work the same with both engines.
// The native video view sits below the page, over the element given to `track()`.

const TRACK_TYPES = { audio: 'audio', sub: 'sub', video: 'video' };

export function parseTracks(json) {
  try {
    const list = JSON.parse(json || '[]');
    return Array.isArray(list) ? list.filter((track) => TRACK_TYPES[track?.type]) : [];
  } catch { return []; }
}

function trackLabel(track, index) {
  const parts = [track.title, track.lang?.toUpperCase(), track.codec].filter(Boolean);
  return parts.length ? parts.join(' · ') : `Piste ${index + 1}`;
}

/** Keeps a native view aligned with `element`; `clip()` returns a rect it must stay inside. */
export function trackElement(element, send, { clip } = {}) {
  let active = false;
  let frame = 0;
  let last = '';
  const update = () => {
    frame = 0;
    if (!active) return;
    const rect = element.getBoundingClientRect();
    let bounds = rect.width >= 2 && rect.height >= 2 && element.isConnected
      ? { x: rect.left, y: rect.top, width: rect.width, height: rect.height } : null;
    const limit = bounds && clip?.();
    if (limit && (rect.left < limit.left - 1 || rect.right > limit.right + 1 || rect.top < limit.top - 1 || rect.bottom > limit.bottom + 1)) bounds = null;
    const key = JSON.stringify(bounds);
    if (key === last) return;
    last = key;
    send(bounds);
  };
  const schedule = () => { if (active && !frame) frame = requestAnimationFrame(update); };
  const resize = new ResizeObserver(schedule);
  resize.observe(element);
  window.addEventListener('resize', schedule);
  document.addEventListener('scroll', schedule, true);
  // Catches layout changes that fire no event (class toggles, panels opening…).
  const timer = setInterval(schedule, 300);
  return {
    start() { active = true; last = ''; schedule(); },
    stop() { active = false; last = ''; send(null); },
    refresh: schedule,
    destroy() {
      active = false; resize.disconnect(); clearInterval(timer);
      window.removeEventListener('resize', schedule);
      document.removeEventListener('scroll', schedule, true);
    },
  };
}

export class MpvMedia extends EventTarget {
  constructor(surface, { invoke, listen }) {
    super();
    this.surface = surface;
    this.invoke = invoke;
    this.token = 0;
    this.attached = null;
    this.tracker = null;
    this.error = null;
    this.lastLog = '';
    this.info = { hwdec: '', videoCodec: '', audioCodec: '' };
    this.state = { src: null, time: 0, duration: NaN, paused: false, volume: 1, muted: false, speed: 1, width: 0, height: 0, tracks: [], buffering: false, ended: false, live: false, started: false };
    this.unlisten = listen('mpv', (event) => this.#onEvent(event.payload));
  }

  /** Aligns the native view with `element` while something is loaded. */
  track(element, options) {
    this.tracker?.destroy();
    this.tracker = trackElement(element, (bounds) => {
      this.invoke('mpv_bounds', { surface: this.surface, bounds }).catch(() => {});
    }, options);
    if (this.state.src) this.tracker.start();
  }

  #attach() {
    this.attached ??= this.invoke('mpv_attach', { surface: this.surface }).catch((error) => { this.attached = null; throw error; });
    return this.attached;
  }

  #set(name, value) {
    return this.invoke('mpv_set', { surface: this.surface, name, value: String(value) }).catch(() => {});
  }

  #emit(type) { this.dispatchEvent(new Event(type)); }

  async open(url, { headers = {}, live = false, start = null } = {}) {
    const token = ++this.token;
    this.error = null;
    this.lastLog = '';
    // Events until mpv starts this file belong to the previous one (its end, its errors).
    this.awaitingStart = true;
    Object.assign(this.state, { src: url, time: Number(start) || 0, duration: live ? Infinity : NaN, paused: false, width: 0, height: 0, tracks: [], buffering: true, ended: false, live, started: false });
    this.#emit('emptied');
    await this.#attach();
    if (token !== this.token) return;
    this.tracker?.start();
    // The facade holds the sound settings: they survive engine switches and new instances.
    await Promise.all([this.#set('volume', Math.round(this.state.volume * 100)), this.#set('mute', this.state.muted ? 'yes' : 'no'), this.#set('speed', this.state.speed)]);
    await this.invoke('mpv_load', { surface: this.surface, url, headers, start: Number(start) > 0 ? Number(start) : null });
    if (token !== this.token) return;
    this.#emit('loadstart');
  }

  stop() {
    this.token += 1;
    if (!this.state.src) return;
    this.state.src = null;
    this.state.started = false;
    this.tracker?.stop();
    this.invoke('mpv_detach', { surface: this.surface }).catch(() => {});
    this.#emit('emptied');
  }

  destroy() {
    this.stop();
    this.tracker?.destroy();
    this.invoke('mpv_destroy', { surface: this.surface }).catch(() => {});
    this.attached = null;
    Promise.resolve(this.unlisten).then((unlisten) => unlisten?.()).catch(() => {});
  }

  #onEvent(payload) {
    if (payload?.surface !== this.surface || !this.state.src) return;
    const { state } = this;
    if (payload.kind === 'start') { this.awaitingStart = false; return; }
    if (this.awaitingStart && payload.kind !== 'property') return;
    switch (payload.kind) {
      case 'property': this.#property(payload.name, payload.value); break;
      case 'loaded': this.#emit('loadedmetadata'); break;
      case 'video': this.#emit('resize'); break;
      case 'restart':
        state.buffering = false;
        if (!state.paused) { state.started = true; this.#emit('playing'); }
        break;
      case 'end':
        if (payload.error) {
          this.error = { code: 4, message: this.lastLog ? `${payload.error} (${this.lastLog})` : payload.error };
          this.#emit('error');
        }
        break;
      case 'log': this.lastLog = String(payload.error || '').slice(0, 300); break;
      default: break;
    }
  }

  #property(name, value) {
    const { state } = this;
    switch (name) {
      case 'time-pos':
        if (typeof value === 'number') { state.time = value; this.#emit('timeupdate'); }
        break;
      case 'duration':
        state.duration = state.live ? Infinity : typeof value === 'number' && value > 0 ? value : NaN;
        this.#emit('durationchange');
        break;
      case 'pause':
        state.paused = Boolean(value);
        this.#emit(state.paused ? 'pause' : 'play');
        if (!state.paused && state.started && !state.buffering) this.#emit('playing');
        break;
      case 'paused-for-cache':
        state.buffering = Boolean(value);
        if (state.buffering) this.#emit('waiting');
        else if (!state.paused && state.started) this.#emit('playing');
        break;
      case 'eof-reached':
        // With keep-open, the end of a film is reported here rather than by `end-file`.
        if (value && !state.live && !state.ended) { state.ended = true; this.#emit('ended'); }
        break;
      case 'volume':
        if (typeof value === 'number') { state.volume = Math.max(0, Math.min(1, value / 100)); this.#emit('volumechange'); }
        break;
      case 'mute': state.muted = Boolean(value); this.#emit('volumechange'); break;
      case 'speed': if (typeof value === 'number') { state.speed = value; this.#emit('ratechange'); } break;
      case 'dwidth': state.width = Number(value) || 0; break;
      case 'dheight': state.height = Number(value) || 0; this.#emit('resize'); break;
      case 'track-list': state.tracks = parseTracks(value); this.#emit('tracks'); break;
      case 'hwdec-current': this.info.hwdec = value || ''; break;
      case 'video-codec': this.info.videoCodec = value || ''; break;
      case 'audio-codec-name': this.info.audioCodec = value || ''; break;
      default: break;
    }
  }

  // ----- HTMLMediaElement subset -----

  getAttribute(name) { return name === 'src' ? this.state.src : null; }
  get currentTime() { return this.state.time; }
  set currentTime(value) {
    if (!Number.isFinite(value)) return;
    this.state.time = value; this.state.ended = false;
    this.invoke('mpv_command', { surface: this.surface, args: ['seek', String(value), 'absolute'] }).catch(() => {});
    this.#emit('timeupdate');
  }
  get duration() { return this.state.duration; }
  get paused() { return this.state.paused; }
  get ended() { return this.state.ended; }
  get volume() { return this.state.volume; }
  set volume(value) {
    this.state.volume = Math.max(0, Math.min(1, Number(value) || 0));
    this.#set('volume', Math.round(this.state.volume * 100)); this.#emit('volumechange');
  }
  get muted() { return this.state.muted; }
  set muted(value) { this.state.muted = Boolean(value); this.#set('mute', value ? 'yes' : 'no'); this.#emit('volumechange'); }
  get playbackRate() { return this.state.speed; }
  set playbackRate(value) { this.state.speed = Number(value) || 1; this.#set('speed', this.state.speed); this.#emit('ratechange'); }
  get videoWidth() { return this.state.width; }
  get videoHeight() { return this.state.height; }

  play() {
    if (this.state.ended && !this.state.live) this.currentTime = 0;
    this.state.paused = false;
    this.#set('pause', 'no'); this.#emit('play');
    return Promise.resolve();
  }
  pause() { this.state.paused = true; this.#set('pause', 'yes'); this.#emit('pause'); }

  /** Audio tracks shaped like `HTMLMediaElement.audioTracks`: setting `enabled` selects one. */
  get audioTracks() {
    const select = (id) => this.#set('aid', id);
    return this.state.tracks.filter((track) => track.type === 'audio').map((track, index) => ({
      label: trackLabel(track, index), language: track.lang || '',
      get enabled() { return Boolean(track.selected); },
      set enabled(value) { if (value) select(track.id); },
    }));
  }

  /** Subtitle tracks embedded in the stream; mpv draws them onto the video itself. */
  subtitleTracks() {
    return this.state.tracks.filter((track) => track.type === 'sub').map((track, index) => ({ id: track.id, label: trackLabel(track, index), selected: Boolean(track.selected) }));
  }
  selectSubtitle(id) { return this.#set('sid', id == null ? 'no' : id); }
}
