// Synthetic transport, loaded ONLY by the opt-in Rust browser fixture.
(() => {
  let instance;
  const titles = ['Open Circuit','Soft Signal','After Hours','Low Frequency','Glass Surface','Parallel Lines','Night Transit','Slow Motion','Static Memory','Blue Room','First Light','Closing Time'];
  class Player {
    constructor(options) { this.options = options; this.listeners = {}; this.state = null; instance = this; }
    addListener(name, fn) { this.listeners[name] = fn; }
    connect() {
      return new Promise(resolve => this.options.getOAuthToken(() => {
        document.getElementById('radio-player').dataset.fixtureConnections = '1';
        this.listeners.ready({device_id:'fixture-browser'}); resolve(true);
      }));
    }
    disconnect() {}
    activateElement() { return Promise.resolve(); }
    getCurrentState() { return Promise.resolve(this.state); }
    publish() { this.listeners.player_state_changed(this.state); return Promise.resolve(); }
    pause() { this.state.paused = true; return this.publish(); }
    resume() { this.state.paused = false; return this.publish(); }
    seek(position) { this.state.position = position; return this.publish(); }
    setVolume(volume) { document.getElementById('radio-player').dataset.fixtureVolume = String(volume); return Promise.resolve(); }
    play(id) {
      const button = [...document.querySelectorAll('.preview-track')].find(b => b.dataset.trackId === id);
      this.state = {paused:false, position:0, duration:210000, disallows:{}, track_window:{current_track:{id, name:button?.getAttribute('aria-label').replace(/^Play /, '') || titles[Number(id)] || 'Fixture track', artists:[{name:'Synthetic artist'}], album:{images:[]}}}};
      this.publish();
    }
  }
  const original = window.fetch.bind(window);
  window.fetch = async (url, options) => {
    const result = await original(url, options);
    if (url === '/api/playback/play' && result.ok) instance.play(new URLSearchParams(options.body).get('track_id'));
    return result;
  };
  window.Spotify = {Player};
  window.onSpotifyWebPlaybackSDKReady();
})();
