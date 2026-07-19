import { afterEach, describe, expect, it } from "vitest";

import { ModelResolutionError, type ModelSpec } from "../src/models.js";
import {
	createExtractionSession,
	type SecretProvider,
} from "../src/session.js";
import {
	createExtractDocumentsTool,
	extractDocumentText,
	type NormalizedDocumentRef,
} from "../src/tools/extract-documents.js";

// A model that is present in the default Pi model catalog. `getModel` does not
// require an API key to exist, so an in-memory credential store is sufficient.
const KNOWN_MODEL: ModelSpec = {
	provider: "openai",
	modelId: "gpt-4o-mini",
	promptVersion: "quotation-v1",
};

const SECRET_REF = "/novus/development/ai/provider-key";
const API_KEY = "sk-test-secret-key-DO-NOT-LEAK-9f3a7c1d";

function fakeSecretProvider(key = API_KEY): SecretProvider {
	return {
		async resolve(): Promise<string> {
			return key;
		},
	};
}

const created: Array<() => void> = [];

afterEach(() => {
	while (created.length > 0) {
		const dispose = created.pop();
		if (dispose) {
			dispose();
		}
	}
});

describe("session-factory", () => {
	it("uses SessionManager.inMemory() and writes no durable session file", async () => {
		const { session, dispose } = await createExtractionSession({
			modelSpec: KNOWN_MODEL,
			secretRef: SECRET_REF,
			secretProvider: fakeSecretProvider(),
			documents: [],
		});
		created.push(dispose);

		// In-memory sessions have no on-disk session file.
		expect(session.sessionFile).toBeUndefined();
	});

	it("enables only the extract_documents tool and no built-in mutation/external tools", async () => {
		const { session, dispose } = await createExtractionSession({
			modelSpec: KNOWN_MODEL,
			secretRef: SECRET_REF,
			secretProvider: fakeSecretProvider(),
			documents: [],
		});
		created.push(dispose);

		const toolNames = session.agent.state.tools.map((tool) => tool.name);
		expect(toolNames).toContain("extract_documents");
		// No shell, filesystem, or external-service built-ins.
		for (const forbidden of [
			"bash",
			"edit",
			"write",
			"read",
			"grep",
			"find",
			"ls",
		]) {
			expect(toolNames).not.toContain(forbidden);
		}
		// The extraction tool is the *only* enabled tool.
		expect(toolNames).toEqual(["extract_documents"]);
	});

	it("resolves the API key at runtime via the secret provider and keeps it out of the prompt", async () => {
		let resolvedRef: string | undefined;
		const provider: SecretProvider = {
			async resolve(secretRef: string): Promise<string> {
				resolvedRef = secretRef;
				return API_KEY;
			},
		};

		const { session, dispose } = await createExtractionSession({
			modelSpec: KNOWN_MODEL,
			secretRef: SECRET_REF,
			secretProvider: provider,
			documents: [],
		});
		created.push(dispose);

		// The secret provider was invoked with the configured reference at runtime.
		expect(resolvedRef).toBe(SECRET_REF);

		// The plaintext key must never appear in the system prompt.
		const systemPrompt = session.agent.state.systemPrompt ?? "";
		expect(systemPrompt).not.toContain(API_KEY);
	});

	it("rejects an unregistered model instead of falling back", async () => {
		// A provider/model pair that is not in the catalog.
		await expect(
			createExtractionSession({
				modelSpec: {
					provider: "nope",
					modelId: "no-such-model",
					promptVersion: "v1",
				},
				secretRef: SECRET_REF,
				secretProvider: fakeSecretProvider(),
				documents: [],
			}),
		).rejects.toBeInstanceOf(ModelResolutionError);
	});

	it("rejects an empty API key from the secret provider", async () => {
		await expect(
			createExtractionSession({
				modelSpec: KNOWN_MODEL,
				secretRef: SECRET_REF,
				secretProvider: fakeSecretProvider(""),
				documents: [],
			}),
		).rejects.toThrow(/empty AI provider key/);
	});
});

describe("extract_documents tool", () => {
	it("returns pre-normalized text for requested references and reports unavailable ones", () => {
		const documents: NormalizedDocumentRef[] = [
			{
				objectRef: "raw/wf-1/doc-a",
				mediaType: "text/csv",
				extractedText: "a,b,c\n1,2,3",
			},
			{
				objectRef: "raw/wf-1/doc-b",
				mediaType: "application/pdf",
				extractedText: null,
			},
		];

		const text = extractDocumentText(documents, [
			"raw/wf-1/doc-a",
			"raw/wf-1/doc-b",
			"raw/wf-1/missing",
		]);

		expect(text).toContain("a,b,c\n1,2,3");
		expect(text).toContain("raw/wf-1/doc-b: no normalized text available");
		expect(text).toContain("raw/wf-1/missing: no normalized text available");
	});

	it("builds a tool with the expected name and a JSON-schema parameter shape", () => {
		const tool = createExtractDocumentsTool([]);
		expect(tool.name).toBe("extract_documents");
		expect(tool.parameters).toMatchObject({
			type: "object",
			properties: { object_refs: { type: "array" } },
		});
	});
});
