import { execFileSync } from "node:child_process";

// Lifecycle callbacks only: no token/output stream is copied or inspected.
export function report(agent, patch, env = process.env) {
  if (!env.SD_HARNESS_EXE || !env.SD_HARNESS_FILE || !env.SD_HARNESS_PID) return;
  try {
    // Keep lifecycle metadata below the Rust reader's bound even for very large
    // prompts. Never copy an unbounded prompt/transcript into a subprocess pipe.
    const bounded = Object.fromEntries(Object.entries(patch).map(([key, value]) =>
      [key, typeof value === "string" ? value.slice(0, 1000) : value]));
    execFileSync(env.SD_HARNESS_EXE, ["harness-event", agent], {
      env, input: JSON.stringify({ ...bounded, emitter: process.pid }),
      stdio: ["pipe", "ignore", "ignore"], timeout: 2000,
    });
  } catch { /* Observability must never interrupt or approve an agent action. */ }
}
