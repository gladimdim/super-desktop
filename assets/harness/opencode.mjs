import { report } from "./report.mjs";

export const SuperDesktop = async ({ client }) => {
  let selected;
  let failed = false;
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
    const session = await info(id);
    if (!session || session.parentID) return false;
    if (selected !== id) failed = false;
    selected = id;
    emit({ session: id, title: session.title ?? "" });
    return true;
  };
  emit({ status: "unknown" });
  return {
    "chat.message": async (input, output) => {
      if (!await choose(input.sessionID)) return;
      failed = false;
      emit({ session: selected, status: "working", model: input.model ? `${input.model.providerID}/${input.model.modelID}` : "",
        prompt: output.parts.filter(p => p.type === "text" && !p.synthetic).map(p => p.text).join(" ") });
    },
    event: async ({ event }) => {
      const p = event.properties ?? {};
      if (event.type === "session.created" || event.type === "session.updated") {
        const value = p.info;
        if (!value) return;
        sessions.set(value.id, value);
        if (!selected && !value.parentID) {
          await choose(value.id);
          if (event.type === "session.created") emit({ session: selected, status: "idle" });
        }
        if (value.id === selected) emit({ session: selected, title: value.title ?? "" });
        return;
      }
      if (event.type === "tui.session.select") { await choose(p.sessionID); return; }
      if (p.sessionID !== selected || !selected) return;
      if (event.type === "session.status") {
        const status = p.status?.type;
        if (["busy", "retry"].includes(status)) failed = false;
        emit({ session: selected, status: status === "idle" ? (failed ? "error" : "idle") : ["busy", "retry"].includes(status) ? "working" : "unknown" });
      } else if (event.type === "permission.asked" || event.type === "question.asked") {
        emit({ session: selected, status: "waiting" });
      } else if (event.type === "permission.replied" || event.type === "question.replied" || event.type === "question.rejected") {
        emit({ session: selected, status: "working" });
      } else if (event.type === "session.error") {
        failed = true;
        emit({ session: selected, status: "error" });
      }
    },
  };
};
