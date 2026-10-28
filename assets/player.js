// Spotify owns audio transport. Rust binds playback requests to the current account/preview.
(() => {
  'use strict';
  const el = id => document.getElementById(id);
  let player, device, sdkLoading, connecting, resolveReady, rejectReady, readyTimer;
  let state = null, observedAt = 0, busy = false, seeking = false, disposed = false;
  let queueButtons = [], queuePreview;
  const hardwareVolume = /iPad|iPhone|iPod/.test(navigator.userAgent) ||
    (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);
  const connected = () => el('radio-player')?.dataset.connected === 'true';
  function tracks() {
    const preview = document.querySelector('#selection');
    if (preview) {
      queueButtons = [...preview.querySelectorAll('.preview-track')];
      queuePreview = preview.dataset.previewId;
    }
    return queueButtons;
  }
  const currentId = () => state?.track_window?.current_track?.linked_from?.id || state?.track_window?.current_track?.id;
  function status(text) {
    const node = el('playback-status');
    if (node) { node.textContent = text; node.title = text; }
  }
  function clock(ms) {
    const s = Math.floor(Math.max(0, ms || 0) / 1000);
    return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
  }
  async function request(path, values = {}) {
    const csrf = document.querySelector('meta[name="csrf-token"]')?.content;
    const response = await fetch(path, {method:'POST', credentials:'same-origin', cache:'no-store',
      body:new URLSearchParams({...values, csrf:csrf || ''}), signal:AbortSignal.timeout(35000)});
    if (!response.ok) {
      const body = await response.json().catch(() => ({}));
      throw new Error(typeof body.error === 'string' ? body.error : 'Reconnect Spotify to use playback.');
    }
    return response.status === 204 ? null : response.json();
  }
  function controls() {
    if (!el('radio-player')) return;
    const current = currentId();
    const queue = tracks(), index = queue.findIndex(t => t.dataset.trackId === current);
    el('radio-player').setAttribute('aria-busy', String(busy));
    el('player-toggle').disabled = !connected() || busy || (!!connecting && !device);
    el('player-previous').disabled = !device || busy || index <= 0;
    el('player-next').disabled = !device || busy || index < 0 || index >= queue.length - 1;
    el('player-seek').disabled = !device || busy || !state?.duration || !!state?.disallows?.seeking;
    el('player-volume').disabled = !device || hardwareVolume;
    el('player-volume').title = hardwareVolume ? 'Use your device volume controls' : 'Volume';
    const paused = !state || state.paused;
    el('player-toggle').setAttribute('aria-label', paused ? 'Play' : 'Pause');
    el('player-toggle').title = paused ? 'Play' : 'Pause';
    el('player-toggle').querySelector('img').src = paused ? '/icons/play' : '/icons/pause';
    queue.forEach(button => { button.disabled = !device || busy; });
  }
  function showState(next) {
    state = next; observedAt = performance.now();
    const track = next?.track_window?.current_track;
    if (track) {
      el('player-title').textContent = track.name || 'Unknown track';
      el('player-artist').textContent = (track.artists || []).map(a => a.name).join(', ');
      const image = track.album?.images?.[0]?.url;
      const art = el('player-art');
      let safeImage = false;
      try { const url = new URL(image); safeImage = url.protocol === 'https:' && (url.hostname === 'scdn.co' || url.hostname.endsWith('.scdn.co')); } catch (_) {}
      art.hidden = !safeImage;
      if (safeImage) art.src = image; else art.removeAttribute('src');
      const id = track.linked_from?.id || track.id;
      if (/^[a-zA-Z0-9]{22}$/.test(id)) el('player-link').href = `https://open.spotify.com/track/${id}`;
      else el('player-link').removeAttribute('href');
      el('player-seek').max = String(next.duration || 1);
      el('player-duration').textContent = clock(next.duration);
    }
    controls(); tick();
  }
  function tick() {
    if (!state || seeking) return;
    const position = Math.min(state.duration, state.position + (state.paused ? 0 : performance.now() - observedAt));
    el('player-position').textContent = clock(position);
    el('player-seek').value = String(position);
  }
  function loadSdk() {
    if (window.Spotify?.Player) return Promise.resolve();
    if (sdkLoading) return sdkLoading;
    sdkLoading = new Promise((resolve, reject) => {
      const script = document.createElement('script');
      const timer = setTimeout(() => { script.remove(); sdkLoading = null; reject(new Error('Spotify player could not load. Check your connection and reload.')); }, 15000);
      window.onSpotifyWebPlaybackSDKReady = () => { clearTimeout(timer); resolve(); };
      script.src = 'https://sdk.scdn.co/spotify-player.js';
      script.onerror = () => { clearTimeout(timer); script.remove(); sdkLoading = null; reject(new Error('Spotify player could not load. Check your connection and reload.')); };
      document.head.appendChild(script);
    });
    return sdkLoading;
  }
  function failed(text) {
    status(text); device = null; showState(null);
    if (rejectReady) rejectReady(new Error(text));
    clearTimeout(readyTimer); resolveReady = rejectReady = null;
  }
  async function prepare() {
    if (!connected() || disposed) throw new Error('Connect Spotify to play music.');
    if (device) return;
    if (connecting) return connecting;
    connecting = (async () => {
      status('Connecting player...');
      await loadSdk();
      if (disposed) return;
      if (!player) {
        player = new Spotify.Player({name:'Promptlist', volume:0.5,
          getOAuthToken: callback => {
            request('/api/playback/token').then(body => {
              if (!disposed && connected()) callback(body.access_token);
            }).catch(error => { if (!disposed) failed(error.message); });
          }});
        player.addListener('ready', ({device_id}) => {
          if (disposed) return;
          device = device_id; status(''); clearTimeout(readyTimer);
          resolveReady?.(); resolveReady = rejectReady = null; controls();
        });
        player.addListener('not_ready', () => failed('Player disconnected. Press Play to reconnect.'));
        player.addListener('player_state_changed', next => { if (!disposed) showState(next); });
        player.addListener('autoplay_failed', () => status('Your browser blocked playback. Press Play in the header to resume.'));
        player.addListener('initialization_error', () => failed('Protected audio is unavailable in this browser. Open Promptlist in Chrome or another supported browser.'));
        player.addListener('authentication_error', () => failed('Reconnect Spotify to grant playback permissions.'));
        player.addListener('account_error', () => failed('Spotify Premium is required for this player.'));
        player.addListener('playback_error', () => status('Spotify could not play this track. Try another song.'));
      }
      await new Promise((resolve, reject) => {
        resolveReady = resolve; rejectReady = reject;
        readyTimer = setTimeout(() => failed('Player connection timed out. Press Play to retry.'), 15000);
        player.connect().then(ok => { if (!ok) failed('Spotify player could not connect. Press Play to retry.'); }).catch(() => failed('Spotify player could not connect. Press Play to retry.'));
      });
    })();
    controls();
    try { await connecting; } finally { connecting = null; controls(); }
  }
  async function run(operation) {
    if (busy) return;
    busy = true; controls();
    try { await operation(); } catch (error) { status(error.message || 'Playback failed. Try again.'); }
    finally { busy = false; controls(); }
  }
  function start(button) {
    const track = button?.dataset.trackId, preview = document.querySelector('#selection')?.dataset.previewId || queuePreview;
    if (!track || !preview) { status('Select a track from your playlist.'); return; }
    // Called in the original click path, before asynchronous authorization/network work.
    const activation = player?.activateElement();
    run(async () => {
      await activation; await prepare(); status('Starting playback...');
      await request('/api/playback/play', {device_id:device, track_id:track, preview_id:preview});
      status('');
      const next = await player.getCurrentState();
      if (next) showState(next);
    });
  }
  document.addEventListener('click', event => {
    const track = event.target.closest('.preview-track');
    if (track) { start(track); return; }
    const button = event.target.closest('#player-toggle, #player-previous, #player-next');
    if (!button) return;
    if (button.id !== 'player-toggle') {
      const queue = tracks(), index = queue.findIndex(t => t.dataset.trackId === currentId());
      start(queue[index + (button.id === 'player-next' ? 1 : -1)]); return;
    }
    if (!state && device) { start(tracks()[0]); return; }
    const activation = player?.activateElement();
    run(async () => {
      await activation; await prepare();
      if (!state) { status('Select a track from your playlist.'); return; }
      if (state.paused) await player.resume(); else await player.pause();
      status('');
    });
  });
  document.addEventListener('input', event => {
    if (event.target.id === 'player-seek') { seeking = true; el('player-position').textContent = clock(Number(event.target.value)); }
  });
  document.addEventListener('change', event => {
    if (!player || !device) return;
    if (event.target.id === 'player-seek') {
      const position = Number(event.target.value); seeking = false;
      run(() => player.seek(position));
    }
    if (event.target.id === 'player-volume' && !hardwareVolume) {
      run(() => player.setVolume(Number(event.target.value) / 100));
    }
  });
  function dispose() {
    disposed = true; device = null; clearTimeout(readyTimer);
    rejectReady?.(new Error('Player disconnected.'));
    resolveReady = rejectReady = null;
    player?.disconnect();
  }
  document.addEventListener('promptlist:render', () => { if (!connected()) dispose(); controls(); });
  window.addEventListener('pagehide', dispose);
  window.addEventListener('pageshow', event => { if (event.persisted) { disposed = false; prepare().catch(e => status(e.message)); } });
  setInterval(tick, 250);
  setInterval(() => { if (device && !document.hidden && !busy) player.getCurrentState().then(next => { if (!disposed && device) showState(next); }).catch(() => {}); }, 2000);
  if (connected()) prepare().catch(error => status(error.message));
  controls();
})();
