import { describe, expect, it } from "vitest";
import {
  serializeHistory,
  digestHistory,
  rehydrateHistory,
  nextCheckpoint,
  HistoryIntegrityError,
  type SanitizedHistory,
} from "../src/history.js";
import type { HistoryCheckpoint, ModelVersion } from "../src/contracts.js";

const modelVersion: ModelVersion = {
  provider: "fireworks",
  modelId: "accounts/fireworks/models/kimi-k2p5",
  promptVersion: "quotation-v1",
};

function makeHistory(
  entriesCount: number,
  overrides: Partial<SanitizedHistory> = {},
): SanitizedHistory {
  const entries = Array.from({ length: entriesCount }, (_, i) => ({
    role: "user" as const,
    content: `Turn ${i + 1}`,
    modelVersion,
    promptVersion: modelVersion.promptVersion,
  }));

  return {
    version: "novus.ai.v1",
    workflowId: "workflow-001",
    entries,
    modelVersion,
    promptVersion: modelVersion.promptVersion,
    ...overrides,
  };
}

describe("history", () => {
  it("serializeHistory is deterministic", () => {
    const history = makeHistory(2);
    const a = serializeHistory(history);
    const b = serializeHistory(history);
    expect(a).toBe(b);

    // Keys should be sorted alphabetically
    const parsed = JSON.parse(a);
    const keys = Object.keys(parsed);
    expect(keys).toEqual([...keys].sort());
  });

  it("digestHistory is deterministic", () => {
    const history = makeHistory(3);
    const d1 = digestHistory(history);
    const d2 = digestHistory(history);
    expect(d1).toBe(d2);
    // SHA-256 hex is 64 chars
    expect(d1).toHaveLength(64);
  });

  it("digestHistory changes when content changes", () => {
    const a = makeHistory(1, { workflowId: "workflow-a" });
    const b = makeHistory(1, { workflowId: "workflow-b" });
    expect(digestHistory(a)).not.toBe(digestHistory(b));
  });

  it("round-trip rehydrateHistory matches digest", () => {
    const history = makeHistory(2);
    const checkpoint: HistoryCheckpoint = {
      sequence: 2,
      historyDigest: digestHistory(history),
      sanitizedObjectRef: "s3://bucket/obj",
    };

    const rehydrated = rehydrateHistory(checkpoint, history);
    expect(rehydrated.entries).toHaveLength(2);
    expect(rehydrated.modelVersion).toEqual(modelVersion);
  });

  it("tampered history throws HistoryIntegrityError", () => {
    const history = makeHistory(2);
    const checkpoint: HistoryCheckpoint = {
      sequence: 2,
      historyDigest: digestHistory(history),
      sanitizedObjectRef: null,
    };

    // Tamper with an entry
    const tampered: SanitizedHistory = {
      ...history,
      entries: [
        ...history.entries.slice(0, 1),
        { role: "user", content: "INJECTED" },
      ],
    };

    expect(() => rehydrateHistory(checkpoint, tampered)).toThrow(
      HistoryIntegrityError,
    );
  });

  it("sequence mismatch throws HistoryIntegrityError", () => {
    const history = makeHistory(3);
    const checkpoint: HistoryCheckpoint = {
      sequence: 5, // does not match entries.length
      historyDigest: digestHistory(history),
      sanitizedObjectRef: null,
    };

    expect(() => rehydrateHistory(checkpoint, history)).toThrow(
      HistoryIntegrityError,
    );
  });

  it("nextCheckpoint increments sequence and carries digest + object ref", () => {
    const history = makeHistory(3);
    const checkpoint = nextCheckpoint(history, "s3://bucket/next");

    expect(checkpoint.sequence).toBe(3);
    expect(checkpoint.historyDigest).toBe(digestHistory(history));
    expect(checkpoint.sanitizedObjectRef).toBe("s3://bucket/next");
  });

  it("nextCheckpoint with null object ref", () => {
    const history = makeHistory(1);
    const checkpoint = nextCheckpoint(history, null);

    expect(checkpoint.sequence).toBe(1);
    expect(checkpoint.sanitizedObjectRef).toBeNull();
  });

  it("empty history produces valid checkpoint", () => {
    const history = makeHistory(0);
    const checkpoint = nextCheckpoint(history, null);
    expect(checkpoint.sequence).toBe(0);
    expect(checkpoint.historyDigest).toHaveLength(64);

    // Should rehydrate successfully
    const rehydrated = rehydrateHistory(checkpoint, history);
    expect(rehydrated.entries).toHaveLength(0);
  });
});
