// Always — site behaviour. No dependencies. Every section degrades to readable static HTML.
(() => {
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));

  // ── Overlay narrator: the nav pill reports what the page is "doing" ──
  const overlay = $('.overlay');
  const overlayState = $('[data-overlay-state]');
  const setOverlay = (text) => {
    if (!overlayState || overlayState.textContent === text) return;
    overlayState.classList.add('is-swapping');
    setTimeout(() => { overlayState.textContent = text; overlayState.classList.remove('is-swapping'); }, 160);
  };
  const narrated = $$('[data-narrate]');
  if (narrated.length && 'IntersectionObserver' in window) {
    const io = new IntersectionObserver((entries) => {
      entries.forEach((e) => { if (e.isIntersecting) setOverlay(e.target.dataset.narrate); });
    }, { rootMargin: '-45% 0px -50% 0px' });
    narrated.forEach((s) => io.observe(s));
  }

  // ── Hero live line ──
  const live = $('.liveline');
  if (live) {
    const wave = $('[data-wave]', live);
    const text = $('[data-live-text]', live);
    const target = $('[data-live-target]', live);
    const status = $('[data-live-status]', live);
    const toggle = $('[data-live-toggle]', live);
    const lines = [
      { app: 'Ghostty', say: 'claude, move the session store off the audio thread and keep the overlay responsive' },
      { app: 'Claude', say: 'Explain why this test only fails on Linux, then propose the smallest fix.' },
      { app: 'IntelliJ IDEA', say: '// debounce the overlay so it stops flickering between partials' },
      { app: 'Slack', say: 'Pushed the fix — can you try it on your machine before lunch?' },
    ];
    const BARS = 48;
    wave.innerHTML = '<span></span>'.repeat(BARS);
    const bars = [...wave.children];
    let level = 0;
    let paused = false;
    let visible = true;
    let raf = 0;
    let t = 0;

    const drawWave = () => {
      t += 1;
      bars.forEach((b, i) => {
        const env = Math.sin((i / (BARS - 1)) * Math.PI);
        const n = 0.55 + 0.45 * Math.sin(t * 0.23 + i * 1.7) * Math.sin(t * 0.07 + i * 0.6);
        const h = Math.max(0.04, level * env * n + (1 - level) * 0.04);
        b.style.transform = `scaleY(${h.toFixed(3)})`;
      });
      raf = requestAnimationFrame(drawWave);
    };
    const setPhase = (p, label) => { live.dataset.phase = p; status.textContent = label; };
    let gateHeld = () => false;
    const gate = async () => { while (paused || !visible || gateHeld()) await wait(200); };

    const run = async () => {
      let k = 0;
      for (;;) {
        const { app, say } = lines[k % lines.length];
        target.textContent = app;
        text.textContent = '';
        setPhase('silence', 'Listening');
        level = 0;
        await wait(900); await gate();
        setPhase('speaking', 'Hearing you');
        level = 1;
        // Partial results arrive word by word while speaking
        const words = say.split(' ');
        for (let w = 0; w < words.length; w++) {
          await gate();
          text.textContent = words.slice(0, w + 1).join(' ');
          await wait(140 + Math.random() * 120);
        }
        level = 0.15;
        setPhase('silence', 'Transcribing');
        await wait(420); await gate();
        level = 0;
        setPhase('sent', 'Typed into ' + app);
        await wait(1500); await gate();
        k += 1;
      }
    };

    if (reduced) {
      target.textContent = lines[0].app;
      text.textContent = lines[0].say;
      setPhase('sent', 'Typed into ' + lines[0].app);
      toggle.hidden = true;
    } else {
      drawWave();
      run();
      if ('IntersectionObserver' in window) {
        new IntersectionObserver(([e]) => {
          visible = e.isIntersecting;
          if (!visible) cancelAnimationFrame(raf); else if (!paused) { cancelAnimationFrame(raf); drawWave(); }
        }).observe(live);
      }
      // WCAG 2.2.2: the loop also holds while the pointer or keyboard focus is on it
      let held = false
      const hold = (on) => { held = on; if (!paused) overlay.dataset.state = on ? 'paused' : 'listening' }
      live.addEventListener('pointerenter', () => hold(true))
      live.addEventListener('pointerleave', () => hold(false))
      live.addEventListener('focusin', () => hold(true))
      live.addEventListener('focusout', () => hold(false))
      gateHeld = () => held
      toggle.addEventListener('click', () => {
        paused = !paused;
        toggle.setAttribute('aria-pressed', String(paused));
        toggle.textContent = paused ? 'Resume demo' : 'Pause demo';
        overlay.dataset.state = paused ? 'paused' : 'listening';
        if (paused) cancelAnimationFrame(raf); else { cancelAnimationFrame(raf); drawWave(); }
      });
    }
  }

  // ── My Voice room ──
  const voiceToggle = $('[data-voice-toggle]');
  const stage = $('[data-room]');
  if (voiceToggle && stage) {
    const out = $('[data-room-out]', stage);
    const render = () => {
      const on = voiceToggle.getAttribute('aria-checked') === 'true';
      stage.dataset.voice = on ? 'on' : 'off';
      const heard = $$('.source', stage).filter((s) => !on || s.classList.contains('source--you'));
      out.textContent = heard.map((s) => $('.source__said', s).textContent).join(' ');
    };
    voiceToggle.addEventListener('click', () => {
      voiceToggle.setAttribute('aria-checked', String(voiceToggle.getAttribute('aria-checked') !== 'true'));
      render();
    });
    render();
  }

  // ── Phantom phrases get struck through once, on first view ──
  const phantoms = $('.phantoms');
  if (phantoms) {
    $$('s', phantoms).forEach((s, i) => s.style.setProperty('--n', i));
    if (reduced || !('IntersectionObserver' in window)) phantoms.classList.add('is-struck');
    else {
      const io = new IntersectionObserver(([e]) => {
        if (e.isIntersecting) { phantoms.classList.add('is-struck'); io.disconnect(); }
      }, { threshold: 0.5 });
      io.observe(phantoms);
    }
  }

  // ── Vocabulary teach ──
  const teach = $('[data-teach]');
  if (teach) {
    const word = $('[data-teach-word]', teach);
    const fix = $('[data-teach-fix]', teach);
    const reset = $('[data-teach-reset]', teach);
    const learned = $('[data-teach-learned]', teach);
    const apply = () => {
      teach.dataset.state = 'fixed';
      word.textContent = 'Kubernetes';
      learned.textContent = 'Learned: cuber netties → Kubernetes';
      fix.disabled = true;
      fix.dataset.state = 'success';
      reset.hidden = false;
    };
    fix.addEventListener('click', apply);
    reset.addEventListener('click', () => {
      teach.dataset.state = 'wrong';
      word.textContent = 'cuber netties';
      learned.textContent = '';
      fix.disabled = false;
      delete fix.dataset.state;
      reset.hidden = true;
      fix.focus();
    });
    teach._apply = apply;
  }

  // ── Shortcuts: OS labels + live key detection ──
  const chords = $('[data-chords]');
  if (chords) {
    const segs = $$('[data-os]').filter((b) => b.classList.contains('seg__btn'));
    const setOS = (os) => {
      chords.dataset.os = os;
      segs.forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.os === os)));
      $$('kbd[data-mac]', chords).forEach((k) => { k.textContent = os === 'mac' ? k.dataset.mac : k.dataset.pc; });
    };
    segs.forEach((b) => b.addEventListener('click', () => setOS(b.dataset.os)));
    if (!/Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent)) setOS('pc');

    const lit = (name, on) => { const row = $(`[data-chord="${name}"]`, chords); if (row) row.classList.toggle('is-lit', on); };
    const clearAll = () => $$('.chord', chords).forEach((c) => c.classList.remove('is-lit'));
    let litTimer = 0;
    window.addEventListener('keydown', (e) => {
      const mod = e.ctrlKey && e.altKey;
      const key = (e.code || '').replace('Key', '').toLowerCase();
      if (e.key === 'Meta' || (e.key === 'OS')) { lit('hold-cmd', true); return; }
      if (!mod) return;
      const name = e.shiftKey && key === 'p' ? 'shift-p' : ['v', 'w', 'p', 'a'].includes(key) ? key : '';
      if (!name) return;
      clearAll(); lit(name, true);
      clearTimeout(litTimer); litTimer = setTimeout(clearAll, 1200);
      if (name === 'w' && teach && teach.dataset.state !== 'fixed' && teach.getBoundingClientRect().top < innerHeight) teach._apply();
    });
    window.addEventListener('keyup', (e) => { if (e.key === 'Meta' || e.key === 'OS') lit('hold-cmd', false); });
    window.addEventListener('blur', clearAll);
  }

  // ── Install tabs (WAI-ARIA tabs, arrow keys) ──
  const tabs = $('[data-tabs]');
  if (tabs) {
    const list = $$('[role="tab"]', tabs);
    const select = (tab, focus) => {
      list.forEach((t) => {
        const on = t === tab;
        t.setAttribute('aria-selected', String(on));
        t.tabIndex = on ? 0 : -1;
        $('#' + t.getAttribute('aria-controls')).hidden = !on;
      });
      if (focus) tab.focus({ preventScroll: true });
    };
    list.forEach((t, i) => {
      t.addEventListener('click', () => select(t));
      t.addEventListener('keydown', (e) => {
        const d = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
        if (e.key === 'Home') { e.preventDefault(); select(list[0], true); }
        else if (e.key === 'End') { e.preventDefault(); select(list[list.length - 1], true); }
        else if (d) { e.preventDefault(); select(list[(i + d + list.length) % list.length], true); }
      });
    });
    if (/Win/.test(navigator.platform)) select($('#tab-win'));
    else if (/Linux/.test(navigator.platform) && !/Android/.test(navigator.userAgent)) select($('#tab-linux'));
  }

  // ── Copy buttons ──
  const status = $('[data-copy-status]');
  $$('[data-copy]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const code = btn.parentElement.querySelector('code').textContent;
      const done = (ok) => {
        btn.dataset.state = ok ? 'success' : 'error';
        btn.textContent = ok ? 'Copied' : 'Select it';
        if (status) status.textContent = ok ? 'Command copied to the clipboard.' : 'Copy is unavailable here — select the command and copy it.';
        setTimeout(() => { btn.textContent = 'Copy'; delete btn.dataset.state; if (status) status.textContent = ''; }, 2000);
      };
      try { await navigator.clipboard.writeText(code); done(true); }
      catch {
        const ta = Object.assign(document.createElement('textarea'), { value: code });
        ta.setAttribute('readonly', ''); ta.style.cssText = 'position:fixed;opacity:0';
        document.body.append(ta); ta.select();
        let ok = false; try { ok = document.execCommand('copy'); } catch {}
        ta.remove(); done(ok);
      }
    });
  });
})();
