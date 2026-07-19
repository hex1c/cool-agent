import { defineTool } from "@earendil-works/pi-coding-agent";
import { Type, type Static } from "typebox";

/**
 * A normalized document made available to the extraction tool. The caller
 * (the Lambda handler) builds this from the Rust-normalized attachments;
 * the tool itself performs no I/O and has no filesystem, network, or
 * external-service access.
 */
export interface NormalizedDocumentRef {
	readonly objectRef: string;
	readonly mediaType: string;
	readonly extractedText: string | null;
}

const TOOL_NAME = "extract_documents";

const ExtractParameters = Type.Object({
	object_refs: Type.Array(
		Type.String({ description: "Document object reference to extract." }),
		{ description: "Document object references to extract." },
	),
});

type ExtractParametersType = Static<typeof ExtractParameters>;

/**
 * Build the in-memory lookup map of object references to their pre-normalized
 * text. Documents whose `extractedText` is `null` (e.g. Office formats whose
 * extraction is deferred) are omitted from the map and reported as
 * unavailable when requested.
 */
function buildDocumentIndex(
	documents: readonly NormalizedDocumentRef[],
): Map<string, string> {
	const byRef = new Map<string, string>();
	for (const doc of documents) {
		if (doc.extractedText != null) {
			byRef.set(doc.objectRef, doc.extractedText);
		}
	}
	return byRef;
}

/**
 * Pure extraction logic, separated from the tool's `execute` so it can be
 * unit-tested without an `ExtensionContext`. Returns a single text block
 * joining each requested reference's normalized text, or an "unavailable"
 * notice for references that have no normalized text.
 */
export function extractDocumentText(
	documents: readonly NormalizedDocumentRef[],
	refs: readonly string[],
): string {
	const byRef = buildDocumentIndex(documents);
	const blocks = refs.map((ref) => {
		const text = byRef.get(ref);
		if (text == null) {
			return `${ref}: no normalized text available`;
		}
		return `${ref}:\n${text}`;
	});
	return blocks.join("\n\n");
}

/**
 * Build the narrowly scoped `extract_documents` tool.
 *
 * The tool only reads from an in-memory map of object references to their
 * pre-normalized text. It cannot read the filesystem, run shell commands,
 * call external services, or mutate anything.
 */
export function createExtractDocumentsTool(
	documents: readonly NormalizedDocumentRef[],
) {
	return defineTool({
		name: TOOL_NAME,
		label: "Extract Documents",
		description:
			"Return the pre-normalized text for the requested document object references. " +
			"No filesystem, network, or external-service access is available.",
		parameters: ExtractParameters,
		execute: async (_toolCallId, params: ExtractParametersType) => {
			const text = extractDocumentText(documents, params.object_refs);
			return {
				content: [{ type: "text" as const, text }],
				details: {},
			};
		},
	});
}

export const extractDocumentsToolName = TOOL_NAME;
