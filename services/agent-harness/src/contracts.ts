import * as ajvFormats from "ajv-formats";
import { Ajv2020, type ValidateFunction } from "ajv/dist/2020.js";
import requestSchema from "../../../config/schema/ai-request.schema.json" with { type: "json" };
import responseSchema from "../../../config/schema/ai-response.schema.json" with { type: "json" };

export const CONTRACT_VERSION = "novus.ai.v1" as const;

export type Operation =
  "extraction" | "quotation_calculation" | "draft" | "calendar";

export interface ModelVersion {
  provider: string;
  modelId: string;
  promptVersion: string;
}

export interface HistoryCheckpoint {
  sequence: number;
  historyDigest: string;
  sanitizedObjectRef: string | null;
}

export interface AttachmentInput {
  objectRef: string;
  mediaType: string;
  checksum: string;
  extractedText: string | null;
}

export interface AiRequest {
  contractVersion: typeof CONTRACT_VERSION;
  requestId: string;
  workflowId: string;
  operation: Operation;
  model: ModelVersion;
  history: HistoryCheckpoint;
  input: { instruction: string; attachments: AttachmentInput[] };
}

export interface TypedError {
  code: string;
  message: string;
  retryable: boolean;
}

export interface AiResponse {
  contractVersion: typeof CONTRACT_VERSION;
  requestId: string;
  outcome: "success" | "invalid" | "retryable_error" | "terminal_error";
  model: ModelVersion;
  checkpoint: HistoryCheckpoint;
  extraction: Record<string, unknown> | null;
  quotation: Record<string, unknown> | null;
  draft: Record<string, unknown> | null;
  calendar: Record<string, unknown> | null;
  error: TypedError | null;
}

type FormatsPlugin = (validator: Ajv2020) => Ajv2020;
const addFormats = ajvFormats.default as unknown as FormatsPlugin;
const ajv = new Ajv2020({ allErrors: true, strict: true });
addFormats(ajv);
const requestValidator: ValidateFunction<AiRequest> = ajv.compile<AiRequest>(
  requestSchema as object,
);
const responseValidator: ValidateFunction<AiResponse> = ajv.compile<AiResponse>(
  responseSchema as object,
);

export function parseRequest(value: unknown): AiRequest {
  if (!requestValidator(value)) {
    throw new ContractValidationError("request", requestValidator.errors);
  }
  return value;
}

export function parseResponse(value: unknown): AiResponse {
  if (!responseValidator(value)) {
    throw new ContractValidationError("response", responseValidator.errors);
  }
  return value;
}

export class ContractValidationError extends Error {
  public readonly errors: unknown;

  public constructor(contract: string, errors: unknown) {
    super(`Invalid ${contract} contract`);
    this.name = "ContractValidationError";
    this.errors = errors;
  }
}
