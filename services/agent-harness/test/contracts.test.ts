import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parseRequest, parseResponse } from "../src/contracts.js";

const fixturePath = fileURLToPath(
  new URL("../../../tests/fixtures/contracts/v1.json", import.meta.url),
);

async function loadFixture(): Promise<unknown> {
  try {
    return JSON.parse(await readFile(fixturePath, "utf8")) as unknown;
  } catch (error: unknown) {
    throw new Error("Unable to load contract fixture", { cause: error });
  }
}

describe("versioned AI contracts", () => {
  it("accepts the shared request and response fixture", async () => {
    const fixture = (await loadFixture()) as {
      request: unknown;
      response: unknown;
    };

    expect(parseRequest(fixture.request).contractVersion).toBe("novus.ai.v1");
    expect(parseResponse(fixture.response).outcome).toBe("success");
  });

  it("rejects an unknown request property", () => {
    expect(() =>
      parseRequest({ contractVersion: "novus.ai.v1", unexpected: true }),
    ).toThrow("Invalid request contract");
  });
});
