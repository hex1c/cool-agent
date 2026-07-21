import { createHash } from "node:crypto";
import type { HistoryCheckpoint, ModelVersion } from "./contracts.js";

/**
 * A single ordered turn in a sanitized conversation history.
 */
export interface SanitizedHistoryEntry {
  readonly role: "user" | "assistant" | "tool";
  readonly content: string;
  readonly modelVersion?: ModelVersion;
  readonly promptVersion?: string;
}

/**
 * A complete sanitized conversation history for one workflow.
 *
 * All entries are ordered (oldest first). The top-level `modelVersion`
 * and `promptVersion` record the configuration that produced the most
 * recent assistant turn; individual entries may carry their own version
 * stamps for auditability.
 */
export interface SanitizedHistory {
  readonly version: string;
  readonly workflowId: string;
  readonly entries: readonly SanitizedHistoryEntry[];
  readonly modelVersion: ModelVersion;
  readonly promptVersion: string;
}

/**
 * The result of rehydrating a history from a stored checkpoint.
 */
export interface RehydratedHistory {
  readonly entries: readonly SanitizedHistoryEntry[];
  readonly modelVersion: ModelVersion;
  readonly promptVersion: string;
}

/**
 * Raised when a history checkpoint fails integrity verification — the
 * digest does not match or the sequence count is inconsistent.
 */
export class HistoryIntegrityError extends Error {
  public constructor(reason: string) {
    super(`History integrity check failed: ${reason}`);
    this.name = "HistoryIntegrityError";
  }
}

/**
 * Deterministically serialize a sanitized history to a JSON string.
 *
 * Keys are sorted for stability; the exact byte representation is used
 * as the pre-image for the SHA-256 digest.
 */
export function serializeHistory(history: SanitizedHistory): string {
  return JSON.stringify(history, sortedKeysReplacer);
}

/**
 * Sorted-keys JSON replacer: produces objects with lexicographically
 * ordered keys so serialization is deterministic regardless of insertion
 * order.
 */
function sortedKeysReplacer(_key: string, value: unknown): unknown {
  if (value != null && typeof value === "object" && !Array.isArray(value)) {
    const sorted: Record<string, unknown> = {};
    const keys = Object.keys(value).sort();
    for (const k of keys) {
      sorted[k] = (value as Record<string, unknown>)[k];
    }
    return sorted;
  }
  return value;
}

/**
 * Compute the SHA-256 hex digest of the deterministic serialized form
 * of `history`. This digest is stored in the checkpoint and verified on
 * rehydration.
 */
export function digestHistory(history: SanitizedHistory): string {
  const serialized = serializeHistory(history);
  return createHash("sha256").update(serialized).digest("hex");
}

/**
 * Rehydrate a history from a stored checkpoint.
 *
 * Verifies:
 * 1. The SHA-256 digest of `history` matches `checkpoint.historyDigest`.
 * 2. `checkpoint.sequence` equals `history.entries.length` (each turn
 *    advances the sequence by one).
 *
 * Throws `HistoryIntegrityError` on any mismatch.
 */
export function rehydrateHistory(
  checkpoint: HistoryCheckpoint,
  history: SanitizedHistory,
): RehydratedHistory {
  const computed = digestHistory(history);
  if (computed !== checkpoint.historyDigest) {
    throw new HistoryIntegrityError(
      `digest mismatch: expected ${checkpoint.historyDigest}, computed ${computed}`,
    );
  }

  if (history.entries.length !== checkpoint.sequence) {
    throw new HistoryIntegrityError(
      `sequence mismatch: checkpoint has ${checkpoint.sequence}, history has ${history.entries.length} entries`,
    );
  }

  return {
    entries: history.entries,
    modelVersion: history.modelVersion,
    promptVersion: history.promptVersion,
  };
}

/**
 * Build the next checkpoint after a successful turn.
 *
 * `sequence` is set to the current number of history entries (before
 * appending the new turn — the caller increments `entries` before
 * persisting). `historyDigest` captures the full current state.
 */
export function nextCheckpoint(
  history: SanitizedHistory,
  sanitizedObjectRef: string | null,
): HistoryCheckpoint {
  return {
    sequence: history.entries.length,
    historyDigest: digestHistory(history),
    sanitizedObjectRef,
  };
}
