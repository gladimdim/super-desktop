import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, dirname, resolve } from "node:path";
import { report } from "./report.mjs";

export default {
  id: "super-desktop-metadata",
  name: "SUPER DESKTOP metadata",
  register(api) {
    const runs = new Map();
    const observed = new Map();
    let timer;
    const emit = (ctx, patch) => {
      // OpenClaw canonicalizes TUI names to agent:<agent-id>:<session-name>.
      const name = ctx.sessionKey?.split(":").at(-1);
      if (!/^agent:[^:]+:sd_term_[a-zA-Z0-9_-]+$/.test(ctx.sessionKey ?? "")) return;
      try {
        const root = join(homedir(), ".local/state/super-desktop/harness");
        const link = JSON.parse(readFileSync(join(root, `${name}.link.json`), "utf8"));
        if (typeof link.path !== "string" || dirname(resolve(link.path)) !== root) return;
        const state = JSON.parse(readFileSync(link.path, "utf8"));
        if (state.agent !== "openclaw" || !state.pid) return;
        process.kill(state.pid, 0); // Stop heartbeat work for exited TUI processes.
        const entry = api.runtime?.agent?.session?.getSessionEntry?.({
          sessionKey: ctx.sessionKey, agentId: ctx.agentId ?? ctx.sessionKey.split(":")[1],
          readConsistency: "latest",
        });
        // A reset keeps the key but replaces the native session. Ignore late
        // callbacks from the old run instead of resurrecting its title/status.
        if (ctx.sessionId && entry?.sessionId && ctx.sessionId !== entry.sessionId) return;
        const nativeId = ctx.sessionId ?? entry?.sessionId;
        const metadata = { session: nativeId ? `${ctx.sessionKey}/${nativeId}` : ctx.sessionKey };
        if (entry) metadata.title = entry.label ?? entry.displayName ?? "";
        if (ctx.modelId) metadata.model = ctx.modelProviderId ? `${ctx.modelProviderId}/${ctx.modelId}` : ctx.modelId;
        report("openclaw", { ...metadata, ...patch }, {
          ...process.env, SD_HARNESS_FILE: link.path, SD_HARNESS_EXE: link.exe,
          SD_HARNESS_PID: String(state.pid),
        });
        if (observed.size >= 256 && !observed.has(ctx.sessionKey)) observed.delete(observed.keys().next().value);
        observed.set(ctx.sessionKey, { sessionKey: ctx.sessionKey, sessionId: nativeId, agentId: ctx.agentId });
        return true;
      } catch { /* The desktop/TUI may have exited. */ }
    };
    const stop = () => {
      clearInterval(timer);
      timer = undefined;
      // Shutdown has a shared gateway deadline; expiry handles a stopped or
      // crashed gateway without spawning one reporter per observed session.
      observed.clear();
      runs.clear();
    };
    api.on("gateway_start", () => {
      clearInterval(timer);
      // Refresh native renames while idle and keep a bounded liveness heartbeat.
      // No transcript or terminal output is scanned.
      timer = setInterval(() => {
        for (const [key, ctx] of observed) if (!emit(ctx, {})) observed.delete(key);
      }, 3000);
      timer.unref();
    });
    api.on("gateway_stop", stop);
    api.registerRuntimeLifecycle?.({ id: "super-desktop-metadata", dispose: stop });
    api.on("session_start", (event, ctx) => { if (emit({ ...event, ...ctx }, { status: "idle" })) runs.delete(ctx.sessionKey); });
    api.on("session_end", (event, ctx) => {
      if (emit({ ...event, ...ctx }, { status: "unknown" })) {
        runs.delete(ctx.sessionKey);
        observed.delete(ctx.sessionKey);
      }
    });
    api.on("before_model_resolve", (event, ctx) => {
      if (!emit(ctx, { status: "working", ...(typeof event.prompt === "string" ? { prompt: event.prompt } : {}) })) return;
      if (runs.size >= 256) runs.delete(runs.keys().next().value);
      runs.set(ctx.sessionKey, { id: ctx.runId, waiting: false });
    });
    api.on("llm_input", (event, ctx) => {
      const run = runs.get(ctx.sessionKey);
      if (run?.id && event.runId && run.id !== event.runId) return;
      // Tool continuations must not replace the user's tracked prompt or erase
      // the model when a later hook omits it.
      emit(ctx, { status: run?.waiting ? "waiting" : "working",
        ...(event.model ? { model: event.provider ? `${event.provider}/${event.model}` : event.model } : {}) });
    });
    api.on("agent_end", (event, ctx) => {
      const run = runs.get(ctx.sessionKey);
      if (run?.id && event.runId && run.id !== event.runId) return;
      emit(ctx, { status: run?.waiting ? "waiting" : event.success ? "idle" : "error" });
    });
    const events = api.agent?.events ?? api;
    events.registerAgentEventSubscription?.({
      id: "super-desktop-approvals", streams: ["approval"],
      handle(event) {
        const run = runs.get(event.sessionKey);
        if (!run || (run.id && run.id !== event.runId)) return;
        if (event.data.phase === "requested") {
          run.waiting = event.data.status === "pending";
          emit(event, { status: run.waiting ? "waiting" : "error" });
        } else if (event.data.phase === "resolved") {
          run.waiting = false;
          emit(event, { status: ["denied", "failed"].includes(event.data.status) ? "error" : "working" });
        }
      },
    });
  },
};
