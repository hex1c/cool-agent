import type { AiResponse, Operation, TypedError } from "./contracts.js";

/**
 * Result of business-rule validation. JSON Schema (parseResponse) already
 * checked structural shape; this layer enforces semantic invariants the
 * schema cannot express (non-negativity, operation-appropriate non-null
 * result fields, etc.).
 */
export type ValidationResult =
  | { readonly valid: true }
  | { readonly valid: false; readonly errors: string[] };

/**
 * Raised when business-rule validation detects an unrecoverable semantic
 * violation. Callers may prefer the returned `ValidationResult` union;
 * this class exists for explicit throw/catch patterns.
 */
export class BusinessRuleError extends Error {
  public readonly errors: string[];

  public constructor(errors: string[]) {
    super(`Business rule validation failed: ${errors.join("; ")}`);
    this.name = "BusinessRuleError";
    this.errors = errors;
  }
}

const OPERATION_REQUIRED_FIELD = {
  extraction: "extraction",
  quotation_calculation: "quotation",
  draft: "draft",
  calendar: "calendar",
} as const satisfies Record<
  Operation,
  keyof Pick<AiResponse, "extraction" | "quotation" | "draft" | "calendar">
>;

const CURRENCY_RE = /^[A-Z]{3}$/;

/**
 * Validate business rules on top of an already schema-validated AiResponse.
 *
 * - Non-success outcomes are valid-but-not-success (caller must not advance
 *   state). No further checks are run.
 * - For success, the operation-appropriate result field must be non-null
 *   (when `operation` is supplied), and each populated result block is
 *   checked for semantic invariants.
 */
export function validateExtraction(
  response: AiResponse,
  operation?: Operation,
): ValidationResult {
  if (response.outcome !== "success") {
    return { valid: true };
  }

  const errors: string[] = [];

  // If an operation is supplied, the corresponding result field must be
  // non-null on success.
  if (operation) {
    const requiredField = OPERATION_REQUIRED_FIELD[operation];
    if (response[requiredField] == null) {
      errors.push(
        `Operation "${operation}" requires "${requiredField}" to be non-null on success`,
      );
    }
  }

  // Per-result-block business rules. Each block is only checked when
  // non-null (the schema already validated structural shape).

  if (response.extraction != null) {
    validateExtractionBlock(response.extraction, errors);
  }

  if (response.quotation != null) {
    validateQuotationBlock(response.quotation, errors);
  }

  if (response.draft != null) {
    validateDraftBlock(response.draft, errors);
  }

  if (response.calendar != null) {
    validateCalendarBlock(response.calendar, errors);
  }

  if (response.error != null) {
    validateErrorBlock(response.error, errors);
  }

  if (errors.length > 0) {
    return { valid: false, errors };
  }
  return { valid: true };
}

function validateExtractionBlock(
  extraction: Record<string, unknown>,
  errors: string[],
): void {
  const items = extraction["items"];
  if (!Array.isArray(items) || items.length === 0) {
    errors.push("extraction.items must be a non-empty array");
    return;
  }

  const currency = extraction["currency"];
  if (typeof currency !== "string" || !CURRENCY_RE.test(currency)) {
    errors.push("extraction.currency must match ^[A-Z]{3}$");
  }

  for (let i = 0; i < items.length; i++) {
    const item = items[i] as Record<string, unknown> | undefined;
    if (!item) continue;

    if (typeof item["quantity"] !== "string" || item["quantity"].length === 0) {
      errors.push(`extraction.items[${i}].quantity must be a non-empty string`);
    }

    const price = item["unitPriceMicroInr"];
    if (typeof price !== "number" || !Number.isInteger(price) || price < 0) {
      errors.push(
        `extraction.items[${i}].unitPriceMicroInr must be a non-negative integer`,
      );
    }
  }
}

function validateQuotationBlock(
  quotation: Record<string, unknown>,
  errors: string[],
): void {
  const currency = quotation["currency"];
  if (typeof currency !== "string" || !CURRENCY_RE.test(currency)) {
    errors.push("quotation.currency must match ^[A-Z]{3}$");
  }

  const fields = ["subtotalMicroInr", "taxMicroInr", "totalMicroInr"] as const;
  for (const field of fields) {
    const value = quotation[field];
    if (typeof value !== "number" || !Number.isInteger(value) || value < 0) {
      errors.push(`quotation.${field} must be a non-negative integer`);
    }
  }

  const taxRateBps = quotation["taxRateBps"];
  if (
    typeof taxRateBps !== "number" ||
    !Number.isInteger(taxRateBps) ||
    taxRateBps < 0 ||
    taxRateBps > 10000
  ) {
    errors.push("quotation.taxRateBps must be an integer between 0 and 10000");
  }

  const assumptions = quotation["assumptions"];
  if (!Array.isArray(assumptions)) {
    errors.push("quotation.assumptions must be an array");
  } else {
    for (let i = 0; i < assumptions.length; i++) {
      if (typeof assumptions[i] !== "string") {
        errors.push(`quotation.assumptions[${i}] must be a string`);
      }
    }
  }
}

function validateDraftBlock(
  draft: Record<string, unknown>,
  errors: string[],
): void {
  if (typeof draft["subject"] !== "string" || draft["subject"].length === 0) {
    errors.push("draft.subject must be a non-empty string");
  }
  if (typeof draft["body"] !== "string" || draft["body"].length === 0) {
    errors.push("draft.body must be a non-empty string");
  }
}

function validateCalendarBlock(
  calendar: Record<string, unknown>,
  errors: string[],
): void {
  const fields = ["title", "start", "end", "timezone"] as const;
  for (const field of fields) {
    const value = calendar[field];
    if (typeof value !== "string" || value.length === 0) {
      errors.push(`calendar.${field} must be a non-empty string`);
    }
  }

  const attendees = calendar["attendees"];
  if (!Array.isArray(attendees)) {
    errors.push("calendar.attendees must be an array");
  }

  if (typeof calendar["sendInvitations"] !== "boolean") {
    errors.push("calendar.sendInvitations must be a boolean");
  }
}

function validateErrorBlock(error: TypedError, errors: string[]): void {
  if (error.code.length === 0) {
    errors.push("error.code must be a non-empty string");
  }
  if (error.message.length === 0) {
    errors.push("error.message must be a non-empty string");
  }
}
