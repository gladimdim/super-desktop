import { report } from "./report.mjs";
import { randomUUID } from "node:crypto";

export default function integration(pi) {
  let outcome = "unknown";
  let turn;
  let finalResponse = false;
  const metadata = (ctx) => ({
    session: ctx.sessionManager.getSessionId(),
    title: ctx.sessionManager.getSessionName() ?? "",
    model: ctx.model ? `${ctx.model.provider}/${ctx.model.id}` : "",
  });
  const emit = (ctx, patch) => report("pi", { ...metadata(ctx), ...patch });
  pi.on("session_start", async (_event, ctx) => {
    const messages = ctx.sessionManager.getBranch().filter(e => e.type === "message" && e.message?.role === "user");
    const content = messages.at(-1)?.message.content;
    const prompt = typeof content === "string" ? content : (content ?? []).filter(p => p.type === "text").map(p => p.text).join(" ");
    turn = undefined;
    finalResponse = false;
    emit(ctx, { status: ctx.isIdle() ? "idle" : "working", prompt, completionSupported: true });
  });
  pi.on("session_info_changed", async (_event, ctx) => emit(ctx, {}));
  pi.on("before_agent_start", async (event, ctx) => {
    outcome = "unknown";
    turn = randomUUID();
    finalResponse = false;
    emit(ctx, { status: "working", prompt: event.prompt });
  });
  pi.on("agent_start", async (_event, ctx) => emit(ctx, { status: "working" }));
  // agent_end alone is not final: retries and queued continuations may follow.
  pi.on("agent_before_settle", async (event) => { outcome = event.outcome; });
  pi.on("message_end", async (event) => {
    const message = event.message;
    if (message?.role === "assistant") {
      finalResponse = message.stopReason === "stop" && message.content?.some(
        part => part.type === "text" && typeof part.text === "string" && part.text.trim());
    }
  });
  pi.on("agent_settled", async (_event, ctx) => {
    const completed = outcome === "completed" && turn && finalResponse;
    emit(ctx, { status: completed ? "completed" : outcome === "error" ? "error" : "idle",
      ...(completed ? { completionTurn: turn } : {}) });
  });
  pi.on("ui_prompt_start", async (_event, ctx) => emit(ctx, { status: "waiting" }));
  pi.on("ui_prompt_end", async (_event, ctx) => emit(ctx, { status: ctx.isIdle() ? "idle" : "working" }));
  pi.on("model_select", async (_event, ctx) => emit(ctx, {}));
  pi.on("session_shutdown", async (_event, ctx) => emit(ctx, { status: "unknown" }));
}
