// Injected into every frame when IRIE_PLAYER_DEBUG is set: reports what media
// elements, EME and Web Audio are doing, to diagnose silent playback.
(function () {
  const post = (o) => {
    try {
      o.type = "diag";
      o.frame = location.host;
      window.webkit.messageHandlers.ipc.postMessage(JSON.stringify(o));
    } catch (e) {
      try { window.chrome.webview.postMessage(JSON.stringify(o)); } catch (_) {}
    }
  };
  post({ ev: "frame", url: location.href.slice(0, 90) });
  const seen = new WeakSet();
  const tracked = [];
  const info = (m) => ({
    tag: m.tagName, rs: m.readyState, ns: m.networkState, paused: m.paused, muted: m.muted,
    vol: m.volume, t: +m.currentTime.toFixed(2), err: m.error && m.error.code,
    keys: !!m.mediaKeys, src: (m.currentSrc || m.src || "").slice(0, 50), inDom: m.isConnected,
  });
  const watch = (m) => {
    if (seen.has(m)) return;
    seen.add(m);
    tracked.push(m);
    for (const e of ["play", "playing", "pause", "waiting", "stalled", "error", "encrypted", "waitingforkey",
                     "webkitneedkey", "loadedmetadata", "canplay", "ended", "emptied", "abort"]) {
      m.addEventListener(e, () => post(Object.assign({ ev: e }, info(m))));
    }
  };
  const P = HTMLMediaElement.prototype;
  const origPlay = P.play;
  P.play = function () {
    watch(this);
    const r = origPlay.apply(this, arguments);
    if (r && r.then) r.then(() => post(Object.assign({ ev: "play_ok" }, info(this))),
                            (e) => post(Object.assign({ ev: "play_rejected", e: String(e) }, info(this))));
    return r;
  };
  const origKeys = P.setMediaKeys;
  if (origKeys) P.setMediaKeys = function (k) {
    watch(this);
    return origKeys.call(this, k).then((v) => { post({ ev: "setMediaKeys_ok" }); return v; },
                                       (e) => { post({ ev: "setMediaKeys_fail", e: String(e) }); throw e; });
  };
  if (navigator.requestMediaKeySystemAccess) {
    const o = navigator.requestMediaKeySystemAccess.bind(navigator);
    navigator.requestMediaKeySystemAccess = (ks, c) => o(ks, c).then(
      (r) => { post({ ev: "eme_access_ok", ks }); return r; },
      (e) => { post({ ev: "eme_access_fail", ks, e: String(e) }); throw e; });
  }
  if (window.MediaKeySession) {
    const S = MediaKeySession.prototype;
    const gen = S.generateRequest, upd = S.update;
    S.generateRequest = function (t, d) {
      this.addEventListener("message", () => post({ ev: "eme_license_request" }));
      this.addEventListener("keystatuseschange", () => {
        const st = []; this.keyStatuses.forEach((s) => st.push(s)); post({ ev: "eme_keystatus", st });
      });
      return gen.call(this, t, d).then((v) => { post({ ev: "eme_generate_ok", t }); return v; },
                                       (e) => { post({ ev: "eme_generate_fail", e: String(e) }); throw e; });
    };
    S.update = function (r) {
      return upd.call(this, r).then((v) => { post({ ev: "eme_update_ok" }); return v; },
                                    (e) => { post({ ev: "eme_update_fail", e: String(e) }); throw e; });
    };
  }
  for (const name of ["AudioContext", "webkitAudioContext"]) {
    const AC = window[name];
    if (!AC) continue;
    window[name] = new Proxy(AC, {
      construct(t, a) {
        const c = new t(...a);
        post({ ev: "audioctx_new", state: c.state, rate: c.sampleRate });
        c.addEventListener("statechange", () => post({ ev: "audioctx_state", state: c.state }));
        return c;
      },
    });
  }
  window.addEventListener("error", (e) => post({ ev: "js_error", msg: String(e.message).slice(0, 160) }));
  window.addEventListener("unhandledrejection", (e) => post({ ev: "js_rejection", msg: String(e.reason).slice(0, 160) }));
  const short = (u) => String(u).replace(/^https?:\/\//, "").replace(/\?.*$/, "").slice(0, 90);
  if (window.fetch) {
    const of = window.fetch;
    window.fetch = function (u, o) {
      const url = short(u && u.url ? u.url : u);
      post({ ev: "fetch_start", url, method: (o && o.method) || (u && u.method) || "GET", body: o && o.body ? (o.body.byteLength || o.body.length || "?") : 0 });
      return of.apply(this, arguments).then(
        (r) => { post({ ev: "fetch", url, status: r.status }); return r; },
        (e) => { post({ ev: "fetch_fail", url, e: String(e) }); throw e; });
    };
  }
  const xo = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (m, u) {
    const url = short(u);
    post({ ev: "xhr_start", method: m, url });
    this.addEventListener("loadend", () => post({ ev: "xhr", url, status: this.status }));
    return xo.apply(this, arguments);
  };
  const ce = Document.prototype.createElement;
  Document.prototype.createElement = function (tag) {
    const el = ce.apply(this, arguments);
    if (/^(audio|video)$/i.test(tag)) { post({ ev: "create_" + tag }); watch(el); }
    return el;
  };
  if (window.Audio) {
    const A = window.Audio;
    window.Audio = new Proxy(A, { construct(t, a) { const el = new t(...a); post({ ev: "new_Audio" }); watch(el); return el; } });
  }
  if (window.MediaSource) {
    const MS = window.MediaSource;
    window.MediaSource = new Proxy(MS, { construct(t, a) {
      const m = new t(...a); post({ ev: "new_MediaSource" });
      m.addEventListener("sourceopen", () => post({ ev: "ms_sourceopen" }));
      const asb = m.addSourceBuffer.bind(m);
      m.addSourceBuffer = (type) => { post({ ev: "ms_addSourceBuffer", type }); return asb(type); };
      return m; } });
  }
  if (window.WebKitMediaKeys) {
    const WK = window.WebKitMediaKeys;
    window.WebKitMediaKeys = new Proxy(WK, { construct(t, a) {
      const k = new t(...a); post({ ev: "new_WebKitMediaKeys", ks: a[0] });
      const cs = k.createSession.bind(k);
      k.createSession = (type, data) => {
        post({ ev: "wk_createSession", type });
        const sess = cs(type, data);
        for (const e of ["webkitkeymessage", "webkitkeyadded", "webkitkeyerror"]) {
          sess.addEventListener(e, () => post({ ev: e, code: sess.error && sess.error.code, sys: sess.error && sess.error.systemCode }));
        }
        return sess;
      };
      return k; }, get(t, p) { return t[p]; } });
    window.WebKitMediaKeys.isTypeSupported = WK.isTypeSupported.bind(WK);
  }
  const wsmk = HTMLMediaElement.prototype.webkitSetMediaKeys;
  if (wsmk) HTMLMediaElement.prototype.webkitSetMediaKeys = function (k) {
    post({ ev: "webkitSetMediaKeys" }); watch(this);
    return wsmk.call(this, k);
  };
  if (window.WebKitMediaKeys) {
    post({ ev: "has_WebKitMediaKeys", fps: WebKitMediaKeys.isTypeSupported && WebKitMediaKeys.isTypeSupported("com.apple.fps.1_0", "audio/mp4") });
  }
  if (window.MediaSource) post({ ev: "mse", aac: MediaSource.isTypeSupported('audio/mp4; codecs="mp4a.40.2"'), flac: MediaSource.isTypeSupported('audio/mp4; codecs="flac"') });
  else post({ ev: "no_mse", managed: !!window.ManagedMediaSource });
  for (const lvl of ["log", "info"]) {
    const o = console[lvl];
    console[lvl] = function () {
      post({ ev: "console_" + lvl, msg: Array.from(arguments).map(String).join(" ").slice(0, 200) });
      return o.apply(console, arguments);
    };
  }
  for (const lvl of ["error", "warn"]) {
    const o = console[lvl];
    console[lvl] = function () {
      post({ ev: "console_" + lvl, msg: Array.from(arguments).map(String).join(" ").slice(0, 200) });
      return o.apply(console, arguments);
    };
  }
  setInterval(() => tracked.forEach((m) => { if (!m.paused) post(Object.assign({ ev: "tick" }, info(m))); }), 4000);
})();
