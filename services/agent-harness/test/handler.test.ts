import { describe, expect, it } from "vitest";
import { handler } from "../src/index.js";

const request = {
  contractVersion: "novus.ai.v1" as const,
  requestId: "request-001",
  workflowId: "workflow-001",
  operation: "extraction" as const,
  model: {
    provider: "fireworks",
    modelId: "accounts/fireworks/models/kimi-k2p5",
    promptVersion: "quotation-v1",
  },
  history: {
    sequence: 0,
    historyDigest: "sha256:fixture",
    sanitizedObjectRef: null,
  },
  input: { instruction: "Extract this.", attachments: [] },
};

describe("foundation handler", () => {
  it("returns a schema-shaped fail-closed response without mutation tools", async () => {
    const response = await handler(request, {} as never, () => undefined);

    if (!response) {
      throw new Error("Handler returned no response");
    }
    expect(response.outcome).toBe("invalid");
    expect(response.error?.code).toBe("NOT_IMPLEMENTED");
  });
});
