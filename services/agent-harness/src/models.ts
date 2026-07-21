import type { ModelRuntime } from "@earendil-works/pi-coding-agent";

/**
 * The configured AI model selection. Mirrors the Rust `AiConfig` /
 * `AiRequest.model` contract: a provider, model id, and prompt version.
 */
export interface ModelSpec {
  readonly provider: string;
  readonly modelId: string;
  readonly promptVersion: string;
}

/**
 * The resolved Pi model descriptor. `getModel` returns `undefined` when the
 * provider/model pair is not registered, so the narrowed type is non-nullable.
 */
export type ResolvedModel = NonNullable<ReturnType<ModelRuntime["getModel"]>>;

/**
 * Raised when the configured provider/model cannot be resolved from the
 * model runtime's catalog. Fail closed — never fall back to an arbitrary model.
 */
export class ModelResolutionError extends Error {
  public readonly provider: string;
  public readonly modelId: string;

  public constructor(provider: string, modelId: string) {
    super(`AI model not registered: ${provider}/${modelId}`);
    this.name = "ModelResolutionError";
    this.provider = provider;
    this.modelId = modelId;
  }
}

/**
 * Resolve a configured model from the runtime catalog.
 *
 * `getModel` does not check whether an API key exists — authentication is
 * injected separately at runtime via the secret provider — but it does
 * require the model to be present in the catalog (built-in or custom
 * `models.json`). An unknown model is rejected.
 */
export function resolveModel(
  runtime: ModelRuntime,
  spec: ModelSpec,
): ResolvedModel {
  const model = runtime.getModel(spec.provider, spec.modelId);
  if (!model) {
    throw new ModelResolutionError(spec.provider, spec.modelId);
  }
  return model;
}
