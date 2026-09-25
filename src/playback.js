// Playback engine: picks how a source is played (WebKit directly, local relay with headers,
// MPEG-TS repackaging, FFmpeg conversion), escalates on failure, watches for frozen streams,
// and fails over to other sources of the same channel.

export const MODE_LABELS = {
  native: 'Lecture directe',
  relay: 'Relais local (en-têtes du fournisseur)',
  segmenter: 'Reconditionnement MPEG-TS → HLS',
  transcode: 'Conversion FFmpeg (compatibilité)',
};

const TS_EXTENSIONS = new Set(['ts', 'm2ts', 'mts']);
const TRANSCODE_EXTENSIONS = new Set(['mkv', 'avi', 'wmv', 'flv', 'mpg', 'mpeg', 'vob', 'divx', '3gp']);

export function urlExtension(url = '') {
  try {
    const name = new URL(url).pathname.split('/').pop() || '';
    return name.includes('.') ? name.split('.').pop().toLowerCase() : '';
  } catch { return ''; }
}

export function isLocalUrl(url = '') { return url.startsWith('file:'); }

export function hasCustomHeaders(headers = {}) { return Boolean(headers.userAgent || headers.referrer); }

export function initialPlan({ url, headers = {}, live = false, ffmpeg = false }) {
  const ext = urlExtension(url);
  if (isLocalUrl(url)) {
    if (TS_EXTENSIONS.has(ext)) return { mode: 'segmenter', live: false };
    if (TRANSCODE_EXTENSIONS.has(ext) && ffmpeg) return { mode: 'transcode', live: false };
    return { mode: 'native', live: false };
  }
  let path = '';
  try { path = new URL(url).pathname; } catch { /* invalid URLs fail in the player */ }
  if (ext === 'mpd' && ffmpeg) return { mode: 'transcode', live };
  if (TS_EXTENSIONS.has(ext) || /\/(udp|rtp)\//i.test(path)) return { mode: 'segmenter', live };
  if (TRANSCODE_EXTENSIONS.has(ext) && ffmpeg) return { mode: 'transcode', live: false };
  if (hasCustomHeaders(headers)) return { mode: 'relay', live };
  return { mode: 'native', live };
}

/** Next engine to try after a failure, or null when the source itself is unusable. */
export function nextPlan({ probe, ffmpeg = false, tried = new Set(), url = '', live = false }) {
  if (probe?.engine === 'none') return null;
  const order = [];
  if (probe?.engine === 'segmenter') order.push('segmenter');
  if (probe?.engine === 'transcode') order.push('transcode');
  if (!isLocalUrl(url)) order.push('relay');
  else if (TS_EXTENSIONS.has(urlExtension(url))) order.push('segmenter');
  order.push('transcode');
  const mode = order.find((item) => !tried.has(item) && (item !== 'transcode' || ffmpeg));
  return mode ? { mode, live } : null;
}

export function failureMessage(probe, ffmpeg, fallback, tried = new Set()) {
  if (probe?.engine === 'transcode' && !ffmpeg) {
    return `${probe.message || 'Ce flux nécessite le moteur de compatibilité.'} Installez FFmpeg (brew install ffmpeg) pour le lire.`;
  }
  // The conversion ran and failed: its own error says more than the probe's codec diagnosis.
  if (probe?.engine === 'transcode' && tried.has('transcode') && fallback) return fallback;
  return probe?.message || fallback || 'La lecture de ce flux a échoué.';
}

export function mediaErrorMessage(video) {
  switch (video?.error?.code) {
    case 2: return 'Une erreur réseau a interrompu la lecture.';
    case 3: return 'Le flux reçu ne peut pas être décodé par macOS.';
    case 4: return 'Le format de ce flux n’est pas reconnu par macOS.';
    default: return 'La lecture de ce flux a échoué.';
  }
}

function errorText(error) { return typeof error === 'string' ? error : error?.message || String(error); }

export function canCopyVideo(probe) {
  return Boolean(probe?.video?.includes('h264') && !probe.unsupported?.some((codec) => probe.video.includes(codec)));
}

export class Player {
  constructor(video, deps, hooks = {}, options = {}) {
    this.video = video;
    this.deps = deps;
    this.hooks = hooks;
    this.token = 0;
    this.ctx = null;
    this.session = null;
    this.lastTime = -1;
    this.lastProgress = 0;
    video.addEventListener('error', () => {
      if (!this.ctx || !video.getAttribute('src')) return;
      if (this.hooks.interceptError?.(this.ctx)) return;
      this.#failure(this.ctx, null);
    });
    video.addEventListener('playing', () => this.#onPlaying());
    if (options.watchdog) this.watchdog = setInterval(() => this.#watch(), 2000);
  }

  get info() { return this.ctx; }

  async play(ctx) {
    const token = ++this.token;
    this.ctx = { ...ctx, index: ctx.index ?? 0, token };
    await this.#startSource(this.ctx);
  }

  stop() {
    this.token += 1;
    this.ctx = null;
    this.#closeSession();
    this.video.pause();
    this.video.removeAttribute('src');
    this.video.load();
  }

  destroy() { this.stop(); clearInterval(this.watchdog); }

  reload() {
    const ctx = this.ctx;
    if (!ctx?.plan) return;
    ctx.resumeAt = ctx.live ? null : this.video.currentTime;
    this.#load(ctx, ctx.plan);
  }

  force(mode) {
    const ctx = this.ctx;
    if (!ctx?.url) return;
    ctx.tried.add(mode);
    this.#load(ctx, { mode, live: ctx.live });
  }

  useSource(index) {
    const ctx = this.ctx;
    if (!ctx || index < 0 || index >= ctx.alternatives.length) return;
    ctx.index = index;
    this.#startSource(ctx);
  }

  #stale(ctx) { return !this.ctx || ctx.token !== this.token; }

  #closeSession() {
    if (this.session) {
      this.deps.invoke('close_stream', { session: this.session }).catch(() => {});
      this.session = null;
    }
  }

  async #startSource(ctx) {
    const channel = ctx.alternatives[ctx.index];
    Object.assign(ctx, { channel, tried: new Set(), probe: undefined, stalls: 0, audioChecked: false, started: false, plan: null, url: null });
    this.hooks.onSource?.(ctx);
    let resolved;
    try { resolved = await this.deps.resolve(channel, ctx); }
    catch (error) { if (!this.#stale(ctx)) this.#sourceFailed(ctx, errorText(error)); return; }
    if (this.#stale(ctx) || ctx.channel !== channel) return;
    ctx.url = resolved.url;
    ctx.headers = resolved.headers || {};
    ctx.live = Boolean(resolved.live);
    ctx.probePromise = /^https?:/i.test(ctx.url)
      ? this.deps.invoke('probe_stream', { url: ctx.url, headers: ctx.headers }).catch(() => null)
      : Promise.resolve(null);
    ctx.probePromise.then((probe) => {
      if (this.#stale(ctx) || ctx.channel !== channel) return;
      ctx.probe = probe;
      this.hooks.onProbe?.(probe, ctx);
    });
    await this.#load(ctx, initialPlan({ url: ctx.url, headers: ctx.headers, live: ctx.live, ffmpeg: this.deps.ffmpeg() }));
  }

  async #load(ctx, plan) {
    Object.assign(ctx, { plan, failing: false });
    ctx.tried.add(plan.mode);
    this.#closeSession();
    this.hooks.onPlan?.(plan, ctx);
    let src;
    try {
      if (plan.mode === 'native') src = await this.deps.nativeSrc(ctx.url);
      else {
        if (plan.mode !== 'relay') this.hooks.onStatus?.(plan.mode === 'transcode' ? 'Démarrage de la conversion FFmpeg…' : 'Préparation du flux MPEG-TS…', ctx);
        const probe = plan.mode === 'transcode' ? await ctx.probePromise : ctx.probe;
        const opened = await this.deps.invoke('open_stream', { request: {
          url: ctx.url, headers: ctx.headers, mode: plan.mode, live: plan.live,
          copyVideo: canCopyVideo(probe), deinterlace: Boolean(probe?.video?.some((codec) => codec.startsWith('mpeg'))),
        } });
        if (this.#stale(ctx) || ctx.plan !== plan) { this.deps.invoke('close_stream', { session: opened.session }).catch(() => {}); return; }
        this.session = opened.session;
        src = opened.url;
      }
    } catch (error) {
      if (!this.#stale(ctx) && ctx.plan === plan) this.#failure(ctx, errorText(error));
      return;
    }
    if (this.#stale(ctx) || ctx.plan !== plan) return;
    this.lastTime = -1;
    this.lastProgress = performance.now();
    if (Number.isFinite(ctx.resumeAt) && ctx.resumeAt > 0) {
      const at = ctx.resumeAt;
      this.video.addEventListener('loadedmetadata', () => {
        if (!this.#stale(ctx) && Number.isFinite(this.video.duration)) {
          try { this.video.currentTime = Math.min(at, Math.max(0, this.video.duration - 1)); } catch { /* not seekable */ }
        }
      }, { once: true });
      ctx.resumeAt = null;
    }
    this.video.src = src;
    this.video.load();
    this.hooks.onLoaded?.(ctx, src);
    try { await this.video.play(); }
    catch (error) {
      if (this.#stale(ctx) || ctx.plan !== plan) return;
      if (error?.name === 'NotAllowedError' || error?.name === 'AbortError') return;
      if (!this.video.error && !this.hooks.interceptError?.(ctx)) this.#failure(ctx, null);
    }
  }

  async #failure(ctx, message) {
    if (this.#stale(ctx) || ctx.failing) return;
    ctx.failing = true;
    const plan = ctx.plan;
    const mediaMessage = mediaErrorMessage(this.video);
    this.hooks.onStatus?.('Analyse du flux…', ctx);
    const probe = await ctx.probePromise;
    if (this.#stale(ctx) || ctx.plan !== plan) return;
    const errors = [];
    if (this.session) {
      try {
        const status = await this.deps.invoke('stream_status', { session: this.session });
        errors.push(...[status.failure, ...status.errors].filter(Boolean));
      } catch { /* the session may be gone */ }
    }
    ctx.lastErrors = errors;
    const next = nextPlan({ probe, ffmpeg: this.deps.ffmpeg(), tried: ctx.tried, url: ctx.url || '', live: ctx.live });
    if (next) {
      this.hooks.onRetry?.(next, ctx);
      this.#load(ctx, next);
      return;
    }
    this.#sourceFailed(ctx, failureMessage(probe, this.deps.ffmpeg(), message || errors[0] || mediaMessage, ctx.tried));
  }

  #sourceFailed(ctx, message) {
    this.hooks.onSourceFailed?.(ctx, message);
    if (ctx.index + 1 < ctx.alternatives.length) {
      ctx.index += 1;
      this.hooks.onFailover?.(ctx, message);
      this.#startSource(ctx);
      return;
    }
    this.#closeSession();
    this.hooks.onFailure?.(message, ctx);
  }

  #onPlaying() {
    const ctx = this.ctx;
    if (!ctx) return;
    this.lastProgress = performance.now();
    if (ctx.started) return;
    ctx.started = true;
    this.hooks.onStarted?.(ctx);
    setTimeout(() => this.#checkAudio(ctx), 5000);
  }

  async #checkAudio(ctx) {
    if (this.#stale(ctx) || ctx.audioChecked) return;
    ctx.audioChecked = true;
    const probe = await ctx.probePromise;
    if (this.#stale(ctx)) return;
    if (probe?.audioMissing) { this.hooks.onAudioIssue?.('missing', ctx); return; }
    const tracks = this.video.audioTracks;
    if (probe?.audio?.length && tracks && tracks.length === 0) {
      if (ctx.plan?.mode !== 'transcode' && this.deps.ffmpeg()) {
        const next = { mode: 'transcode', live: ctx.live };
        this.hooks.onRetry?.(next, ctx, 'audio');
        this.#load(ctx, next);
      } else this.hooks.onAudioIssue?.('undecoded', ctx);
    }
  }

  #watch() {
    const ctx = this.ctx;
    const video = this.video;
    if (!ctx?.plan || ctx.failing || video.paused || video.ended || !video.getAttribute('src')) return;
    if (video.currentTime !== this.lastTime) {
      this.lastTime = video.currentTime;
      this.lastProgress = performance.now();
      return;
    }
    const limit = ctx.started ? 12000 : 30000;
    if (performance.now() - this.lastProgress < limit) return;
    this.lastProgress = performance.now();
    if (ctx.started && ctx.stalls < 2) {
      ctx.stalls += 1;
      this.hooks.onStatus?.('Flux figé : reconnexion…', ctx);
      ctx.resumeAt = ctx.live ? null : video.currentTime;
      this.#load(ctx, ctx.plan);
      return;
    }
    this.#failure(ctx, ctx.started ? 'Le flux s’est figé et ne reprend pas.' : 'Le flux ne démarre pas : aucune image reçue.');
  }
}
