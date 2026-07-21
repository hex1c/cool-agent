import { describe, expect, it } from "vitest";
import { ContractValidationError, parseResponse } from "../src/contracts.js";
import { validateExtraction, BusinessRuleError } from "../src/validation.js";
import type { AiResponse } from "../src/contracts.js";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const fixtureResponse = (() => {
  const fixturePath = resolve(
    import.meta.dirname,
    "../../../tests/fixtures/contracts/v1.json",
  );
  const raw = readFileSync(fixturePath, "utf-8");
  const parsed = JSON.parse(raw) as { response: Record<string, unknown> };
  return parsed.response;
})();

function successResponse(
  overrides: Partial<Record<string, unknown>> = {},
): AiResponse {
  return parseResponse({
    ...fixtureResponse,
    ...overrides,
  });
}

describe("extraction", () => {
  it("valid success response passes validation", () => {
    const response = successResponse();
    const result = validateExtraction(response, "extraction");
    expect(result.valid).toBe(true);
  });

  it("success missing the operation-required result is invalid", () => {
    // extraction operation but extraction field is null
    const response = successResponse({
      extraction: null,
    });
    const result = validateExtraction(response, "extraction");
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.errors.some((e) => e.includes("extraction"))).toBe(true);
    }
  });

  it("negative unitPriceMicroInr is invalid", () => {
    const response = successResponse({
      extraction: {
        customer: { name: "Test", address: null, contact: null },
        items: [
          {
            description: "Test item",
            quantity: "1",
            unitPriceMicroInr: -100,
          },
        ],
        currency: "INR",
      },
    });
    const result = validateExtraction(response, "extraction");
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.errors.some((e) => e.includes("unitPriceMicroInr"))).toBe(
        true,
      );
    }
  });

  it("bad currency is invalid (business-rule check)", () => {
    // Bypass schema validation: the schema also catches bad currency,
    // but business rules double-check it for defense in depth.
    const response = successResponse();
    const badResponse = {
      ...response,
      extraction: {
        customer: { name: "Test", address: null, contact: null },
        items: [{ description: "Test", quantity: "1", unitPriceMicroInr: 100 }],
        currency: "inr",
      },
    } as AiResponse;
    const result = validateExtraction(badResponse, "extraction");
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.errors.some((e) => e.includes("currency"))).toBe(true);
    }
  });

  it("non-success outcome is valid-but-not-success", () => {
    const response = successResponse({
      outcome: "invalid",
      extraction: null,
      error: { code: "E1", message: "Something", retryable: false },
    });
    const result = validateExtraction(response, "extraction");
    expect(result.valid).toBe(true);
  });

  it("schema-invalid JSON throws ContractValidationError", () => {
    expect(() => parseResponse({ notAValidResponse: true })).toThrow(
      ContractValidationError,
    );
  });

  it("BusinessRuleError carries errors array", () => {
    const err = new BusinessRuleError(["bad thing"]);
    expect(err.errors).toEqual(["bad thing"]);
    expect(err.message).toContain("bad thing");
  });

  it("quotation_calculation operation requires quotation non-null", () => {
    const response = successResponse({
      extraction: null,
      quotation: null,
    });
    const result = validateExtraction(response, "quotation_calculation");
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.errors.some((e) => e.includes("quotation"))).toBe(true);
    }
  });

  it("valid quotation passes validation", () => {
    const response = successResponse({
      extraction: null,
      quotation: {
        currency: "INR",
        subtotalMicroInr: 1000000,
        taxMicroInr: 180000,
        totalMicroInr: 1180000,
        taxRateBps: 1800,
        assumptions: ["Standard rates"],
      },
    });
    const result = validateExtraction(response, "quotation_calculation");
    expect(result.valid).toBe(true);
  });

  it("negative quotation amounts are invalid", () => {
    const response = successResponse({
      extraction: null,
      quotation: {
        currency: "INR",
        subtotalMicroInr: -1,
        taxMicroInr: 0,
        totalMicroInr: 0,
        taxRateBps: 0,
        assumptions: [],
      },
    });
    const result = validateExtraction(response, "quotation_calculation");
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.errors.some((e) => e.includes("subtotalMicroInr"))).toBe(
        true,
      );
    }
  });

  it("draft operation requires draft non-null", () => {
    const response = successResponse({
      extraction: null,
      draft: null,
    });
    const result = validateExtraction(response, "draft");
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.errors.some((e) => e.includes("draft"))).toBe(true);
    }
  });

  it("valid draft passes validation", () => {
    const response = successResponse({
      extraction: null,
      draft: {
        subject: "Test draft",
        body: "Hello world",
      },
    });
    const result = validateExtraction(response, "draft");
    expect(result.valid).toBe(true);
  });

  it("calendar operation requires calendar non-null", () => {
    const response = successResponse({
      extraction: null,
      calendar: null,
    });
    const result = validateExtraction(response, "calendar");
    expect(result.valid).toBe(false);
    if (!result.valid) {
      expect(result.errors.some((e) => e.includes("calendar"))).toBe(true);
    }
  });

  it("valid calendar passes validation", () => {
    const response = successResponse({
      extraction: null,
      calendar: {
        title: "Meeting",
        start: "2025-01-01T00:00:00Z",
        end: "2025-01-01T01:00:00Z",
        timezone: "UTC",
        attendees: ["user@example.com"],
        sendInvitations: false,
      },
    });
    const result = validateExtraction(response, "calendar");
    expect(result.valid).toBe(true);
  });
});
