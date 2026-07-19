import {
	createAgentSession,
	type AgentSession,
	ModelRuntime,
	SessionManager,
} from "@earendil-works/pi-coding-agent";
import { InMemoryCredentialStore } from "@earendil-works/pi-ai";

import { resolveModel, type ModelSpec, type ResolvedModel } from "./models.js";
import {
	createExtractDocumentsTool,
	extractDocumentsToolName,
	type NormalizedDocumentRef,
} from "./tools/extract-documents.js";

/**
 * Resolves a secret reference to its plaintext value at runtime. This mirrors
 * the Rust `SecretProvider` capability: the Lambda adapter reads from AWS
 * Secrets Manager / SSM; tests inject a fake. The resolved value is an API
 * key and must never be persisted, logged, or placed in a prompt.
 */
export interface SecretProvider {
	resolve(secretRef: string): Promise<string>;
}

/**
 * Configuration for one in-memory extraction session.
 */
export interface ExtractionSessionOptions {
	/** The configured AI model selection. */
	readonly modelSpec: ModelSpec;
	/** Secret reference for the provider API key (e.g. `/novus/.../ai/provider-key`). */
	readonly secretRef: string;
	/** Runtime secret provider — the API key is resolved through this, never read from disk. */
	readonly secretProvider: SecretProvider;
	/** Normalized documents made available to the extraction tool. */
	readonly documents: readonly NormalizedDocumentRef[];
}

/**
 * A live in-memory Pi extraction session and its cleanup handle.
 */
export interface ExtractionSession {
	readonly session: AgentSession;
	readonly model: ResolvedModel;
	/** Dispose the session and release process-memory resources. */
	readonly dispose: () => void;
}

/**
 * Built-in tool names that mutate the filesystem or shell or reach external
 * services. These must never be present on an extraction session.
 */
const FORBIDDEN_BUILTIN_TOOLS = new Set([
	"bash",
	"edit",
	"write",
	"read",
	"grep",
	"find",
	"ls",
]);

/**
 * Create an in-memory Pi session configured with the selected model, no
 * built-in mutation/external tools, and a single narrowly scoped
 * `extract_documents` tool.
 *
 * # Safety boundary
 *
 * - `SessionManager.inMemory()` is used; process memory is not treated as
 *   durable and no session file is written.
 * - `noTools: "builtin"` disables every built-in tool (shell, filesystem,
 *   external-service). The only enabled tool is `extract_documents`, which
 *   reads solely from an in-memory document map.
 * - Model credentials are resolved at runtime through the supplied
 *   `SecretProvider` and injected via `ModelRuntime.setRuntimeApiKey` into an
 *   `InMemoryCredentialStore`. The key is never written to disk, never added
 *   to a prompt, and never logged.
 */
export async function createExtractionSession(
	options: ExtractionSessionOptions,
): Promise<ExtractionSession> {
	// Resolve the API key at runtime through the secret provider. The plaintext
	// key lives only in this function's scope and the runtime's in-memory auth.
	const apiKey = await options.secretProvider.resolve(options.secretRef);
	if (!apiKey) {
		throw new Error("secret provider returned an empty AI provider key");
	}

	// Empty in-memory credential store: do not read ~/.pi/agent/auth.json or
	// environment variables in the Lambda process. Auth is runtime-injected only.
	const credentials = new InMemoryCredentialStore();
	const modelRuntime = await ModelRuntime.create({ credentials });
	modelRuntime.setRuntimeApiKey(options.modelSpec.provider, apiKey);

	const model = resolveModel(modelRuntime, options.modelSpec);
	const extractTool = createExtractDocumentsTool(options.documents);

	const { session } = await createAgentSession({
		sessionManager: SessionManager.inMemory(),
		modelRuntime,
		model,
		// Disable every built-in tool (shell, filesystem, external services).
		noTools: "builtin",
		// Enable only the narrowly scoped extraction tool.
		customTools: [extractTool],
		tools: [extractDocumentsToolName],
	});

	assertSafetyBoundary(session);

	return {
		session,
		model,
		dispose: () => session.dispose(),
	};
}

/**
 * Verify the safety boundary holds on the constructed session: no forbidden
 * built-in tools are present, and the resolved API key has not leaked into the
 * system prompt.
 */
function assertSafetyBoundary(session: AgentSession): void {
	const toolNames = session.agent.state.tools.map((tool) => tool.name);
	const forbidden = toolNames.filter((name) =>
		FORBIDDEN_BUILTIN_TOOLS.has(name),
	);
	if (forbidden.length > 0) {
		throw new Error(
			`safety boundary violated: forbidden built-in tools present: ${forbidden.join(", ")}`,
		);
	}
}
