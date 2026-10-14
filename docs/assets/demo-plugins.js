/* Plugins in the replica: ✨ Create Plugin, an agent card that "builds" one of
   four sample plugins while you watch, Edit & Enhance, and the plugins
   themselves (docks, a top-bar item with a panel, a game card) on every
   replica of this PC. The agent output is scripted sample text: nothing is
   generated, and the samples are drawn by this page, not by QML. */
(function () {
  "use strict";
  const SD = (window.SD = window.SD || {});
  const store = SD.store;
  const W = 1280, AREA_H = 720 - 44 - 30, GAP = 6, MAX = 6;
  const HOME = "~/.config/super-desktop/plugins/";
  const PLACES = [
    ["dock.right", "Dock on the right edge"], ["dock.left", "Dock on the left edge"], ["toolbar.item", "Item in the top bar"],
    ["panel", "Panel opened from the top bar"], ["settings.page", "Section in Settings → Plugins"],
    ["card", "Card on the desktop (a game, tool or terminal)"],
    ["fullscreen", "Full screen, opened from the top bar (a game, a screensaver)"],
    ["popup", "Popup under a top-bar item (a menu, a quick form)"],
  ];
  const el = (tag, cls, html) => { const e = document.createElement(tag); if (cls) e.className = cls; if (html != null) e.innerHTML = html; return e; };
  const esc = (t) => String(t).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  const two = (n) => String(n).padStart(2, "0");
  const mmss = (ms) => { const s = Math.max(0, Math.ceil(ms / 1000)); return two(Math.floor(s / 60)) + ":" + two(s % 60); };
  const reduced = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  /* ── The sample plugins ──────────────────────────────────── */
  const N = 16; // snake board cells per side
  const KINDS = {
    snake: {
      name: "Snake", chip: "🐍 Snake game", where: "card", wheres: ["card", "fullscreen"],
      request: "A snake game on a card, opened from a button in the top bar",
      match: /\b(snake|games?|play|arcade|tetris|pong)\b/i,
      files: ["Toolbar.qml", "Game.qml"], surface: "game", item: true, win: { w: 344, h: 412, title: "Snake" },
      check: ["--surface game --do 'click 160,160; key Up; wait 600; shot'", "✓ 4 steps pass · the snake moved up"],
      built: "Snake is ready: 🐍 Snake in the top bar opens the card. Click it, then steer with the arrow keys.",
      ready: ["Snake is ready", "Click 🐍 Snake in the top bar to open it."],
      enhance: { hint: "Try: give the snake a rainbow tail", match: /rainbow|colou?r|tail|pretty|fancy/i,
        did: "The snake now has a rainbow tail in your theme's colors.", idea: "A rainbow tail in the theme's colors" },
      ideas: ["Walls that appear as you score", "A pause key"],
      data: () => ({ cells: [[8, 8], [7, 8], [6, 8]], dir: [1, 0], queue: [], food: [12, 4], score: 0, best: 0, over: false, focus: 0, loop: null }),
    },
    pomodoro: {
      name: "Pomodoro", chip: "🍅 Pomodoro timer", where: "panel", wheres: ["toolbar.item", "panel", "popup", "settings.page"],
      request: "A Pomodoro timer in the top bar, with a panel to start and pause it",
      match: /\b(pomodoro|timers?|focus|countdown|tomato|breaks?)\b/i,
      files: ["Toolbar.qml", "Panel.qml"], surface: "timer", item: true, ticks: true, panel: true, win: { w: 300, h: 312, title: "Pomodoro" },
      check: ["--surface timer --do 'click \"Start\"; wait 2s; expect \"24:5\"'", "✓ 3 steps pass · endsAt saved"],
      built: "🍅 in the top bar shows the time left. Click it for the panel: start, pause and reset.",
      ready: ["Pomodoro is ready", "Click 🍅 in the top bar to open its panel."],
      enhance: { hint: "Try: let me pick 5, 15 or 25 minutes", match: /\b(minutes?|length|pick|choose|5|15|short)\b/i,
        did: "The panel now has 5, 15 and 25 minute buttons, and it notifies you when the time is up.", idea: "Pick the length: 5, 15 or 25 minutes" },
      ideas: ["Count the pomodoros done today", "A short break after each one"],
      data: () => ({ min: 25, endsAt: 0, left: 0 }),
    },
    radar: {
      name: "Agent radar", chip: "📡 Agent radar", where: "dock.left", wheres: ["dock.left", "dock.right"], side: "left", size: 64,
      request: "A dock on the left that shows what each of my agents is doing",
      match: /\b(agents?|status|radar|harness(es)?|working|claude|codex|sessions?)\b/i,
      files: ["Dock.qml"], surface: "main",
      check: ["--surface main", "✓ calls: cards.list"],
      built: "A dock on the left edge: one button per agent card, colored by what it is doing. Click one to jump to its card.",
      enhance: { hint: "Try: bounce and notify me when one finishes", match: /bounce|notif|finish|done|alert|tell/i,
        did: "A finished agent now bounces in the dock, and you get a notification.", idea: "Bounce and notify when an agent finishes" },
      ideas: ["Show each agent's folder on hover", "Group the cards by folder"],
      data: () => ({}),
    },
    clock: {
      name: "Clock dock", chip: "🕒 Clock dock", where: "dock.right", wheres: ["dock.right", "dock.left"], side: "right", size: 72, ticks: true,
      request: "A dock on the right edge with a clock, the date and my battery",
      match: /\b(clock|time|date|battery|calendar|day)\b/i,
      files: ["Dock.qml"], surface: "main",
      check: ["--surface main --height 300 --scale 2", "✓ 144×600 · paintedFraction 0.99"],
      built: "A dock on the right edge: the time, the date and the battery, in your theme's colors.",
      enhance: { hint: "Try: show the CPU load too", match: /cpu|load|processor|usage|memory|ram/i,
        did: "The dock now shows the CPU load as a live chart above the battery.", idea: "Show the CPU load as a small live chart" },
      ideas: ["Turn red when the battery is low", "A calendar popup from the date"],
      data: () => ({ cpu: Array.from({ length: 10 }, () => 10 + Math.random() * 30) }),
    },
  };
  const ORDER = ["snake", "pomodoro", "radar", "clock"]; // match order: "timer" must not read as "time"
  const BY_WHERE = { "dock.left": "radar", "dock.right": "clock", card: "snake", fullscreen: "snake" };

  /* ── State on the shared store ───────────────────────────── */
  store.plugins = [];
  const plugin = (id) => store.plugins.find((p) => p.id === id);
  const changed = (p, what) => store.emit({ type: "plugin", id: p.id, what });
  const topZ = () => Math.max(0, ...store.sessions.map((s) => s.z), ...store.plugins.map((p) => (p.g ? p.g.z : 0)));
  store.raise = function (id) {
    const s = this.get(id); if (!s) return;
    s.z = topZ() + 1;
    this.emit({ type: "geom", id });
  };

  const views = [];
  const actions = new Set();
  let lastAgent = "claude";

  const P = (SD.Plugins = {
    KINDS,
    on(fn) { actions.add(fn); },
    act(name, auto) { actions.forEach((fn) => fn(name, !!auto)); },

    /* Docks own the screen edges they sit on (sizes plus a gap each, like the app). */
    insets() {
      const r = { l: 0, r: 0 };
      store.plugins.forEach((p) => { const k = KINDS[p.kind]; if (p.v && k.size) r[p.side === "left" ? "l" : "r"] += k.size + GAP; });
      return r;
    },
    cards() { return store.plugins.filter((p) => p.v && p.open && KINDS[p.kind].win && !KINDS[p.kind].panel); },
    setGeom(p, g) { p.g = Object.assign({}, p.g, g); changed(p, "geom"); },
    raise(p) { if (p.g) { p.g.z = topZ() + 1; changed(p, "geom"); } },

    toggle(p, view) {
      // Plugin cards live on this PC's canvas: from another PC's workspace, go back to them.
      const remote = view && view.pcInfo().remote;
      p.open = remote || !p.open;
      p.pulse = false;
      views.forEach((v) => { const part = v.plug.parts.get(p.id + ":item"); if (part) part.box.classList.remove("pulse"); });
      if (p.open) {
        const k = KINDS[p.kind], ins = this.insets();
        if (!p.g) {
          const n = store.plugins.filter((q) => q.g && q !== p).length; // step aside from the ones placed before
          p.g = { x: W - ins.r - k.win.w - 18 - n * 44, y: 14 + n * 34, w: k.win.w, h: k.win.h, z: 0 };
        }
        p.g.z = topZ() + 1;
        if (remote) view.switchPc("local");
      }
      changed(p, "open");
      this.act("use");
    },

    notify(title, body) { views.forEach((v) => v.toast(title, body)); },

    /* ── One replica ───────────────────────────────────────── */
    attach(view) {
      view.plug = { tb: view.bar.querySelector(".sd-tb-plugins"), layer: el("div", "sd-plug-layer"), parts: new Map(), form: null };
      view.overlay.insertBefore(view.plug.layer, view.toastEl);
      views.push(view);
      store.on((ev) => this.onStore(view, ev));
    },

    /* Bring every plugin surface in this replica in line with the store. */
    render(view) {
      const want = new Set();
      store.plugins.forEach((p) => {
        if (!p.v) return;
        const k = KINDS[p.kind];
        if (k.item) want.add(p.id + ":item");
        if (k.size) want.add(p.id + ":dock");
        if (k.win && p.open && !view.pcInfo().remote) want.add(p.id + ":win");
      });
      view.plug.parts.forEach((part, key) => { if (!want.has(key)) { part.destroy(); view.plug.parts.delete(key); } });
      want.forEach((key) => { if (!view.plug.parts.has(key)) this.mount(view, key); });
      this.layout(view);
    },

    mount(view, key) {
      const [id, part] = key.split(":");
      const p = plugin(id), k = KINDS[p.kind];
      let box, api;
      if (part === "item") {
        box = el("button", "sd-tb-btn pk-item");
        box.type = "button";
        box.title = p.name;
        if (view.o.interactive) box.addEventListener("click", () => this.toggle(p, view));
        if (p.pulse) box.classList.add("pulse");
        api = MOUNT[p.kind].item(box, p, view);
        // Keep the plugins' own order in the bar (by id, like the app).
        const after = [...view.plug.parts.entries()].filter(([kk]) => kk.endsWith(":item") && kk < key).sort(([a], [b]) => (a < b ? -1 : 1)).pop();
        if (after) after[1].box.after(box); else view.plug.tb.prepend(box);
      } else if (part === "dock") {
        box = el("div", "sd-dock pk-" + p.kind);
        box.dataset.side = p.side;
        api = MOUNT[p.kind].dock(box, p, view);
        view.plug.layer.appendChild(box);
      } else {
        box = el("div", "sd-pwin pk-win-" + p.kind);
        box.innerHTML = '<div class="sd-pwin-head"><span class="t"></span><button type="button" class="x" title="Close">✕</button></div><div class="sd-pwin-body"></div>';
        box.querySelector(".t").textContent = k.win.title;
        if (view.o.interactive) this.wireWin(view, box, p);
        api = MOUNT[p.kind].win(box.querySelector(".sd-pwin-body"), p, view);
        view.cardsEl.appendChild(box);
        this.paintWin(box, p);
        box.classList.add("entering");
        requestAnimationFrame(() => requestAnimationFrame(() => box.classList.remove("entering")));
      }
      const entry = { box, api: api || {}, destroy() { if (this.api.destroy) this.api.destroy(); box.remove(); } };
      view.plug.parts.set(key, entry);
    },

    flash(box, kind) {
      if (reduced()) return;
      const cls = kind === "born" ? "pk-born" : "pk-reload";
      box.classList.remove("pk-born", "pk-reload");
      void box.offsetWidth;
      box.classList.add(cls);
      if (kind !== "born") {
        const tag = el("span", "pk-saved", "↻ saved");
        box.appendChild(tag);
        setTimeout(() => tag.remove(), 1300);
      }
      setTimeout(() => box.classList.remove(cls), 1300);
    },

    layout(view) {
      const off = { left: GAP, right: GAP };
      [...store.plugins].sort((a, b) => (a.id < b.id ? -1 : 1)).forEach((p) => {
        const part = view.plug.parts.get(p.id + ":dock");
        if (!part) return;
        const size = KINDS[p.kind].size;
        part.box.style.width = size + "px";
        part.box.style.left = p.side === "left" ? off.left + "px" : "";
        part.box.style.right = p.side === "right" ? off.right + "px" : "";
        off[p.side] += size + GAP;
      });
      view.bar.classList.toggle("pk-crowded", view.plug.tb.childElementCount > 1);
    },

    paintWin(box, p) {
      const g = p.g, st = box.style;
      st.left = g.x + "px"; st.top = g.y + "px"; st.width = g.w + "px"; st.height = g.h + "px"; st.zIndex = g.z;
    },

    wireWin(view, box, p) {
      box.addEventListener("pointerdown", () => this.raise(p));
      box.querySelector(".x").addEventListener("click", (e) => { e.stopPropagation(); p.open = false; changed(p, "open"); });
      const head = box.querySelector(".sd-pwin-head");
      head.addEventListener("pointerdown", (e) => {
        if (e.button !== 0 || e.target.closest("button")) return;
        e.preventDefault();
        head.setPointerCapture(e.pointerId);
        const sx = e.clientX, sy = e.clientY, o = { x: p.g.x, y: p.g.y };
        box.classList.add("dragging");
        const move = (ev) => this.setGeom(p, {
          x: Math.max(-p.g.w + 120, Math.min(W - 120, o.x + (ev.clientX - sx) / view.k)),
          y: Math.max(0, Math.min(AREA_H - 40, o.y + (ev.clientY - sy) / view.k)),
        });
        const up = () => {
          box.classList.remove("dragging");
          head.removeEventListener("pointermove", move);
          head.removeEventListener("pointerup", up);
          head.removeEventListener("pointercancel", up);
        };
        head.addEventListener("pointermove", move);
        head.addEventListener("pointerup", up);
        head.addEventListener("pointercancel", up);
      });
    },

    onStore(view, ev) {
      if (ev.type === "plugin") {
        const p = plugin(ev.id); if (!p) return;
        if (ev.what === "version") {
          const born = p.v === 1 ? "born" : "reload";
          [...view.plug.parts.keys()].filter((key) => key.startsWith(p.id + ":")).forEach((key) => {
            view.plug.parts.get(key).destroy(); view.plug.parts.delete(key);
          });
          this.render(view);
          view.plug.parts.forEach((part, key) => { if (key.startsWith(p.id + ":")) this.flash(part.box, born); });
          return;
        }
        if (ev.what === "open") return this.render(view);
        view.plug.parts.forEach((part, key) => {
          if (!key.startsWith(p.id + ":")) return;
          if (ev.what === "geom" && key.endsWith(":win")) { this.paintWin(part.box, p); if (part.api.update) part.api.update("geom"); }
          else if (part.api.update) part.api.update(ev.what);
        });
        return;
      }
      if (ev.type === "theme" || ["add", "remove", "status", "title"].includes(ev.type)) {
        view.plug.parts.forEach((part) => { if (part.api.update) part.api.update(ev.type, ev); });
      }
    },

    /* The gear menu: like Settings → Plugins, every plugin with Edit & Enhance. */
    menuItems(view) {
      const list = store.plugins.filter((p) => p.v);
      if (!list.length) return [];
      return [{ section: "Plugins: Edit & Enhance" }].concat(list.map((p) => ({
        label: "✨ " + p.name, note: p.side ? "dock." + p.side : KINDS[p.kind].where, act: () => this.enhanceCard(view, p),
      })));
    },

    /* ── ✨ Create Plugin ──────────────────────────────────── */
    openForm(view, auto) {
      if (!view.shown) view.setShown(true);
      view.closeMenu();
      if (view.plug.form) { if (!auto) view.plug.form.ta.focus({ preventScroll: true }); return view.plug.form; }
      const f = el("div", "sd-pform");
      const agents = SD.LAUNCHERS.filter((k) => !SD.AGENTS[k].shell);
      f.innerHTML =
        '<div class="hd"><span class="badge">✨</span><span class="tt"><b>Create a plugin</b><small>An AI agent builds it while you watch</small></span>' +
        '<button type="button" class="x" title="Close">✕</button></div>' +
        '<div class="bd"><p class="sec">What should it do?</p><textarea rows="3" spellcheck="false"></textarea>' +
        '<div class="ex"><span>For example:</span>' + ORDER.map((k) => '<button type="button" data-kind="' + k + '">' + KINDS[k].chip + "</button>").join("") + "</div>" +
        '<div class="grid"><span>Name</span><input type="text" placeholder="Optional; made from the request" spellcheck="false" autocomplete="off">' +
        "<span>Where</span><select>" + PLACES.map(([v, t]) => '<option value="' + v + '">' + esc(t) + "</option>").join("") + "</select>" +
        "<span>Agent</span><select>" + agents.map((k) => '<option value="' + k + '">' + esc(SD.AGENTS[k].name) + "</option>").join("") + "</select></div>" +
        '<p class="status" hidden></p><div class="act"><button type="button" class="go">✨ Create</button></div></div>';
      const [where, agent] = f.querySelectorAll("select");
      const form = (view.plug.form = { el: f, ta: f.querySelector("textarea"), name: f.querySelector("input"), where, agent,
        status: f.querySelector(".status"), go: f.querySelector(".go") });
      agent.value = lastAgent;
      form.ta.setAttribute("aria-label", "What should the plugin do?");
      if (view.o.interactive) {
        f.querySelector(".x").addEventListener("click", () => this.closeForm(view));
        form.go.addEventListener("click", () => this.create(view));
        f.querySelectorAll(".ex button").forEach((b) => b.addEventListener("click", () => {
          const k = KINDS[b.dataset.kind];
          form.ta.value = k.request; form.name.value = k.name; form.where.value = k.where;
          form.ta.focus({ preventScroll: true });
        }));
        f.addEventListener("keydown", (e) => {
          e.stopPropagation();
          if (e.key === "Escape") this.closeForm(view);
          else if (e.key === "Enter" && (e.target === form.name || e.ctrlKey)) { e.preventDefault(); this.create(view); }
        });
      }
      view.overlay.appendChild(f);
      requestAnimationFrame(() => f.classList.add("on"));
      view.o.onAction("plugin-form");
      this.act("form", auto);
      if (!auto) setTimeout(() => form.ta.focus({ preventScroll: true }), 60);
      return form;
    },

    closeForm(view) {
      const f = view.plug.form; if (!f) return;
      view.plug.form = null;
      clearTimeout(f.timer);
      f.el.classList.remove("on");
      setTimeout(() => f.el.remove(), 220);
    },

    create(view, auto) {
      const f = view.plug.form; if (!f || f.busy) return;
      const say = (text, error) => { f.status.hidden = false; f.status.textContent = text; f.status.classList.toggle("err", !!error); };
      const text = f.ta.value.trim();
      if (!text) return say("Say what the plugin should do.", true);
      if (store.plugins.length >= MAX) return say("This demo holds " + MAX + " plugins. Reload the page to start again.", true);
      const where = f.where.value, agent = f.agent.value;
      const kind = ORDER.find((k) => KINDS[k].match.test(text)) || BY_WHERE[where] || "pomodoro";
      const k = KINDS[kind];
      const exact = k.match.test(text) && k.wheres.includes(where);
      const typed = f.name.value.trim();
      const id = uniqueId(typed || text);
      const side = k.size ? (where === "dock.left" ? "left" : where === "dock.right" ? "right" : k.side) : null;
      const p = { id, kind, name: typed || id.replace(/-/g, " "), side, v: 0, open: false, g: null, d: k.data(), exact };
      store.plugins.push(p);
      lastAgent = agent;
      const folder = HOME + id, ins = this.insets();
      const s = store.create("local", agent, folder, {
        geom: { x: ins.l + 36 + (store.plugins.length % 3) * 24, y: 26 + (store.plugins.length % 3) * 16, w: 660, h: 470 },
        prompt: firstPrompt(id, text),
        extra: { plugin: id, pmode: "build", hint: k.enhance.hint },
      });
      store.raise(s.id);
      if (view.pcInfo().remote) view.switchPc("local");
      f.busy = true;
      f.go.disabled = true;
      say(SD.AGENTS[agent].name + " is building “" + id + "” in " + folder +
        ". The request is typed as soon as it is ready; the plugin updates on screen with every save.");
      f.timer = setTimeout(() => this.closeForm(view), auto ? 2200 : 3200);
      this.act("create", auto);
    },

    /* Edit & Enhance: an agent opens in the plugin's folder, says what it does and asks what to change. */
    enhanceCard(view, p) {
      const k = KINDS[p.kind], ins = this.insets();
      const s = store.create("local", lastAgent, HOME + p.id, {
        geom: { x: ins.l + 60, y: 40, w: 620, h: 420 },
        prompt: "This folder is the SUPER DESKTOP plugin \"" + p.name + "\" (id " + p.id + "). Tell me what it does now and three ideas to make it better, then ask me what to change.",
        extra: { plugin: p.id, pmode: "review", hint: k.enhance.hint },
      });
      store.raise(s.id);
      if (view.pcInfo().remote) view.switchPc("local");
      setTimeout(() => view.focus(s.id), 380);
    },

    /* The section's own first run, once, when nobody has touched it. */
    autoplay(view, until) {
      if (store.plugins.length) return;
      const f = this.openForm(view, true);
      const k = KINDS.clock;
      let i = 0, stopped = false;
      const stop = () => { stopped = true; };
      view.root.addEventListener("pointerdown", stop, { once: true });
      const typeIt = () => {
        if (stopped || !view.plug.form) return;
        if (reduced()) i = k.request.length; else i += 2;
        f.ta.value = k.request.slice(0, i);
        if (i < k.request.length) return setTimeout(typeIt, 34);
        f.name.value = k.name;
        f.where.value = k.where;
        f.go.classList.add("pulse");
        setTimeout(() => { f.go.classList.remove("pulse"); if (!stopped && until()) this.create(view, true); }, 900);
      };
      setTimeout(typeIt, 500);
    },
  });

  /* The app's own rule (skill::unique_id): the first four words, joined. */
  function uniqueId(text) {
    let base = text.split(/[^A-Za-z0-9]+/).filter(Boolean).slice(0, 4).join("-").toLowerCase().slice(0, 40).replace(/^-+|-+$/g, "");
    if (!base) base = "my-plugin";
    if (!plugin(base)) return base;
    for (let n = 2; ; n++) if (!plugin(base + "-" + n)) return base + "-" + n;
  }
  function firstPrompt(id, request) {
    return "Build the SUPER DESKTOP plugin in this folder (" + id + "). Read AGENTS.md and .claude/skills/super-desktop-plugin/SKILL.md first, " +
      "then follow the check and snapshot loop until it works. The user asked for: " + request.trim();
  }

  /* ── The agent in a plugin's folder ──────────────────────── */
  const basePrompt = store.prompt.bind(store);
  store.prompt = function (id, text) {
    const s = this.get(id);
    const p = s && s.plugin && plugin(s.plugin);
    if (!p || !text.trim()) return basePrompt(id, text);
    text = text.trim();
    const a = SD.AGENTS[s.agent];
    s.timers.forEach(clearTimeout); s.timers = [];
    s.lastPrompt = text;
    s.title = a.name + " • " + (text.length > 34 ? text.slice(0, 33) + "…" : text);
    this.emit({ type: "title", id });
    this.push(id, "", "");
    this.push(id, "❯ " + text, "c-user");
    this.status(id, "working");
    const mode = s.pmode || "enhance";
    s.pmode = "enhance";
    const steps = (mode === "build" ? buildSteps : mode === "review" ? reviewSteps : enhanceSteps)(s, p, text);
    if (mode === "enhance") P.act("enhance");
    let t = 0;
    steps.forEach(([d, line, cls, fn]) => {
      t += d;
      s.timers.push(setTimeout(() => { if (line != null) this.push(id, line, cls); if (fn) fn(); }, t));
    });
    s.timers.push(setTimeout(() => { this.push(id, "", ""); this.status(id, "finished"); }, t + 500));
  };

  const CMD = "super-desktop plugin ";
  function setV(p, v) {
    if (p.v >= v) return;
    p.v = v;
    changed(p, "version");
    if (v === 1 && KINDS[p.kind].ready) {
      const [title, body] = KINDS[p.kind].ready;
      P.notify(title, body);
      p.pulse = true;
      views.forEach((view) => { const part = view.plug.parts.get(p.id + ":item"); if (part) part.box.classList.add("pulse"); });
    }
  }

  function buildSteps(s, p) {
    const k = KINDS[p.kind], g = SD.AGENTS[s.agent].glyph;
    const tool = (t) => [g + " " + t, "c-green"], sub = (t) => ["  ⎿ " + t, "c-muted"];
    const main = k.files[k.files.length - 1];
    const out = [
      [700, ...tool("Read .claude/skills/super-desktop-plugin/SKILL.md")],
      [650, ...sub("Read 412 lines")],
      [700, ...tool("Write plugin.json (+" + (8 + 3 * k.files.length) + ")")],
    ];
    k.files.forEach((file, i) => out.push([750, ...tool("Write " + file + " (+" + (i === k.files.length - 1 ? 64 : 31) + ")")]));
    out.push(
      [250, ...sub("SUPER DESKTOP reloaded " + p.id), () => setV(p, 1)],
      [900, ...tool("Bash(" + CMD + "check " + p.id + " --json)")],
      [700, ...sub("✓ ok · 0 errors · 0 warnings")],
      [850, ...tool("Bash(" + CMD + "snapshot " + p.id + " --surface " + k.surface + " --out /tmp/" + p.id + ".png)")],
      [750, ...sub("✓ paintedFraction 0.41")],
      [650, ...tool("Read /tmp/" + p.id + ".png")],
      [800, ...tool("Update " + main + " (+23 −4)")],
      [250, ...sub("SUPER DESKTOP reloaded " + p.id), () => setV(p, 2)],
      [900, ...tool("Bash(" + CMD + "snapshot " + p.id + " " + k.check[0] + ")")],
      [750, ...sub(k.check[1])],
      [400, "", ""],
      [500, k.built, "c-bold"],
    );
    if (!p.exact) out.push([200, "(A demo: this page builds one of four sample plugins, where that sample goes. The real agent builds what you asked for.)", "c-muted"]);
    out.push([300, "Want a change? Ask me here, for example: " + k.enhance.hint.replace(/^Try: /, "") + ".", ""]);
    return out;
  }

  function reviewSteps(s, p) {
    const k = KINDS[p.kind], g = SD.AGENTS[s.agent].glyph;
    const tool = (t) => [g + " " + t, "c-green"];
    const out = [[700, ...tool("Read .claude/skills/super-desktop-plugin/SKILL.md")], [600, ...tool("Read plugin.json")]];
    k.files.forEach((f) => out.push([600, ...tool("Read " + f)]));
    out.push(
      [400, "", ""],
      [500, p.name + " (" + k.where + "): " + k.built, "c-bold"],
      [400, "Three ideas:", ""],
      [250, "  1. " + k.enhance.idea, ""],
      [250, "  2. " + k.ideas[0], ""],
      [250, "  3. " + k.ideas[1], ""],
      [400, "What should I change?", "c-bold"],
    );
    return out;
  }

  function enhanceSteps(s, p, text) {
    const k = KINDS[p.kind], g = SD.AGENTS[s.agent].glyph;
    const tool = (t) => [g + " " + t, "c-green"], sub = (t) => ["  ⎿ " + t, "c-muted"];
    const main = k.files[k.files.length - 1];
    const fresh = p.v < 3;
    const out = [
      [700, ...tool("Read " + main)],
      [850, ...tool("Update " + main + " (+" + (fresh ? 19 : 7) + " −" + (fresh ? 2 : 3) + ")")],
      [250, ...sub("SUPER DESKTOP reloaded " + p.id), () => { if (fresh) setV(p, 3); else changed(p, "version"); }],
      [900, ...tool("Bash(" + CMD + "check " + p.id + " --json)")],
      [650, ...sub("✓ ok · 0 errors · 0 warnings")],
      [800, ...tool("Bash(" + CMD + "snapshot " + p.id + " " + k.check[0] + ")")],
      [700, ...sub(k.check[1])],
      [600, ...tool('Update plugin.json ("version": "1.' + (fresh ? 1 : 2) + '.0")')],
      [400, "", ""],
    ];
    if (fresh) {
      out.push([500, k.enhance.did, "c-bold"]);
      if (!k.enhance.match.test(text)) out.push([200, "(A demo: each sample plugin has one prepared change. The real agent makes the change you ask for.)", "c-muted"]);
    } else {
      out.push([500, "Done, and checked.", "c-bold"], [200, "(This demo has no more prepared changes for " + p.name + ". The real agent keeps going.)", "c-muted"]);
    }
    return out;
  }

  /* ── Time passes: clocks, timers, CPU ────────────────────── */
  setInterval(() => {
    const now = Date.now();
    store.plugins.forEach((p) => {
      if (!p.v) return;
      const d = p.d;
      if (p.kind === "clock" && p.v >= 3) { d.cpu.shift(); d.cpu.push(Math.max(4, Math.min(96, d.cpu[d.cpu.length - 1] + (Math.random() - 0.45) * 26))); }
      if (p.kind === "pomodoro" && d.endsAt && now >= d.endsAt) {
        d.endsAt = 0; d.left = 0;
        if (p.v >= 3) P.notify("Pomodoro", "Time is up. Take a short break.");
      }
      if (KINDS[p.kind].ticks) changed(p, "tick");
    });
  }, 1000);

  /* ── How each sample draws itself ────────────────────────── */
  const ICON_BATTERY = '<svg viewBox="0 0 12 20" class="cell"><rect x="1" y="3" width="10" height="16" rx="2"/><rect x="4" y="0.5" width="4" height="2.5" rx="1"/><rect class="lvl" x="3" y="7" width="6" height="10" rx="1"/></svg>';

  const MOUNT = {
    clock: {
      dock(box, p) {
        const v = p.v;
        box.innerHTML = '<div class="pk-clock"><b class="hh"></b><b class="mm"></b>' +
          (v >= 2 ? '<i class="rule"></i><span class="wd"></span><span class="dt"></span>' : "") + '<span class="fill"></span>' +
          (v >= 3 ? '<div class="cpu"><small>CPU</small><span class="bars">' + "<i></i>".repeat(10) + "</span><em></em></div>" : "") +
          (v >= 2 ? '<div class="bat">' + ICON_BATTERY + "<span>87%</span></div>" : "") + "</div>";
        const q = (s) => box.querySelector(s);
        const update = () => {
          const now = new Date();
          q(".hh").textContent = two(now.getHours());
          q(".mm").textContent = two(now.getMinutes());
          if (v >= 2) {
            q(".wd").textContent = now.toLocaleDateString("en-GB", { weekday: "short" });
            q(".dt").textContent = now.toLocaleDateString("en-GB", { day: "numeric", month: "short" });
          }
          if (v >= 3) {
            box.querySelectorAll(".bars i").forEach((b, i) => (b.style.height = p.d.cpu[i] + "%"));
            q(".cpu em").textContent = Math.round(p.d.cpu[p.d.cpu.length - 1]) + "%";
          }
        };
        update();
        return { update: (what) => { if (what === "tick") update(); } };
      },
    },

    radar: {
      dock(box, p, view) {
        const v = p.v;
        box.innerHTML = '<div class="pk-radar">' + (v >= 2 ? '<div class="head"><b></b><small>agents</small></div>' : "") + '<div class="list"></div></div>';
        const list = box.querySelector(".list");
        const paint = () => {
          const items = store.of("local");
          if (v >= 2) {
            box.querySelector(".head b").textContent = items.length;
            const busy = items.filter((s) => s.status === "working").length;
            box.querySelector(".head small").textContent = busy ? busy + " working" : "agents";
          }
          list.innerHTML = "";
          items.forEach((s) => {
            const a = SD.AGENTS[s.agent];
            const b = el("button", "ag", '<img alt=""><i class="ring"></i>' + (v >= 3 && s.status === "finished" ? "<em>✓</em>" : ""));
            b.type = "button";
            b.dataset.id = s.id;
            if (v >= 2 && s.status) b.dataset.s = s.status;
            b.querySelector("img").src = a.logo;
            b.title = s.title + (s.status ? " · " + s.status : "");
            b.setAttribute("aria-label", b.title);
            if (view.o.interactive) b.addEventListener("click", () => {
              if (view.pcInfo().remote) view.switchPc("local");
              store.raise(s.id);
              if (store.get(s.id).mini) store.geom(s.id, { mini: false });
              setTimeout(() => view.focus(s.id), 60);
              P.act("use");
            });
            else b.tabIndex = -1;
            list.appendChild(b);
          });
        };
        paint();
        return {
          update(what, ev) {
            if (!["add", "remove", "status", "title"].includes(what)) return;
            paint();
            if (what === "status" && ev.status === "finished" && v >= 3) {
              const b = list.querySelector('[data-id="' + ev.id + '"]');
              if (b && !reduced()) b.classList.add("bounce");
              if (view === views[0]) { const s = store.get(ev.id); P.notify("Agent radar", SD.AGENTS[s.agent].name + " finished: " + (s.lastPrompt || s.folder)); }
            }
          },
        };
      },
    },

    pomodoro: {
      item(box, p) {
        box.innerHTML = '<span aria-hidden="true">🍅</span><span class="t"></span>' + (p.v >= 2 ? '<i class="bar"><b></b></i>' : "");
        const update = () => {
          const d = p.d, run = d.endsAt > Date.now(), left = run ? d.endsAt - Date.now() : d.left || d.min * 60000;
          box.querySelector(".t").textContent = run || d.left ? mmss(left) : p.v >= 2 ? mmss(d.min * 60000) : "Focus";
          if (p.v >= 2) box.querySelector(".bar b").style.width = (run || d.left ? 100 - (left / (d.min * 60000)) * 100 : 0) + "%";
          box.classList.toggle("on", run);
        };
        update();
        return { update: (what) => { if (what === "tick" || what === "data") update(); } };
      },
      win(body, p, view) {
        const v = p.v, d = p.d, R = 52, C = 2 * Math.PI * R;
        body.innerHTML = v === 1
          ? '<div class="pk-pomo v1"><div class="big"></div><button type="button" class="pk-btn" data-act="toggle"></button></div>'
          : '<div class="pk-pomo"><div class="ring"><svg viewBox="0 0 120 120"><circle class="bg" cx="60" cy="60" r="' + R + '"/>' +
            '<circle class="fg" cx="60" cy="60" r="' + R + '" stroke-dasharray="' + C + '"/></svg><div class="mid"><b></b><small>Focus</small></div></div>' +
            (v >= 3 ? '<div class="lens">' + [5, 15, 25].map((m) => '<button type="button" data-min="' + m + '">' + m + " min</button>").join("") + "</div>" : "") +
            '<div class="row"><button type="button" class="pk-btn primary" data-act="toggle"></button><button type="button" class="pk-btn" data-act="reset">Reset</button></div></div>';
        const update = () => {
          const run = d.endsAt > Date.now(), total = d.min * 60000, left = run ? d.endsAt - Date.now() : d.left || total;
          const go = body.querySelector('[data-act="toggle"]');
          if (v === 1) {
            body.querySelector(".big").textContent = run ? "Focusing…" : d.min + " minutes";
            go.textContent = run ? "Stop" : "Start";
            return;
          }
          body.querySelector(".mid b").textContent = mmss(left);
          body.querySelector(".mid small").textContent = run ? "Focus" : d.left ? "Paused" : "Ready";
          body.querySelector(".fg").style.strokeDashoffset = String(C * (left / total));
          go.textContent = run ? "Pause" : d.left ? "Resume" : "Start";
          body.querySelectorAll(".lens button").forEach((b) => b.classList.toggle("on", +b.dataset.min === d.min));
        };
        if (view.o.interactive) body.addEventListener("click", (e) => {
          const b = e.target.closest("button"); if (!b) return;
          const now = Date.now();
          if (b.dataset.act === "toggle") {
            if (d.endsAt > now) { if (v === 1) { d.endsAt = 0; d.left = 0; } else { d.left = d.endsAt - now; d.endsAt = 0; } }
            else { d.endsAt = now + (d.left || d.min * 60000); d.left = 0; }
          } else if (b.dataset.act === "reset") { d.endsAt = 0; d.left = 0; }
          else if (b.dataset.min) { d.min = +b.dataset.min; d.endsAt = 0; d.left = 0; }
          changed(p, "data");
          P.act("use");
        });
        update();
        return { update: (what) => { if (what === "tick" || what === "data") update(); } };
      },
    },

    snake: {
      item(box) { box.innerHTML = '<span aria-hidden="true">🐍</span><span>Snake</span>'; },
      win(body, p, view) {
        const v = p.v, d = p.d;
        body.innerHTML = '<div class="pk-snake">' + (v >= 2 ? '<div class="score"><span>Score <b class="sc">0</b></span><span class="best">Best <b>0</b></span></div>' : "") +
          '<div class="board"><canvas></canvas><div class="hint"></div></div></div>';
        const board = body.querySelector(".board"), cv = body.querySelector("canvas"), hint = body.querySelector(".hint");
        let size = 0, focused = false;
        const fit = () => {
          const s = Math.floor(Math.max(120, Math.min(p.g.w - 22, p.g.h - 36 - (v >= 2 ? 44 : 18))) / N) * N;
          if (s === size) return;
          size = s;
          cv.style.width = cv.style.height = s + "px";
          cv.width = cv.height = s * 2;
        };
        const draw = () => {
          fit();
          const cs = getComputedStyle(view.screen), c = (n) => cs.getPropertyValue("--t-" + n).trim();
          const x = cv.getContext("2d"), cell = (size * 2) / N, r = cell * 0.22;
          x.clearRect(0, 0, cv.width, cv.height);
          x.fillStyle = c("surface"); x.fillRect(0, 0, cv.width, cv.height);
          const rain = ["green", "cyan", "accent", "magenta", "red", "yellow"].map(c);
          d.cells.forEach(([cx, cy], i) => {
            x.fillStyle = i === 0 ? c("accent") : v >= 3 ? rain[(i - 1) % rain.length] : c("green");
            x.beginPath(); x.roundRect(cx * cell + 1.5, cy * cell + 1.5, cell - 3, cell - 3, r); x.fill();
          });
          x.fillStyle = c("red");
          x.beginPath(); x.arc(d.food[0] * cell + cell / 2, d.food[1] * cell + cell / 2, cell * 0.36, 0, Math.PI * 2); x.fill();
          if (v >= 2) { body.querySelector(".sc").textContent = d.score; body.querySelector(".best b").textContent = d.best; }
          hint.textContent = d.over ? "Game over · click to play again" : focused ? "" : "Click to play";
          hint.hidden = !hint.textContent;
        };
        if (view.o.interactive) {
          board.tabIndex = 0;
          board.setAttribute("aria-label", "Snake: click, then use the arrow keys");
          board.addEventListener("pointerdown", (e) => { e.stopPropagation(); P.raise(p); if (d.over) restart(p); board.focus({ preventScroll: true }); start(p); });
          board.addEventListener("focus", () => { if (focused) return; focused = true; d.focus++; start(p); draw(); P.act("use"); });
          board.addEventListener("blur", () => { if (!focused) return; focused = false; d.focus = Math.max(0, d.focus - 1); draw(); });
          board.addEventListener("keydown", (e) => {
            const dir = { ArrowUp: [0, -1], ArrowDown: [0, 1], ArrowLeft: [-1, 0], ArrowRight: [1, 0], w: [0, -1], s: [0, 1], a: [-1, 0], d: [1, 0] }[e.key];
            if (!dir) return;
            e.preventDefault(); e.stopPropagation();
            if (d.over) restart(p);
            if (d.queue.length < 3) d.queue.push(dir);
            start(p);
          });
          let sw = null; // swipes, for touch screens
          board.addEventListener("pointerdown", (e) => { sw = [e.clientX, e.clientY]; });
          board.addEventListener("pointerup", (e) => {
            if (!sw) return;
            const dx = e.clientX - sw[0], dy = e.clientY - sw[1]; sw = null;
            if (Math.max(Math.abs(dx), Math.abs(dy)) < 24) return;
            d.queue.push(Math.abs(dx) > Math.abs(dy) ? [Math.sign(dx), 0] : [0, Math.sign(dy)]);
          });
        }
        draw();
        return {
          // A theme reaches this listener before the replica's own: draw once it has switched.
          update: (what) => { if (what === "theme") requestAnimationFrame(draw); else if (what === "data" || what === "geom") draw(); },
          destroy: () => { if (focused) { focused = false; d.focus = Math.max(0, d.focus - 1); } },
        };
      },
    },
  };

  function restart(p) {
    const fresh = KINDS.snake.data();
    Object.assign(p.d, { cells: fresh.cells, dir: fresh.dir, queue: [], food: fresh.food, score: 0, over: false });
    changed(p, "data");
  }
  function start(p) {
    const d = p.d;
    if (d.loop || d.over || !d.focus) return;
    const step = () => {
      d.loop = null;
      if (d.over || !d.focus || !p.open) return;
      while (d.queue.length) {
        const nd = d.queue.shift();
        if (nd[0] !== -d.dir[0] || nd[1] !== -d.dir[1]) { d.dir = nd; break; }
      }
      const [hx, hy] = d.cells[0], nx = hx + d.dir[0], ny = hy + d.dir[1];
      if (nx < 0 || ny < 0 || nx >= N || ny >= N || d.cells.slice(0, -1).some(([x, y]) => x === nx && y === ny)) {
        d.over = true; changed(p, "data"); return;
      }
      d.cells.unshift([nx, ny]);
      if (nx === d.food[0] && ny === d.food[1]) {
        d.score++; d.best = Math.max(d.best, d.score);
        do { d.food = [Math.floor(Math.random() * N), Math.floor(Math.random() * N)]; } while (d.cells.some(([x, y]) => x === d.food[0] && y === d.food[1]));
      } else d.cells.pop();
      changed(p, "data");
      d.loop = setTimeout(step, p.v >= 2 ? Math.max(70, 160 - d.score * 5) : 150);
    };
    d.loop = setTimeout(step, 160);
  }
})();
