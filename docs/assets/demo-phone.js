/* The phone companion, bound to the same SD.store as the desktop replicas. */
(function () {
  "use strict";
  const SD = (window.SD = window.SD || {});
  const el = (tag, cls, html) => { const e = document.createElement(tag); if (cls) e.className = cls; if (html != null) e.innerHTML = html; return e; };

  class Phone {
    constructor(root, opts) {
      this.o = Object.assign({ onAction: () => {} }, opts);
      this.root = root;
      this.pc = "local";
      this.open = null;
      root.classList.add("phone");
      root.innerHTML = "";
      const screen = (this.screen = el("div", "phone-screen"));
      screen.appendChild(el("div", "phone-status", "<span class=\"t\"></span><span>5G ▮▮▮ 82%</span>"));
      this.push = el("button", "phone-push", "");
      this.push.type = "button";
      screen.appendChild(this.push);
      this.body = el("div", "");
      this.body.style.cssText = "flex:1;min-height:0;display:flex;flex-direction:column";
      screen.appendChild(this.body);
      root.appendChild(screen);
      const t = screen.querySelector(".t");
      const tick = () => { t.textContent = new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }); };
      tick(); setInterval(tick, 20000);
      SD.store.on((ev) => this.onStore(ev));
      this.list();
    }

    list() {
      this.open = null;
      const b = this.body;
      b.innerHTML = "";
      const head = el("div", "phone-head", "<h4>SUPER DESKTOP</h4>");
      const pcBtn = el("button", "", "");
      pcBtn.type = "button";
      pcBtn.textContent = SD.PCS.find((p) => p.id === this.pc).label + " ▾";
      pcBtn.title = "Switch PC";
      pcBtn.addEventListener("click", () => {
        const i = SD.PCS.findIndex((p) => p.id === this.pc);
        this.pc = SD.PCS[(i + 1) % SD.PCS.length].id;
        this.list();
        this.o.onAction("pc");
      });
      head.appendChild(pcBtn);
      b.appendChild(head);
      const list = (this.listEl = el("div", "phone-list"));
      const items = SD.store.of(this.pc);
      if (!items.length) list.appendChild(el("div", "phone-empty", "No sessions on this PC. Launch one from the desktop above."));
      items.forEach((s) => list.appendChild(this.item(s)));
      b.appendChild(list);
    }

    item(s) {
      const a = SD.AGENTS[s.agent];
      const it = el("button", "phone-item", '<img alt=""><span class="t"><b></b><span></span></span><span class="st"></span>');
      it.type = "button";
      it.dataset.id = s.id;
      it.querySelector("img").src = a.logo;
      it.querySelector("b").textContent = a.name;
      it.querySelector(".t span").textContent = (s.lastPrompt || "No prompt yet") + " · " + s.folder.split("/").pop();
      const st = it.querySelector(".st");
      if (s.status) { st.dataset.s = s.status; st.textContent = s.status; }
      it.addEventListener("click", () => this.term(s.id));
      return it;
    }

    term(id) {
      const s = SD.store.get(id); if (!s) return this.list();
      const a = SD.AGENTS[s.agent];
      this.open = id;
      const b = this.body;
      b.innerHTML = "";
      const head = el("div", "phone-head", '<button type="button" class="back" aria-label="Back">‹</button><h4></h4>');
      head.querySelector("h4").textContent = a.name;
      head.querySelector(".back").addEventListener("click", () => this.list());
      b.appendChild(head);
      this.termEl = el("div", "phone-term");
      s.lines.slice(-40).forEach(([t, c]) => this.line(t, c));
      b.appendChild(this.termEl);
      const form = el("form", "phone-input", '<input type="text" autocomplete="off" spellcheck="false"><button type="submit">Send</button>');
      const input = form.querySelector("input");
      input.placeholder = a.shell ? "Type a command" : "Message " + a.short;
      input.setAttribute("aria-label", "Prompt for " + a.name + " on the phone");
      form.addEventListener("submit", (e) => {
        e.preventDefault();
        if (!input.value.trim()) return;
        SD.store.prompt(id, input.value);
        input.value = "";
        this.o.onAction("phone-prompt");
      });
      b.appendChild(form);
    }

    line(text, cls) {
      const ln = el("div", cls || "");
      ln.textContent = text || " ";
      this.termEl.appendChild(ln);
      while (this.termEl.childElementCount > 50) this.termEl.firstChild.remove();
    }

    notify(s) {
      const a = SD.AGENTS[s.agent];
      const p = this.push;
      p.innerHTML = '<img alt="" style="width:26px;height:26px"><span><b></b><span></span></span>';
      p.querySelector("img").src = a.logo;
      p.querySelector("b").textContent = a.name + " finished";
      p.querySelector("span span").textContent = s.lastPrompt;
      p.onclick = () => { p.classList.remove("on"); this.pc = s.pc; this.term(s.id); };
      p.classList.add("on");
      clearTimeout(this.pushTimer);
      this.pushTimer = setTimeout(() => p.classList.remove("on"), 4200);
    }

    onStore(ev) {
      const s = ev.id && SD.store.get(ev.id);
      if (ev.type === "status" && ev.status === "finished" && s) this.notify(s);
      if (this.open) {
        if (ev.id !== this.open) return;
        if (ev.type === "line") this.line(ev.text, ev.cls);
        else if (ev.type === "clear") this.termEl.innerHTML = "";
        else if (ev.type === "remove") this.list();
        return;
      }
      if (["add", "remove", "status", "title"].includes(ev.type)) {
        const keep = this.listEl ? this.listEl.scrollTop : 0;
        this.list();
        this.listEl.scrollTop = keep;
      }
    }
  }

  SD.Phone = Phone;
})();
