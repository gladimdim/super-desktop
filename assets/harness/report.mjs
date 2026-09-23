import { execFileSync } from "node:child_process";

// Lifecycle callbacks only: no token/output stream is copied or inspected.
export function report(agent, patch, env = process.env) {
  if (!env.SD_HARNESS_EXE || !env.SD_HARNESS_FILE || !env.SD_HARNESS_PID) return;
  try {
    execFileSync(env.SD_HARNESS_EXE, ["harness-event", agent], {
      env, input: JSON.stringify({ ...patch, emitter: process.pid }),
      stdio: ["pipe", "ignore", "ignore"], timeout: 2000,
    });
  } catch { /* Observability must never interrupt or approve an agent action. */ }
}
