import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import piLspClient from "../git/github.com/code-yeongyu/pi-lsp-client/src/index.ts";

const UPSTREAM_DIAGNOSTICS_TOOL = "lsp_diagnostics";
const PROJECT_DIAGNOSTICS_TOOL = "pi_lsp_diagnostics";

/** Load pi-lsp-client without colliding with pi-lens's diagnostics tool. */
export default function registerPiLspClient(pi: ExtensionAPI): void {
  const adaptedPi = new Proxy(pi, {
    get(target, property, receiver) {
      if (property === "registerTool") {
        return (tool: Parameters<ExtensionAPI["registerTool"]>[0]): void => {
          if (tool.name === UPSTREAM_DIAGNOSTICS_TOOL) {
            target.registerTool({
              ...tool,
              name: PROJECT_DIAGNOSTICS_TOOL,
              label: "Pi LSP Diagnostics",
            });
            return;
          }

          target.registerTool(tool);
        };
      }

      const value: unknown = Reflect.get(target, property, receiver);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });

  piLspClient(adaptedPi);
}
