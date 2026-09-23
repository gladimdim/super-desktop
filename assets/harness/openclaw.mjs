import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { report } from "./report.mjs";

export default {
  id: "super-desktop-metadata",
  name: "SUPER DESKTOP metadata",
  register(api) {
    const emit = (ctx, patch) => {
      // OpenClaw canonicalizes TUI names to agent:<agent-id>:<session-name>.
      const name = ctx.sessionKey?.split(":").at(-1);
      if (!/^sd_term_[a-zA-Z0-9_-]+$/.test(name ?? "")) return;
      try {
        const root = join(homedir(), ".local/state/super-desktop/harness");
        const link = JSON.parse(readFileSync(join(root, `${name}.link.json`), "utf8"));
        if (typeof link.path !== "string" || !link.path.startsWith(root + "/")) return;
        const state = JSON.parse(readFileSync(link.path, "utf8"));
        report("openclaw", { session: ctx.sessionKey, model: ctx.resolvedRef ?? ctx.modelId ?? "", ...patch }, {
          ...process.env, SD_HARNESS_FILE: link.path, SD_HARNESS_EXE: link.exe,
          SD_HARNESS_PID: String(state.pid),
        });
      } catch { /* The desktop/TUI may have exited. */ }
    };
    api.on("session_start", (_event, ctx) => { emit(ctx, { status: "idle" }); });
    api.on("session_end", (_event, ctx) => { emit(ctx, { status: "unknown" }); });
    api.on("before_prompt_build", (event, ctx) => { emit(ctx, { status: "working", prompt: event.prompt ?? "" }); });
    api.on("llm_input", (event, ctx) => { emit(ctx, { status: "working", prompt: event.prompt ?? "" }); });
    api.on("agent_end", (event, ctx) => { emit(ctx, { status: event.success ? "idle" : "error" }); });
  },
};
