// Deterministic local provider for installed-Pi lifecycle tests. No HTTP or keys.
import { createAssistantMessageEventStream } from "@earendil-works/pi-ai";

export default function probe(pi) {
  pi.registerProvider("sd-probe", {
    api: "sd-probe-api", apiKey: "local-fixture", baseUrl: "http://127.0.0.1/unused",
    models: [{ id: "fixture", name: "Fixture", reasoning: false, input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 32000, maxTokens: 1000 }],
    streamSimple(model, context) {
      const stream = createAssistantMessageEventStream();
      const user = context.messages.filter(m => m.role === "user").at(-1);
      const text = typeof user?.content === "string" ? user.content : (user?.content ?? []).map(p => p.text ?? "").join("");
      const failed = text.includes("probe-error");
      const message = { role: "assistant", api: model.api, provider: model.provider, model: model.id,
        content: failed ? [] : [{ type: "text", text: "Fixture response" }],
        stopReason: failed ? "error" : "stop", ...(failed ? { errorMessage: "Deliberate fixture failure" } : {}),
        timestamp: Date.now(), usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } } };
      queueMicrotask(() => {
        stream.push(failed ? { type: "error", reason: "error", error: message } : { type: "done", reason: "stop", message });
        stream.end();
      });
      return stream;
    },
  });
}
