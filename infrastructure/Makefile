SHELL := /bin/bash

# SAM copies only infrastructure/ before invoking this file. Resolve the
# checkout from its original artifacts directory so build outputs in target/
# are never recursively copied into every function's temporary source tree.
REPO_ROOT := $(abspath $(ARTIFACTS_DIR)/../../..)

include build-rust.mk

.PHONY: \
	build-WebhookFunction \
	build-OAuthFunction \
	build-AttachmentCollectionFunction \
	build-WorkflowActionFunction \
	build-PdfRenderFunction \
	build-DeliveryFunction \
	build-GoogleActionsFunction \
	build-CalendarActionsFunction \
	build-EmailActionsFunction \
	build-CostGuardIntakeFunction \
	build-CostGuardAiFunction \
	build-CostGuardExternalFunction \
	build-CostGuardFunction \
	build-AgentHarnessFunction

build-WebhookFunction:
	$(call cargo-lambda-bin,webhook-function,$(REPO_ROOT)/functions/webhook)

build-OAuthFunction:
	$(call cargo-lambda-bin,oauth-function,$(REPO_ROOT)/functions/oauth)

build-AttachmentCollectionFunction:
	$(call cargo-lambda-bin,attachment_collection,$(REPO_ROOT)/functions/workflow-actions)

build-WorkflowActionFunction:
	$(call cargo-lambda-bin,workflow,$(REPO_ROOT)/functions/workflow-actions)

build-PdfRenderFunction:
	$(call cargo-lambda-bin,pdf,$(REPO_ROOT)/functions/workflow-actions)

build-DeliveryFunction:
	$(call cargo-lambda-bin,delivery,$(REPO_ROOT)/functions/workflow-actions)

build-GoogleActionsFunction:
	$(call cargo-lambda-bin,google,$(REPO_ROOT)/functions/external-actions)

build-CalendarActionsFunction:
	$(call cargo-lambda-bin,calendar,$(REPO_ROOT)/functions/external-actions)

build-EmailActionsFunction:
	$(call cargo-lambda-bin,email,$(REPO_ROOT)/functions/external-actions)

build-CostGuardIntakeFunction build-CostGuardAiFunction build-CostGuardExternalFunction build-CostGuardFunction:
	$(call cargo-lambda-bin,cost_guard,$(REPO_ROOT)/functions/workflow-actions)

build-AgentHarnessFunction:
	cd "$(REPO_ROOT)" && corepack pnpm install --frozen-lockfile
	cd "$(REPO_ROOT)" && corepack pnpm --filter @novus/agent-harness build
	rm -rf "$(ARTIFACTS_DIR)"
	cd "$(REPO_ROOT)" && corepack pnpm --filter @novus/agent-harness deploy --prod --legacy "$(ARTIFACTS_DIR)"
