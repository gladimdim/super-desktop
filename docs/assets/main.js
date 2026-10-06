/* Page wiring: the replicas, the CLI panel, tabs, clips, swatches and copy buttons. */
(function () {
  "use strict";
  const SD = window.SD;
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  SD.seed();

  /* ── The hero: the workspace you can try ─────────────────── */
  let touched = false;
  const done = (step) => {
    touched = true;
    $$('.try-hints [data-step~="' + step + '"]').forEach((h) => h.classList.add("done"));
  };
  const hero = new SD.Stage($("#hero-stage"), { onAction: done });

  // Open it once by itself if nobody does, so the page never looks empty.
  new IntersectionObserver((entries, io) => {
    if (!entries[0].isIntersecting) return;
    io.disconnect();
    setTimeout(() => { if (!touched && !hero.shown) hero.pressKeys(); }, reduced ? 600 : 2400);
  }, { threshold: 0.5 }).observe($("#hero-stage"));

  const inView = (node) => { const r = node.getBoundingClientRect(); return r.bottom > 0 && r.top < innerHeight; };
  document.addEventListener("keydown", (e) => {
    const typing = e.target.closest && e.target.closest("input, textarea, [contenteditable]");
    if (typing || e.ctrlKey || !inView($("#hero-stage"))) return;
    if (e.key.toLowerCase() === "q" && e.shiftKey) { e.preventDefault(); hero.toggle(); }
    else if (e.key === "Escape" && hero.shown) hero.setShown(false);
  });

  /* ── Other replicas ──────────────────────────────────────── */
  $$("[data-stage]").forEach((node) => {
    const mirror = node.dataset.stage === "mirror";
    const st = new SD.Stage(node, {
      interactive: !mirror, keys: false, shown: true, pc: node.dataset.pc || "local", theme: node.dataset.theme || null,
      onAction: (a) => { if (a === "remote") node.querySelectorAll(".pulse").forEach((p) => p.classList.remove("pulse")); },
    });
    if (node.dataset.pulse) st.bar.querySelector(node.dataset.pulse).classList.add("pulse");
  });

  const phoneRoot = $("#phone-device");
  if (phoneRoot) new SD.Phone(phoneRoot, { onAction: done });

  /* ── Theme swatches drive every replica ──────────────────── */
  const swatches = $$(".swatches button");
  const paint = (t) => swatches.forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.theme === t)));
  swatches.forEach((b) => b.addEventListener("click", () => SD.store.setTheme(b.dataset.theme)));
  SD.store.on((ev) => { if (ev.type === "theme") paint(ev.theme); });
  paint(SD.store.theme);

  /* ── The CLI panel ───────────────────────────────────────── */
  const out = $("#cli-out");
  if (out) {
    let busy = false, req = 1;
    const write = (text, cls) => {
      const span = document.createElement("span");
      if (cls) span.className = cls;
      span.textContent = text + "\n";
      out.appendChild(span);
      out.scrollTop = out.scrollHeight;
    };
    const pad = (s, n) => (s + " ".repeat(n)).slice(0, n);
    const commands = {
      help: () => [
        ["SUPER DESKTOP for agents (abridged)", "a"],
        ["  Discover   capabilities · harness list · terminal list", ""],
        ["  Observe    terminal capture ID --screen", ""],
        ["  Act        harness launch · terminal move/resize · terminal close", ""],
        ["  Every change takes --request-id: sending the same one again", "m"],
        ["  never launches, moves or closes anything twice.", "m"],
      ],
      harness: () => [[pad("ID", 10) + pad("NAME", 16) + "AVAILABLE", "m"]].concat(
        SD.LAUNCHERS.map((k) => [pad(k, 10) + pad(SD.AGENTS[k].name, 16) + "yes", ""])),
      list: () => {
        const rows = SD.store.of("local");
        if (!rows.length) return [["No terminal cards.", "m"]];
        return [[pad("CARD", 30) + pad("HARNESS", 10) + pad("STATUS", 10) + "FOLDER", "m"]].concat(
          rows.map((s) => [pad(s.id, 30) + pad(s.agent, 10) + pad(s.status || "-", 10) + s.folder, ""]));
      },
      launch: () => {
        const s = SD.store.create("local", "codex", "~/code/demo-app", { prompt: "write tests for the parser" });
        SD.store.raise(s.id);
        return [["launched " + s.id, "p"], ["  harness codex · ~/code/demo-app · prompt sent", "m"],
          ["  ↑ It just appeared in the workspace at the top of the page.", "y"]];
      },
    };
    const label = {
      help: "super-desktop help agents",
      harness: "super-desktop harness list",
      list: "super-desktop terminal list",
      launch: () => 'super-desktop harness launch codex --cwd ~/code/demo-app --request-id demo-' + req++ + ' --prompt "write tests for the parser"',
    };
    write("# Click a command below. Output is shortened sample output.", "m");
    $$(".cli-cmds button").forEach((b) => b.addEventListener("click", () => {
      if (busy) return;
      busy = true;
      $$(".cli-cmds button").forEach((x) => (x.disabled = true));
      const key = b.dataset.cmd;
      const text = typeof label[key] === "function" ? label[key]() : label[key];
      const line = document.createElement("span");
      line.innerHTML = '<span class="p">$ </span>';
      out.appendChild(line);
      let i = 0;
      const type = () => {
        line.appendChild(document.createTextNode(text.slice(i, i + 2)));
        i += 2;
        out.scrollTop = out.scrollHeight;
        if (i < text.length) return setTimeout(type, reduced ? 0 : 14);
        line.appendChild(document.createTextNode("\n"));
        setTimeout(() => {
          commands[key]().forEach(([t, c]) => write(t, c));
          write("", "");
          busy = false;
          $$(".cli-cmds button").forEach((x) => (x.disabled = false));
        }, 260);
      };
      type();
    }));
  }

  /* ── Try / Watch tabs ────────────────────────────────────── */
  $$("[role=tablist]").forEach((list) => {
    const tabs = $$("[role=tab]", list);
    tabs.forEach((tab) => tab.addEventListener("click", () => {
      tabs.forEach((t) => {
        const on = t === tab;
        t.setAttribute("aria-selected", String(on));
        const panel = document.getElementById(t.getAttribute("aria-controls"));
        panel.hidden = !on;
        const v = panel.querySelector("video");
        if (v) { if (on && !reduced) v.play().catch(() => {}); else v.pause(); }
      });
    }));
  });

  // A phone-width screen shrinks a replica of a desktop past use: open the real
  // recordings first there. The phone section keeps its (phone-sized) demo.
  if (window.matchMedia("(max-width: 720px)").matches) {
    $$("[role=tablist]").forEach((list) => {
      if (list.closest("#phone")) return;
      const watch = $$("[role=tab]", list).find((t) => /Watch|Real/.test(t.textContent));
      if (watch) watch.click();
    });
  }

  /* ── Clips: play while on screen ─────────────────────────── */
  $$(".clip video").forEach((v) => {
    const clip = v.closest(".clip");
    const btn = clip.querySelector(".play");
    let paused = reduced;
    const sync = () => { btn.textContent = v.paused ? "▶ Play" : "❚❚ Pause"; };
    v.addEventListener("play", sync); v.addEventListener("pause", sync);
    btn.addEventListener("click", () => { paused = !v.paused; if (v.paused) v.play().catch(() => {}); else v.pause(); });
    new IntersectionObserver((e) => {
      const visible = e[0].isIntersecting && !clip.closest("[hidden]");
      if (visible && !paused) v.play().catch(() => {}); else v.pause();
    }, { threshold: 0.35 }).observe(clip);
    sync();
  });

  /* ── Copy buttons ────────────────────────────────────────── */
  $$("[data-copy]").forEach((b) => b.addEventListener("click", async () => {
    const text = document.getElementById(b.dataset.copy).textContent;
    try { await navigator.clipboard.writeText(text); b.textContent = "Copied ✓"; }
    catch { b.textContent = "Select and copy"; }
    setTimeout(() => (b.textContent = "Copy"), 1800);
  }));

  /* ── Sections fade in ────────────────────────────────────── */
  const io = new IntersectionObserver((entries) => entries.forEach((e) => {
    if (e.isIntersecting) { e.target.classList.add("in"); io.unobserve(e.target); }
  }), { threshold: 0.12 });
  $$(".reveal-up").forEach((n) => io.observe(n));
})();
