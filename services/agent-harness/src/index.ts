import type { Handler } from "aws-lambda";
import { parseRequest, type AiRequest, type AiResponse } from "./contracts.js";

export const handler: Handler<AiRequest, AiResponse> = async (event) => {
  const request = parseRequest(event);

  return {
    contractVersion: request.contractVersion,
    requestId: request.requestId,
    outcome: "invalid",
    model: request.model,
    checkpoint: request.history,
    extraction: null,
    quotation: null,
    draft: null,
    calendar: null,
    error: {
      code: "NOT_IMPLEMENTED",
      message: "Agent execution is not enabled in the foundation scaffold.",
      retryable: false,
    },
  };
};
