# Novus Telegram Operations Worker — SAM build entry point (Task 39).
#
# SAM's makefile BuildMethod runs `make build-<LogicalId>` from the CodeUri
# (the repository root) with ARTIFACTS_DIR set to a per-function staging
# directory. Each target builds exactly one Lambda handler and stages it as
# `bootstrap` (Rust provided.al2023) or a deployable Node artifact (agent
# harness). Shared macros live in infrastructure/build-rust.mk and
# infrastructure/build-agent.mk.
#
# Handler inventory and SAM requirements are documented in each package's
# lib.rs (workflow-actions, external-actions) and below.

include infrastructure/build-rust.mk
include infrastructure/build-agent.mk

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
	build-AgentHarnessFunction

# --- Rust handlers (provided.al2023, x86_64) -------------------------------

# Telegram webhook intake (256 MB, 10 s).
build-WebhookFunction:
	$(call cargo-lambda-bin,webhook-function,functions/webhook)

# Private Google OAuth onboarding callback (256 MB, 15 s).
build-OAuthFunction:
	$(call cargo-lambda-bin,oauth-function,functions/oauth)

# Step Functions attachment collection (512 MB, 60 s).
build-AttachmentCollectionFunction:
	$(call cargo-lambda-bin,attachment_collection,functions/workflow-actions)

# Consume StartDirectPdfGeneration confirmation (256 MB, 10 s).
build-WorkflowActionFunction:
	$(call cargo-lambda-bin,workflow,functions/workflow-actions)

# Render Version 1 quotation PDF, pure compute (1024 MB, 60 s).
build-PdfRenderFunction:
	$(call cargo-lambda-bin,pdf,functions/workflow-actions)

# S3/Drive/topic artifact delivery (1024 MB, 120 s).
build-DeliveryFunction:
	$(call cargo-lambda-bin,delivery,functions/workflow-actions)

# Confirmed Drive/Sheets/Docs create + existing-file mutation (512 MB, 60 s).
build-GoogleActionsFunction:
	$(call cargo-lambda-bin,google,functions/external-actions)

# Confirmed Calendar event creation (512 MB, 60 s).
build-CalendarActionsFunction:
	$(call cargo-lambda-bin,calendar,functions/external-actions)

# Confirmed Hostinger SMTP send (512 MB, 60 s).
build-EmailActionsFunction:
	$(call cargo-lambda-bin,email,functions/external-actions)

# --- TypeScript handler (nodejs22.x) ----------------------------------------

# Pi SDK extraction + history rehydration (2048 MB, 180 s).
build-AgentHarnessFunction:
	$(call agent-harness-deploy)
