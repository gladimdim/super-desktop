/* A replica of the SUPER DESKTOP overlay on a 1280 × 720 screen, scaled to fit
   its frame. Several can share SD.store: what happens in one shows in all. */
(function () {
  "use strict";
  const SD = (window.SD = window.SD || {});
  const W = 1280, H = 720, BAR = 44, WAYBAR = 30, AREA_H = H - BAR - WAYBAR;

  const ICON = {
    arrange: '<svg class="sd-icon" viewBox="0 0 24 24"><rect x="3" y="3" width="8" height="18" rx="1.5"/><rect x="13" y="3" width="8" height="8" rx="1.5"/><rect x="13" y="13" width="8" height="8" rx="1.5"/></svg>',
    gear: '<svg class="sd-icon" viewBox="0 0 24 24"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/></svg>',
    hide: '<svg class="sd-icon" viewBox="0 0 24 24"><path d="M17.9 17.9A10 10 0 0 1 12 20c-7 0-11-8-11-8a18 18 0 0 1 5.1-5.9M9.9 4.2A9 9 0 0 1 12 4c7 0 11 8 11 8a18 18 0 0 1-2.2 3.2M14.1 14.1a3 3 0 1 1-4.2-4.2"/><path d="M1 1l22 22"/></svg>',
    min: '<svg class="sd-icon" viewBox="0 0 24 24"><path d="M5 12h14"/></svg>',
    max: '<svg class="sd-icon" viewBox="0 0 24 24"><rect x="4" y="4" width="16" height="16" rx="2"/></svg>',
    close: '<svg class="sd-icon" viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg>',
    chev: '<svg class="sd-icon" viewBox="0 0 24 24" style="width:13px;height:13px"><path d="M6 9l6 6 6-6"/></svg>',
    note: '<svg class="sd-icon" viewBox="0 0 24 24"><path d="M12 20h9M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z"/></svg>',
  };
  SD.THEMES = [
    ["tokyo", "Tokyo Night", "#7aa2f7"], ["latte", "Catppuccin Latte", "#1e66f5"], ["rosepine", "Rosé Pine", "#c4a7e7"],
    ["everforest", "Everforest", "#a7c080"], ["gruvbox", "Gruvbox", "#fabd2f"],
  ];

  const el = (tag, cls, html) => { const e = document.createElement(tag); if (cls) e.className = cls; if (html != null) e.innerHTML = html; return e; };
  const project = (f) => f.split("/").pop();
  const reduced = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  class Stage {
    constructor(root, opts) {
      this.o = Object.assign({ interactive: true, pc: "local", shown: false, keys: true, theme: null, onAction: () => {} }, opts);
      this.root = root;
      this.pc = this.o.pc;
      this.folder = SD.FOLDERS[0];
      this.cards = new Map();
      this.build();
      this.renderCards();
      this.setShown(this.o.shown, true);
      SD.store.on((ev) => this.onStore(ev));
      new ResizeObserver(() => this.fit()).observe(root);
      this.fit();
    }

    fit() {
      this.k = this.root.clientWidth / W || 1;
      this.screen.style.transform = "scale(" + this.k + ")";
      this.root.style.height = Math.round(H * this.k) + "px";
    }

    build() {
      const r = this.root;
      r.classList.add("sd-stage");
      r.innerHTML = "";
      const sc = (this.screen = el("div", "sd-screen"));
      sc.dataset.theme = this.o.theme || SD.store.theme;
      if (!this.o.interactive) { sc.style.pointerEvents = "none"; r.setAttribute("aria-hidden", "true"); }

      // Under the overlay: an app in a browser, and the bar.
      sc.appendChild(el("div", "sd-desktop",
        '<div class="sd-browser"><div class="sd-browser-bar"><span class="dots"><i></i><i></i><i></i></span>' +
        '<span class="sd-url">localhost:5173 — Demo App</span></div><div class="sd-page"><h3>Demo App</h3>' +
        '<p class="sub">The thing you were working on before the shortcut.</p><div class="bar" style="width:72%"></div>' +
        '<div class="bar" style="width:64%"></div><div class="bar" style="width:68%"></div><div class="bar" style="width:40%"></div>' +
        '<div class="row"><div class="tile"></div><div class="tile"></div><div class="tile"></div></div></div></div>' +
        '<div class="sd-waybar"><span class="ws"><b>1</b><span>2</span><span>3</span><span>4</span></span><span class="clock"></span></div>'));
      this.clock = sc.querySelector(".clock");
      const tick = () => { this.clock.textContent = new Date().toLocaleTimeString([], { weekday: "long", hour: "2-digit", minute: "2-digit" }); };
      tick(); setInterval(tick, 20000);

      // The overlay.
      const ov = (this.overlay = el("div", "sd-overlay"));
      const bar = el("div", "sd-topbar");
      bar.innerHTML =
        '<button class="sd-tb-btn pc" type="button"><span class="pc-label"></span>' + ICON.chev + "</button>" +
        '<span class="sd-logo-word">⚡ SUPER DESKTOP</span>' +
        '<span class="sd-folder"><small>working directory for harness</small><button type="button" class="folder"></button></span>' +
        '<span class="sd-sep"></span><button class="sd-tb-btn" type="button" title="New note">' + ICON.note + "+</button>" +
        SD.LAUNCHERS.map((k) => '<button class="sd-tb-btn launch" type="button" data-agent="' + k + '"><img alt="" src="' +
          SD.AGENTS[k].logo + '">' + SD.AGENTS[k].short + "</button>").join("") +
        '<span class="sd-tb-right"><button class="sd-tb-btn arrange" type="button" title="Arrange">' + ICON.arrange + "</button>" +
        '<button class="sd-tb-btn gear" type="button" title="Settings: theme">' + ICON.gear + "</button>" +
        '<button class="sd-tb-btn hide" type="button" title="Hide">' + ICON.hide + '</button><span class="sd-f1">[F1]</span></span>';
      ov.appendChild(bar);
      this.bar = bar;
      this.cardsEl = el("div", "sd-cards");
      ov.appendChild(this.cardsEl);
      this.toastEl = el("div", "sd-toast");
      ov.appendChild(this.toastEl);
      sc.appendChild(ov);
      bar.querySelector(".pc-label").textContent = this.pcInfo().label;
      bar.querySelector(".folder").textContent = this.folder;

      if (this.o.keys) {
        const keys = (this.keysEl = el("button", "sd-keys",
          '<span class="caps"><span class="cap">SUPER</span>+<span class="cap">SHIFT</span>+<span class="cap">Q</span></span>' +
          '<span class="say">Click the keys to open your workspace</span>'));
        keys.type = "button";
        keys.setAttribute("aria-label", "Press SUPER + SHIFT + Q: open the workspace");
        keys.addEventListener("click", () => this.pressKeys());
        sc.appendChild(keys);
      }
      r.appendChild(sc);
      if (this.o.interactive) this.wireBar();
    }

    pcInfo() { return SD.PCS.find((p) => p.id === this.pc); }

    wireBar() {
      const b = this.bar;
      b.querySelectorAll(".launch").forEach((btn) => btn.addEventListener("click", () => this.launch(btn.dataset.agent)));
      b.querySelector(".arrange").addEventListener("click", () => this.arrange());
      b.querySelector(".hide").addEventListener("click", () => this.setShown(false));
      b.querySelector(".pc").addEventListener("click", (e) => this.menu(e.currentTarget, SD.PCS.map((p) => ({
        label: p.label, note: p.remote ? "paired" : p.host, on: p.id === this.pc, act: () => this.switchPc(p.id),
      })), "Workspaces"));
      b.querySelector(".folder").addEventListener("click", (e) => this.menu(e.currentTarget, SD.FOLDERS.map((f) => ({
        label: f, on: f === this.folder, act: () => { this.folder = f; b.querySelector(".folder").textContent = f; },
      })), "Recent folders"));
      b.querySelector(".gear").addEventListener("click", (e) => this.menu(e.currentTarget, SD.THEMES.map(([id, name]) => ({
        label: name, on: id === SD.store.theme, act: () => { SD.store.setTheme(id); this.o.onAction("theme"); },
      })), "Omarchy theme"));
      this.screen.addEventListener("pointerdown", (e) => { if (this.menuEl && !this.menuEl.contains(e.target)) this.closeMenu(); });
    }

    menu(anchor, items, label) {
      if (this.menuEl) { const same = this.menuFor === anchor; this.closeMenu(); if (same) return; }
      const m = el("div", "sd-menu");
      m.appendChild(el("div", "label", label));
      items.forEach((it) => {
        const btn = el("button", it.on ? "on" : "");
        btn.type = "button";
        btn.textContent = it.label;
        if (it.note) btn.appendChild(el("small", "", "")).textContent = it.note;
        btn.addEventListener("click", () => { this.closeMenu(); it.act(); });
        m.appendChild(btn);
      });
      const sr = this.screen.getBoundingClientRect(), ar = anchor.getBoundingClientRect();
      let left = (ar.left - sr.left) / this.k;
      left = Math.min(left, W - 250);
      m.style.left = left + "px";
      m.style.top = (ar.bottom - sr.top) / this.k + 6 + "px";
      this.screen.appendChild(m);
      this.menuEl = m; this.menuFor = anchor;
    }
    closeMenu() { if (this.menuEl) this.menuEl.remove(); this.menuEl = null; this.menuFor = null; }

    pressKeys() {
      if (!this.keysEl) return this.setShown(true);
      this.keysEl.classList.add("press");
      setTimeout(() => { this.keysEl.classList.remove("press"); this.setShown(true); }, 140);
    }

    setShown(v, instant) {
      this.shown = v;
      if (instant || reduced()) this.screen.classList.add("sd-noanim");
      this.screen.classList.toggle("shown", v);
      if (instant || reduced()) requestAnimationFrame(() => requestAnimationFrame(() => this.screen.classList.remove("sd-noanim")));
      if (!v) this.closeMenu();
      if (!instant) this.o.onAction(v ? "reveal" : "hide");
    }
    toggle() { if (this.shown) this.setShown(false); else this.pressKeys(); }

    launch(agent) {
      if (!this.shown) this.setShown(true);
      const s = SD.store.create(this.pc, agent, this.folder);
      SD.store.raise(s.id);
      this.o.onAction("launch");
      setTimeout(() => this.focus(s.id), 380);
    }

    switchPc(id) {
      if (id === this.pc) return;
      this.closeMenu();
      const info = SD.PCS.find((p) => p.id === id);
      const wait = el("div", "sd-connecting", "<span></span>");
      wait.firstChild.textContent = info.remote ? "Connecting to " + info.label + "…" : "Back to this PC…";
      this.cardsEl.style.opacity = "0";
      this.overlay.appendChild(wait);
      setTimeout(() => {
        wait.remove();
        this.pc = id;
        this.bar.querySelector(".pc-label").textContent = info.label;
        this.cardsEl.style.opacity = "";
        this.renderCards(true);
        if (info.remote) { this.o.onAction("remote"); this.toast(info.label, "Type into any card. It runs on that machine."); }
      }, info.remote ? 1100 : 450);
    }

    toast(title, body) {
      const t = this.toastEl;
      t.innerHTML = "<b></b><span></span>";
      t.firstChild.textContent = title;
      t.lastChild.textContent = body;
      t.classList.add("on");
      clearTimeout(this.toastTimer);
      this.toastTimer = setTimeout(() => t.classList.remove("on"), 3800);
    }

    /* ── Cards ─────────────────────────────────────────────── */
    renderCards(animate) {
      this.cards.forEach((c) => c.el.remove());
      this.cards.clear();
      SD.store.of(this.pc).forEach((s) => this.addCard(s, animate));
    }

    addCard(s, animate) {
      const a = SD.AGENTS[s.agent];
      const remote = this.pcInfo().remote;
      const c = el("div", "sd-card");
      c.innerHTML =
        '<div class="sd-card-head"><span class="sd-dot"></span><img alt=""><span class="sd-title"></span>' +
        (remote ? '<span class="sd-chip" data-s="remote">Remote</span>' : "") + '<span class="sd-chip st"></span>' +
        '<span class="sd-card-btns"><button type="button" class="b-min" title="Minimize">' + ICON.min + "</button>" +
        '<button type="button" class="b-max" title="Expand">' + ICON.max + "</button>" +
        '<button type="button" class="b-close" title="Close">' + ICON.close + "</button></span></div>" +
        '<div class="sd-sess"><span class="id"></span><span class="win"></span><span class="host"></span></div>' +
        '<div class="sd-term"></div><label class="sd-prompt"><b></b><input type="text" spellcheck="false" autocomplete="off"></label>' +
        '<div class="sd-card-foot"><span class="pid"></span><i>Double-click to expand • drag the corner to resize</i></div>' +
        '<span class="sd-resize"></span>';
      c.querySelector(".sd-dot").style.background = a.dot;
      c.querySelector("img").src = a.logo;
      c.querySelector(".sd-sess .id").textContent = s.id;
      c.querySelector(".sd-sess .win").textContent = "1:" + project(s.folder);
      c.querySelector(".sd-sess .host").textContent = this.pcInfo().host;
      c.querySelector(".pid").textContent = "PID: " + s.pid + " • " + a.bin;
      c.querySelector(".sd-prompt b").textContent = a.shell ? "$" : "❯";
      const input = c.querySelector("input");
      input.placeholder = a.hint;
      input.setAttribute("aria-label", "Prompt for " + a.name);
      if (!this.o.interactive) input.tabIndex = -1;
      const card = { el: c, s, term: c.querySelector(".sd-term"), input, title: c.querySelector(".sd-title"), chip: c.querySelector(".st") };
      this.cards.set(s.id, card);
      this.paintTitle(card);
      s.lines.slice(-40).forEach(([t, cls]) => this.appendLine(card, t, cls));
      this.paintGeom(card);
      if (animate) { c.classList.add("entering"); requestAnimationFrame(() => requestAnimationFrame(() => c.classList.remove("entering"))); }
      this.cardsEl.appendChild(c);
      if (this.o.interactive) this.wireCard(card);
    }

    paintTitle(card) {
      const s = card.s;
      card.title.textContent = s.title;
      card.chip.hidden = !s.status;
      if (s.status) { card.chip.dataset.s = s.status; card.chip.textContent = s.status; }
    }

    paintGeom(card) {
      const s = card.s, st = card.el.style;
      st.left = s.x + "px"; st.top = s.y + "px"; st.width = s.w + "px"; st.height = s.h + "px"; st.zIndex = s.z;
      card.el.classList.toggle("mini", !!s.mini);
    }

    appendLine(card, text, cls) {
      const ln = el("div", "ln " + (cls || ""));
      ln.textContent = text;
      card.term.appendChild(ln);
      while (card.term.childElementCount > 60) card.term.firstChild.remove();
    }

    focus(id) {
      const card = this.cards.get(id); if (!card) return;
      this.cards.forEach((c) => c.el.classList.toggle("focused", c === card));
      if (this.o.interactive) card.input.focus({ preventScroll: true });
    }

    wireCard(card) {
      const { el: c, s, input } = card;
      const store = SD.store;
      c.addEventListener("pointerdown", () => { store.raise(s.id); this.focus(s.id); });
      card.term.addEventListener("click", () => input.focus({ preventScroll: true }));
      input.addEventListener("keydown", (e) => {
        e.stopPropagation();
        if (e.key === "Enter" && input.value.trim()) {
          store.prompt(s.id, input.value);
          input.value = "";
          this.o.onAction("prompt");
        }
      });
      c.querySelector(".b-close").addEventListener("click", (e) => { e.stopPropagation(); store.close(s.id); });
      c.querySelector(".b-min").addEventListener("click", (e) => { e.stopPropagation(); store.geom(s.id, { mini: !s.mini }); });
      c.querySelector(".b-max").addEventListener("click", (e) => { e.stopPropagation(); this.expand(s); });
      const head = c.querySelector(".sd-card-head");
      head.addEventListener("dblclick", (e) => { if (!e.target.closest("button")) this.expand(s); });
      this.dragger(head, c, s, false);
      this.dragger(c.querySelector(".sd-resize"), c, s, true);
    }

    expand(s) {
      if (s.prev) { const p = s.prev; s.prev = null; SD.store.geom(s.id, p); }
      else { s.prev = { x: s.x, y: s.y, w: s.w, h: s.h, mini: s.mini }; SD.store.geom(s.id, { x: 8, y: 8, w: W - 16, h: AREA_H - 16, mini: false }); }
    }

    dragger(handle, c, s, resize) {
      handle.addEventListener("pointerdown", (e) => {
        if (e.button !== 0 || (!resize && e.target.closest("button"))) return;
        e.preventDefault();
        if (resize) e.stopPropagation();
        SD.store.raise(s.id);
        this.focus(s.id);
        handle.setPointerCapture(e.pointerId);
        const sx = e.clientX, sy = e.clientY, o = { x: s.x, y: s.y, w: s.w, h: s.h };
        let moved = false;
        c.classList.add("dragging");
        const move = (ev) => {
          const dx = (ev.clientX - sx) / this.k, dy = (ev.clientY - sy) / this.k;
          if (Math.abs(dx) + Math.abs(dy) > 3) moved = true;
          const g = resize
            ? { w: Math.max(300, Math.min(W - o.x, o.w + dx)), h: Math.max(160, Math.min(AREA_H - o.y, o.h + dy)) }
            : { x: Math.max(-o.w + 120, Math.min(W - 120, o.x + dx)), y: Math.max(0, Math.min(AREA_H - 40, o.y + dy)) };
          s.prev = null;
          SD.store.geom(s.id, g, this);
        };
        const up = () => {
          c.classList.remove("dragging");
          handle.removeEventListener("pointermove", move);
          handle.removeEventListener("pointerup", up);
          handle.removeEventListener("pointercancel", up);
          if (moved) this.o.onAction(resize ? "resize" : "drag");
        };
        handle.addEventListener("pointermove", move);
        handle.addEventListener("pointerup", up);
        handle.addEventListener("pointercancel", up);
      });
    }

    arrange() {
      const list = SD.store.of(this.pc).filter((s) => !s.mini);
      const n = list.length; if (!n) return;
      const cols = n <= 1 ? 1 : n <= 4 ? 2 : 3, rows = Math.ceil(n / cols), g = 12;
      const w = (W - g * (cols + 1)) / cols, h = (AREA_H - g * (rows + 1)) / rows;
      list.forEach((s, i) => {
        s.prev = null;
        SD.store.geom(s.id, { x: g + (i % cols) * (w + g), y: g + Math.floor(i / cols) * (h + g), w, h });
      });
      this.o.onAction("arrange");
    }

    onStore(ev) {
      if (ev.type === "theme") { if (!this.o.theme) this.screen.dataset.theme = ev.theme; return; }
      const card = this.cards.get(ev.id);
      switch (ev.type) {
        case "add":
          if (ev.pc === this.pc) this.addCard(SD.store.get(ev.id), true);
          break;
        case "remove":
          if (card) { card.el.classList.add("leaving"); setTimeout(() => card.el.remove(), 260); this.cards.delete(ev.id); }
          break;
        case "line": if (card) this.appendLine(card, ev.text, ev.cls); break;
        case "clear": if (card) card.term.innerHTML = ""; break;
        case "status": case "title": if (card) this.paintTitle(card); break;
        case "geom": if (card) this.paintGeom(card); break;
      }
    }
  }

  SD.Stage = Stage;
})();
