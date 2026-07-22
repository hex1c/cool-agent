import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import {
  handler,
  createHandler,
  type HandlerDependencies,
} from "../src/index.js";
import type { AiResponse } from "../src/contracts.js";

const fixtureResponse = (() => {
  const fixturePath = resolve(
    import.meta.dirname,
    "../../../tests/fixtures/contracts/v2.json",
  );
  const raw = readFileSync(fixturePath, "utf-8");
  const parsed = JSON.parse(raw) as { response: Record<string, unknown> };
  return parsed.response;
})();

const request = {
  contractVersion: "novus.ai.v2" as const,
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

function expectErrorResponse(
  response: AiResponse | void,
  outcome: string,
  code: string,
): void {
  if (!response) throw new Error("Handler returned no response");
  expect(response.outcome).toBe(outcome);
  expect(response.error?.code).toBe(code);
}

describe("handler", () => {
  it("default handler fails closed with AI_NOT_CONFIGURED", async () => {
    const response = await handler(request, {} as never, () => undefined);
    expectErrorResponse(response, "invalid", "AI_NOT_CONFIGURED");
    // Must not advance checkpoint
    if (response && response.checkpoint) {
      expect(response.checkpoint.sequence).toBe(request.history.sequence);
    }
  });

  it("with fake returning valid success JSON → outcome success", async () => {
    const deps: HandlerDependencies = {
      runExtraction: async () => JSON.stringify(fixtureResponse),
    };
    const h = createHandler(deps);
    const response = await h(request, {} as never, () => undefined);

    if (!response) throw new Error("Handler returned no response");
    expect(response.outcome).toBe("success");
    expect(response.extraction).not.toBeNull();
    // Checkpoint should have advanced
    expect(response.checkpoint.sequence).toBeGreaterThan(
      request.history.sequence,
    );
  });

  it("with fake returning invalid JSON → outcome invalid", async () => {
    const deps: HandlerDependencies = {
      runExtraction: async () => "not valid json {{{",
    };
    const h = createHandler(deps);
    const response = await h(request, {} as never, () => undefined);

    expectErrorResponse(response, "invalid", "INVALID_OUTPUT");
    // Must not advance checkpoint
    if (response && response.checkpoint) {
      expect(response.checkpoint.sequence).toBe(request.history.sequence);
    }
  });

  it("with fake throwing → outcome retryable_error", async () => {
    const deps: HandlerDependencies = {
      runExtraction: async () => {
        throw new Error("Model timeout");
      },
    };
    const h = createHandler(deps);
    const response = await h(request, {} as never, () => undefined);

    expectErrorResponse(response, "retryable_error", "EXTRACTION_FAILED");
  });

  it("schema-invalid JSON from model → outcome invalid, does not advance", async () => {
    const validBase = JSON.parse(JSON.stringify(fixtureResponse)) as Record<
      string,
      unknown
    >;
    // Remove a required field
    delete validBase["outcome"];
    const deps: HandlerDependencies = {
      runExtraction: async () => JSON.stringify(validBase),
    };
    const h = createHandler(deps);
    const response = await h(request, {} as never, () => undefined);

    expectErrorResponse(response, "invalid", "INVALID_OUTPUT");
    if (response && response.checkpoint) {
      expect(response.checkpoint.sequence).toBe(request.history.sequence);
    }
  });
});
