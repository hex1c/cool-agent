import { readFileSync } from "node:fs";

import { defineTool, type Skill } from "@earendil-works/pi-coding-agent";
import { Type, type Static } from "typebox";

const TOOL_NAME = "load_skill";

const LoadSkillParameters = Type.Object({
  name: Type.String({ description: "Name of the harness skill to load." }),
});

type LoadSkillParametersType = Static<typeof LoadSkillParameters>;

/** Load trusted skill files into memory before the model session starts. */
export function loadSkillInstructions(
  skills: readonly Skill[],
): ReadonlyMap<string, string> {
  return new Map(
    skills.map((skill) => [skill.name, readFileSync(skill.filePath, "utf8")]),
  );
}

/** Build a read-only tool that exposes only preloaded harness skills. */
export function createLoadSkillTool(skills: readonly Skill[]) {
  const instructions = loadSkillInstructions(skills);

  return defineTool({
    name: TOOL_NAME,
    label: "Load Skill",
    description:
      "Load the instructions for a named harness skill. Only bundled harness skills are available; this tool cannot access the filesystem.",
    parameters: LoadSkillParameters,
    execute: async (_toolCallId, params: LoadSkillParametersType) => {
      const content = instructions.get(params.name);
      const text =
        content ??
        `Unknown skill: ${params.name}. Available skills: ${[...instructions.keys()].join(", ") || "none"}`;
      return {
        content: [{ type: "text" as const, text }],
        details: {},
      };
    },
  });
}

export const loadSkillToolName = TOOL_NAME;
