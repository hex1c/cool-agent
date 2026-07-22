import type { Handler } from "aws-lambda";
import type { AiRequest, AiResponse, Operation } from "./contracts.js";
import {
  parseRequest,
  parseResponse,
  ContractValidationError,
} from "./contracts.js";
import { validateExtraction } from "./validation.js";
import {
  type SanitizedHistory,
  type SanitizedHistoryEntry,
  nextCheckpoint,
} from "./history.js";
import { createExtractionSession, type SecretProvider } from "./session.js";
import type { ModelSpec } from "./models.js";
import type { NormalizedDocumentRef } from "./tools/extract-documents.js";

// ---------------------------------------------------------------------------
// Handler dependencies — injectable for tests
// ---------------------------------------------------------------------------

/**
 * Injectable dependencies for the Lambda handler. Tests inject a fake
 * `runExtraction`; production uses the default implementation that creates
 * a real Pi extraction session.
 */
export interface HandlerDependencies {
  readonly runExtraction: (
    request: AiRequest,
    documents: readonly NormalizedDocumentRef[],
  ) => Promise<string>;
}

// ---------------------------------------------------------------------------
// Environment-backed secret provider (stub — the Lambda adapter wires AWS
// Secrets Manager later).
// ---------------------------------------------------------------------------

const DEFAULT_SECRET_REF = "NOVUS_AI_PROVIDER_KEY";

class EnvironmentSecretProvider implements SecretProvider {
  async resolve(secretRef: string): Promise<string> {
    const value = process.env[secretRef];
    if (!value) {
      throw new Error(`Secret not configured: ${secretRef}`);
    }
    return value;
  }
}

// ---------------------------------------------------------------------------
// Default extraction runner (production path)
// ---------------------------------------------------------------------------

async function defaultRunExtraction(
  request: AiRequest,
  documents: readonly NormalizedDocumentRef[],
): Promise<string> {
  const modelSpec: ModelSpec = {
    provider: request.model.provider,
    modelId: request.model.modelId,
    promptVersion: request.model.promptVersion,
  };

  const secretProvider = new EnvironmentSecretProvider();
  const { session, dispose } = await createExtractionSession({
    modelSpec,
    secretRef: DEFAULT_SECRET_REF,
    secretProvider,
    documents,
  });

  try {
    const promptText = buildPrompt(request);
    await session.prompt(promptText, { images: [] });

    const messages = session.agent.state.messages;
    const lastAssistant = [...messages]
      .reverse()
      .find((m) => m.role === "assistant");
    if (!lastAssistant) {
      throw new Error("No assistant response received");
    }

    const content =
      typeof lastAssistant.content === "string"
        ? lastAssistant.content
        : JSON.stringify(lastAssistant.content);
    return content;
  } finally {
    dispose();
  }
}

function buildPrompt(request: AiRequest): string {
  const attachmentLines =
    request.input.attachments.length > 0
      ? `\n\nAvailable documents:\n${request.input.attachments.map((a) => `- ${a.objectRef} (${a.mediaType})`).join("\n")}`
      : "";
  return `${request.input.instruction}${attachmentLines}\n\nRespond with valid JSON only.`;
}

// ---------------------------------------------------------------------------
// Handler factory
// ---------------------------------------------------------------------------

/**
 * Operation → required response field mapping used for building a minimal
 * checkpoint history entry.
 */
const OPERATION_LABEL: Record<Operation, string> = {
  extraction: "extraction",
  quotation_calculation: "quotation_calculation",
  draft: "draft",
  calendar: "calendar",
  email: "email",
};

/**
 * Build a fail-closed error response. The checkpoint is NOT advanced
 * (the original request checkpoint is returned unchanged).
 */
function errorResponse(
  request: AiRequest,
  outcome: AiResponse["outcome"],
  code: string,
  message: string,
  retryable: boolean,
): AiResponse {
  return {
    contractVersion: request.contractVersion,
    requestId: request.requestId,
    outcome,
    model: request.model,
    checkpoint: request.history,
    extraction: null,
    quotation: null,
    draft: null,
    calendar: null,
    email: null,
    error: { code, message, retryable },
  };
}

/**
 * Build a success response. The checkpoint is advanced via `nextCheckpoint`
 * based on a minimal single-turn history for this request.
 */
function successResponse(request: AiRequest, response: AiResponse): AiResponse {
  const history = buildMinimalHistory(request);
  const checkpoint = nextCheckpoint(history, null);

  return {
    contractVersion: request.contractVersion,
    requestId: request.requestId,
    outcome: response.outcome,
    model: request.model,
    checkpoint,
    extraction: response.extraction,
    quotation: response.quotation,
    draft: response.draft,
    calendar: response.calendar,
    email: response.email,
    error: response.error,
  };
}

/**
 * Build a minimal `SanitizedHistory` from the request for checkpoint
 * advancement. Real history persistence is the Rust adapter's job; this
 * provides a consistent local checkpoint so the contract holds.
 */
function buildMinimalHistory(request: AiRequest): SanitizedHistory {
  const entry: SanitizedHistoryEntry = {
    role: "user",
    content: `[${OPERATION_LABEL[request.operation]}] ${request.input.instruction}`,
    modelVersion: request.model,
    promptVersion: request.model.promptVersion,
  };
  return {
    version: request.contractVersion,
    workflowId: request.workflowId,
    entries: [entry],
    modelVersion: request.model,
    promptVersion: request.model.promptVersion,
  };
}

/**
 * Create a Lambda handler for AiRequest → AiResponse.
 *
 * When called without arguments, the handler uses the default production
 * runner that creates a real Pi extraction session with an
 * environment-backed secret provider. Tests can inject a fake
 * `runExtraction` to avoid real model calls.
 */
export function createHandler(
  deps?: HandlerDependencies,
): Handler<AiRequest, AiResponse> {
  const runExtraction = deps?.runExtraction ?? defaultRunExtraction;

  return async (event) => {
    const request = parseRequest(event);

    const documents: NormalizedDocumentRef[] = request.input.attachments.map(
      (a) => ({
        objectRef: a.objectRef,
        mediaType: a.mediaType,
        extractedText: a.extractedText,
      }),
    );

    // --- Run the model ---
    let rawJson: string;
    try {
      rawJson = await runExtraction(request, documents);
    } catch (err) {
      return handleExtractionError(request, err);
    }

    // --- Parse JSON ---
    let parsed: unknown;
    try {
      parsed = JSON.parse(rawJson);
    } catch {
      return errorResponse(
        request,
        "invalid",
        "INVALID_OUTPUT",
        "Model returned invalid JSON",
        false,
      );
    }

    // --- Schema validation ---
    let response: AiResponse;
    try {
      response = parseResponse(parsed);
    } catch (err) {
      if (err instanceof ContractValidationError) {
        return errorResponse(
          request,
          "invalid",
          "INVALID_OUTPUT",
          "Response failed schema validation",
          false,
        );
      }
      throw err;
    }

    // --- Business-rule validation ---
    const validation = validateExtraction(response, request.operation);
    if (!validation.valid) {
      return errorResponse(
        request,
        "invalid",
        "INVALID_OUTPUT",
        validation.errors.join("; "),
        false,
      );
    }

    // --- Build final response ---
    if (response.outcome !== "success") {
      // Non-success: return the model's response but do NOT advance the
      // checkpoint (the caller must not advance state).
      return {
        contractVersion: request.contractVersion,
        requestId: request.requestId,
        outcome: response.outcome,
        model: request.model,
        checkpoint: request.history,
        extraction: response.extraction,
        quotation: response.quotation,
        draft: response.draft,
        calendar: response.calendar,
        email: response.email,
        error: response.error,
      };
    }

    return successResponse(request, response);
  };
}

/**
 * Map an extraction error to the appropriate fail-closed response.
 *
 * Known non-retryable cases (missing configuration) produce
 * `terminal_error` with a sanitized code. Everything else is treated as
 * retryable. The raw error text is never leaked into the response message.
 */
function handleExtractionError(request: AiRequest, err: unknown): AiResponse {
  const message = err instanceof Error ? err.message : "Unknown error";

  if (
    message.includes("Secret not configured") ||
    message.includes("not registered")
  ) {
    return errorResponse(
      request,
      "invalid",
      "AI_NOT_CONFIGURED",
      "AI model or key not configured",
      false,
    );
  }

  return errorResponse(
    request,
    "retryable_error",
    "EXTRACTION_FAILED",
    "Extraction failed",
    true,
  );
}

// ---------------------------------------------------------------------------
// Default handler (Lambda entry point)
// ---------------------------------------------------------------------------

export const handler = createHandler();
