# Shared build macro for the TypeScript Pi agent-harness Lambda (Task 39).
#
# SAM's makefile builder copies the CodeUri (the repository root) into a
# sandbox, so the pnpm workspace root is present. `pnpm deploy --prod` is the
# pnpm-native way to produce a deployable, hoisted production node_modules
# (pnpm's default symlinked store is not Lambda-compatible). It writes the
# package's declared `files` (the compiled `dist/`) plus `package.json` and a
# production-only `node_modules` to the target directory.
#
#   $(call agent-harness-deploy)

PNPM ?= pnpm
AGENT_PKG ?= @novus/agent-harness

define agent-harness-deploy
	$(PNPM) install --frozen-lockfile
	$(PNPM) --filter $(AGENT_PKG) build
	rm -rf "$(ARTIFACTS_DIR)"
	$(PNPM) --filter $(AGENT_PKG) deploy --prod --legacy "$(ARTIFACTS_DIR)"
endef
