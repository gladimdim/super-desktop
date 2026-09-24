import { report } from "./report.mjs";

export const SuperDesktop = async ({ client }) => {
  let selected;
  let failed = false;
  let selection = 0;
  let revision = 0;
  let turn;
  const sessions = new Map();
  const emit = (patch) => report("opencode", patch);
  // Session lookup filters tool-created child agents; cwd and creation order
  // are deliberately not used to decide which conversation owns this card.
  const info = async (id) => {
    if (!sessions.has(id)) {
      try {
        const response = await client.session.get({ path: { id } });
        if (response.data) sessions.set(id, response.data);
      } catch { return undefined; }
    }
    return sessions.get(id);
  };
  const choose = async (id) => {
    const generation = ++selection;
    const session = await info(id);
    if (generation !== selection) return false;
    if (!session || session.parentID) return false;
    if (selected !== id) { failed = false; turn = undefined; ++revision; }
    selected = id;
    emit({ session: id, title: session.title ?? "", completionSupported: true });
    return true;
  };
  // Idle is only a trigger to inspect native records, never completion evidence.
  const settle = async () => {
    const session = selected, prompt = turn, version = revision, generation = selection;
    if (!prompt || failed || typeof client.session.messages !== "function") return;
    try {
      const response = await client.session.messages({ path: { id: session }, query: { limit: 16 },
        signal: AbortSignal.timeout(2000) });
      if (session !== selected || prompt !== turn || version !== revision || generation !== selection || failed) return;
      const messages = response.data;
      if (!Array.isArray(messages) || messages.length > 16) return;
      const own = messages.filter(m => m.info?.sessionID === session);
      const users = own.filter(m => m.info.role === "user");
      if (users.at(-1)?.info.id !== prompt) return;
      const last = own.filter(m => m.info.role === "assistant").at(-1);
      if (!last || last.info.parentID !== prompt || last.info.summary || last.info.error ||
          last.info.finish !== "stop" || !last.info.time?.completed ||
          !last.parts?.some(p => p.type === "text" && !p.synthetic && typeof p.text === "string" && p.text.trim())) return;
      emit({ session, status: "completed", completionTurn: prompt });
    } catch { /* Missing, unsupported or ambiguous native records fail closed. */ }
  };
  // Status of an explicitly selected session from this process's own server
  // (it lists only non-idle sessions). Unavailable or malformed: no guess.
  const current = async (id) => {
    if (typeof client.session.status !== "function") return undefined;
    try {
      const response = await client.session.status({ signal: AbortSignal.timeout(2000) });
      const all = response?.data;
      if (!all || typeof all !== "object" || Array.isArray(all)) return undefined;
      const type = all[id]?.type;
      if (type === undefined || type === "idle") return "idle";
      return ["busy", "retry"].includes(type) ? "working" : undefined;
    } catch { return undefined; }
  };
  // A freshly started OpenCode process (its server runs in-process) has no
  // turn in flight: the TUI opens on an empty composer, so it is idle.
  emit({ status: "idle" });
  return {
    "chat.message": async (input, output) => {
      if (!await choose(input.sessionID)) return;
      failed = false;
      ++revision;
      turn = output.message?.id;
      emit({ session: selected, status: "working", model: input.model ? `${input.model.providerID}/${input.model.modelID}` : "",
        prompt: output.parts.filter(p => p.type === "text" && !p.synthetic).map(p => p.text).join(" ") });
    },
    event: async ({ event }) => {
      const p = event.properties ?? {};
      if (event.type === "session.created" || event.type === "session.updated") {
        const value = p.info;
        if (!value) return;
        sessions.set(value.id, value);
        // Server-wide creation/update events do not identify the TUI's selection.
        // Only a TUI selection or submitted root-session prompt can claim it.
        if (value.id === selected) emit({ session: selected, title: value.title ?? "" });
        return;
      }
      if (event.type === "tui.session.select") {
        if (!await choose(p.sessionID)) return;
        const session = selected, version = revision;
        const status = await current(session);
        // A newer status event or selection wins over this lookup.
        if (status && session === selected && version === revision) emit({ session, status });
        return;
      }
      if (event.type === "session.deleted") {
        sessions.delete(p.info?.id);
        if (p.info?.id === selected) {
          ++selection;
          ++revision;
          turn = undefined;
          emit({ session: selected, status: "unknown", title: "", prompt: "", model: "" });
          selected = undefined;
          failed = false;
        }
        return;
      }
      if (p.sessionID !== selected || !selected) return;
      if (event.type === "session.status") {
        ++revision;
        const status = p.status?.type;
        if (["busy", "retry"].includes(status)) failed = false;
        emit({ session: selected, status: status === "idle" ? (failed ? "error" : "idle") : ["busy", "retry"].includes(status) ? "working" : "unknown" });
        if (status === "idle") await settle();
      } else if (event.type === "permission.asked" || event.type === "question.asked") {
        ++revision;
        emit({ session: selected, status: "waiting" });
      } else if (event.type === "permission.replied" || event.type === "question.replied" || event.type === "question.rejected") {
        ++revision;
        emit({ session: selected, status: "working" });
      } else if (event.type === "session.error") {
        ++revision;
        failed = true;
        emit({ session: selected, status: "error" });
      }
    },
  };
};
