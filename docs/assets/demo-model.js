/* The simulated workspace: harnesses, their scripted replies, and one shared
   store that every replica on the page (desktop, phone, CLI) reads and drives.
   Nothing here talks to a network; all output is sample text. */
(function () {
  "use strict";
  const SD = (window.SD = window.SD || {});

  SD.AGENTS = {
    claude: {
      name: "Claude Code", short: "Claude", logo: "logos/anthropic-white.svg", dot: "#d97757", bin: "claude",
      glyph: "●", hint: "Try: add a dark mode toggle",
      banner: (f) => [["✻ Welcome to Claude Code", "c-accent"], ["  cwd: " + f, "c-muted"], ["", ""], ["  Type a prompt below and press Enter.", "c-muted"]],
    },
    codex: {
      name: "OpenAI Codex", short: "Codex", logo: "logos/openai-white.svg", dot: "#22d3ee", bin: "codex",
      glyph: "•", hint: "Try: write tests for the parser",
      banner: (f) => [[">_ OpenAI Codex", "c-bold"], ["  directory: " + f, "c-muted"], ["", ""], ["  Ask Codex to do anything.", "c-muted"]],
    },
    opencode: {
      name: "OpenCode", short: "OpenCode", logo: "logos/opencode-light.svg", dot: "#a78bfa", bin: "opencode",
      glyph: "◆", hint: "Try: explain this project",
      banner: (f) => [["opencode", "c-bold"], ["  " + f, "c-muted"], ["", ""], ["  What should we build?", "c-muted"]],
    },
    grok: {
      name: "Grok", short: "Grok", logo: "logos/grok-white.svg", dot: "#e5e7eb", bin: "grok",
      glyph: "▸", hint: "Try: fix the failing build",
      banner: (f) => [["Grok CLI", "c-bold"], ["  " + f, "c-muted"], ["", ""], ["  Ready.", "c-muted"]],
    },
    shell: {
      name: "Shell", short: "Shell", logo: "logos/shell-white.svg", dot: "#4ade80", bin: "bash", shell: true,
      hint: "Try: ls, git status, pwd",
      banner: () => [],
    },
  };
  SD.LAUNCHERS = ["claude", "codex", "opencode", "grok", "shell"];
  SD.FOLDERS = ["~/code/demo-app", "~/code/website", "~/code/api-server"];
  SD.PCS = [
    { id: "local", label: "This PC", host: "laptop" },
    { id: "studio", label: "studio-pc", host: "studio-pc", remote: true },
  ];

  /* Scripted work: a list of [delay ms, text, class] steps for a prompt. */
  function project(folder) { return folder.split("/").pop(); }
  SD.script = function (agent, prompt, folder) {
    const g = SD.AGENTS[agent].glyph;
    const p = prompt.toLowerCase();
    const tool = (t) => [g + " " + t, "c-green"];
    const sub = (t) => ["  ⎿ " + t, "c-muted"];
    let steps;
    if (/test/.test(p)) {
      steps = [tool("Read src/parser.ts"), tool("Write tests/parser.test.ts (+64)"), tool("Bash(npm test)"), sub("✓ 52 passed (1.8 s)"),
        ["", ""], ["Added 12 parser tests, including empty and broken input. All 52 tests pass.", "c-bold"]];
    } else if (/fix|bug|error|crash|fail/.test(p)) {
      steps = [tool("Bash(npm run build)"), sub("error TS2532: Object is possibly 'undefined'  src/app.ts:41"), tool("Read src/app.ts"),
        tool("Update src/app.ts (+3 −1)"), tool("Bash(npm run build && npm test)"), sub("✓ build ok · 48 passed"),
        ["", ""], ["Fixed: the handler read the user before it loaded. Build and tests pass.", "c-bold"]];
    } else if (/dark|theme|colou?r|css|style/.test(p)) {
      steps = [tool("Read src/styles.css"), tool("Read src/components/Header.tsx"), tool("Update src/styles.css (+22)"),
        tool("Update src/components/Header.tsx (+14 −2)"), ["", ""], ["Added a dark mode toggle to the header. It follows the system setting until you pick one.", "c-bold"]];
    } else if (/explain|what|how|why|read|overview/.test(p)) {
      steps = [tool("Read README.md"), tool("Read package.json"), tool("Search \"export\" in src (38 files)"), ["", ""],
        ["" + project(folder) + " is a TypeScript web app: src/ holds the UI, api/ the server routes, tests/ the unit tests.", "c-bold"],
        ["Start at src/main.ts; the routes are wired in api/index.ts.", ""]];
    } else if (/perf|fast|slow|speed|optimi/.test(p)) {
      steps = [tool("Bash(npm run bench)"), sub("render: 41 ms"), tool("Read src/list.tsx"), tool("Update src/list.tsx (+9 −4)"),
        tool("Bash(npm run bench)"), sub("render: 12 ms"), ["", ""], ["The list re-rendered every row on each keystroke. It now memoizes rows: 41 ms → 12 ms.", "c-bold"]];
    } else {
      steps = [tool("Read package.json"), tool("Search \"" + prompt.split(/\s+/).slice(0, 2).join(" ").slice(0, 24) + "\" in src"),
        tool("Update src/app.ts (+11 −3)"), tool("Bash(npm test)"), sub("✓ 48 passed"), ["", ""], ["Done. I made the change in src/app.ts and the tests still pass.", "c-bold"]];
    }
    let t = 700;
    return steps.map(([text, cls]) => { t += 380 + Math.random() * 520; return [t, text, cls]; });
  };

  SD.shellReply = function (cmd, folder) {
    const c = cmd.trim();
    if (!c) return [];
    if (c === "ls") return [["README.md  package.json  src  tests  api", ""]];
    if (c === "pwd") return [[folder.replace("~", "/home/you"), ""]];
    if (c === "git status") return [["On branch main", ""], ["nothing to commit, working tree clean", ""]];
    if (c === "whoami") return [["you", ""]];
    if (c === "date") return [[new Date().toString().slice(0, 24), ""]];
    if (c === "help") return [["Try: ls, pwd, git status, echo hello, clear", "c-muted"]];
    if (/^echo\s/.test(c)) return [[c.slice(5), ""]];
    if (c === "clear") return "clear";
    return [["bash: " + c.split(/\s+/)[0] + ": command not found (this is a demo shell)", "c-red"]];
  };

  /* ── The store ───────────────────────────────────────────── */
  const listeners = new Set();
  let nextId = 1;
  const store = (SD.store = {
    sessions: [],
    theme: "tokyo",
    on(fn) { listeners.add(fn); return () => listeners.delete(fn); },
    emit(ev) { listeners.forEach((fn) => fn(ev)); },
    get(id) { return this.sessions.find((s) => s.id === id); },
    of(pc) { return this.sessions.filter((s) => s.pc === pc); },

    create(pc, agent, folder, opts = {}) {
      const a = SD.AGENTS[agent];
      const n = nextId++;
      const s = {
        id: "sd_term_" + (1791290000000 + n * 7919) + "_" + (0x9b7a + n).toString(16),
        pc, agent, folder, title: a.name, status: a.shell ? null : "idle", lines: [], lastPrompt: "",
        pid: 2000 + Math.floor(Math.random() * 60000), x: 0, y: 0, w: 560, h: 330, z: n, mini: false, timers: [],
      };
      Object.assign(s, opts.geom || this.place(pc, s.w, s.h));
      a.banner(folder).forEach(([t, c]) => s.lines.push([t, c]));
      if (a.shell) s.lines.push(["", ""]);
      this.sessions.push(s);
      this.emit({ type: "add", id: s.id, pc });
      if (opts.prompt) setTimeout(() => this.prompt(s.id, opts.prompt), 600);
      return s;
    },

    /* Cascade new cards like the app does, inside the 1280 × 646 card area. */
    place(pc, w, h) {
      const k = this.of(pc).length;
      return { x: 40 + ((k * 46) % 360), y: 24 + ((k * 38) % 200), w, h };
    },

    push(id, text, cls) {
      const s = this.get(id); if (!s) return;
      s.lines.push([text, cls || ""]);
      if (s.lines.length > 160) s.lines.splice(0, s.lines.length - 160);
      this.emit({ type: "line", id, text, cls: cls || "" });
    },

    status(id, status) {
      const s = this.get(id); if (!s || s.status === status) return;
      s.status = status;
      this.emit({ type: "status", id, status });
    },

    prompt(id, text) {
      const s = this.get(id); if (!s) return;
      text = text.trim(); if (!text) return;
      const a = SD.AGENTS[s.agent];
      if (a.shell) {
        this.push(id, "$ " + text, "c-bold");
        const out = SD.shellReply(text, s.folder);
        if (out === "clear") { s.lines = []; this.emit({ type: "clear", id }); return; }
        out.forEach(([t, c]) => this.push(id, t, c));
        return;
      }
      s.timers.forEach(clearTimeout); s.timers = [];
      s.lastPrompt = text;
      s.title = a.name + " • " + (text.length > 34 ? text.slice(0, 33) + "…" : text);
      this.emit({ type: "title", id });
      this.push(id, "", "");
      this.push(id, "❯ " + text, "c-user");
      this.status(id, "working");
      const steps = SD.script(s.agent, text, s.folder);
      steps.forEach(([t, line, cls]) => s.timers.push(setTimeout(() => this.push(id, line, cls), t)));
      const end = steps[steps.length - 1][0] + 500;
      s.timers.push(setTimeout(() => { this.push(id, "", ""); this.status(id, "finished"); }, end));
    },

    close(id) {
      const s = this.get(id); if (!s) return;
      s.timers.forEach(clearTimeout);
      this.sessions = this.sessions.filter((x) => x !== s);
      this.emit({ type: "remove", id, pc: s.pc });
    },

    geom(id, g, src) {
      const s = this.get(id); if (!s) return;
      Object.assign(s, g);
      this.emit({ type: "geom", id, src });
    },

    raise(id) {
      const s = this.get(id); if (!s) return;
      s.z = Math.max(0, ...this.sessions.map((x) => x.z)) + 1;
      this.emit({ type: "geom", id });
    },

    setTheme(t) { this.theme = t; this.emit({ type: "theme", theme: t }); },
  });

  /* A workspace that already has work in it, on both PCs. */
  SD.seed = function () {
    const a = store.create("local", "claude", "~/code/demo-app", { geom: { x: 34, y: 26, w: 590, h: 360 } });
    [["", ""], ["❯ make the signup form validate emails", "c-user"], ["● Read src/forms/Signup.tsx", "c-green"],
      ["● Update src/forms/Signup.tsx (+18 −2)", "c-green"], ["● Bash(npm test)", "c-green"], ["  ⎿ ✓ 48 passed", "c-muted"], ["", ""],
      ["Emails are now checked as you type, with a clear message under the field.", "c-bold"], ["", ""]]
      .forEach(([t, c]) => a.lines.push([t, c]));
    a.status = "finished"; a.lastPrompt = "make the signup form validate emails";
    a.title = "Claude Code • make the signup form validate emails";
    const b = store.create("local", "codex", "~/code/api-server", { geom: { x: 660, y: 120, w: 586, h: 380 } });
    b.lines.push(["", ""]);
    setTimeout(() => store.prompt(b.id, "speed up the slow orders endpoint"), 900);

    const r1 = store.create("studio", "claude", "~/projects/game", { geom: { x: 40, y: 30, w: 600, h: 400 } });
    [["", ""], ["❯ add sound effects to the menu", "c-user"], ["● Read src/menu.gd", "c-green"], ["● Update src/menu.gd (+26)", "c-green"],
      ["", ""], ["Menu clicks and hovers now play short sounds from assets/sfx.", "c-bold"], ["", ""]].forEach(([t, c]) => r1.lines.push([t, c]));
    r1.status = "finished"; r1.lastPrompt = "add sound effects to the menu"; r1.title = "Claude Code • add sound effects to the menu";
    store.create("studio", "shell", "~/projects/game", { geom: { x: 680, y: 70, w: 540, h: 300 } });
    store.create("studio", "codex", "~/projects/site", { geom: { x: 420, y: 330, w: 560, h: 300 } });
  };
})();
