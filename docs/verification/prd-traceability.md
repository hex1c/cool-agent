<!-- markdownlint-disable MD013 -->

# PRD Success-Criteria Traceability

Maps each success criterion in `docs/prd/telegram-operations-worker.md` §16 to
the automated test(s) that prove it, or to the manual evidence required where
no automated test exists. Status legend:

- ✅ Automated — covered by one or more passing automated tests.
- ⚠️ Partial — automated coverage exists for part of the criterion; a gap
  remains.
- 🔶 Manual — requires human verification (live provider credentials, visual
  review, or staging deployment).
- ❌ Gap — no automated or documented manual evidence yet.

## Traceability matrix

| # | Criterion (short) | Automated test(s) | Status |
| --: | ------------------- | ------------------- | :------: |
| 1 | Approved participant completes private Google OAuth onboarding | `crates/oauth/tests/oauth_contracts.rs::connect_exchanges_code_and_stores_refresh_token`; `::reconnect_clears_then_reconnects` | ✅ |
| 2 | Tokens stored securely; never in user messages or logs | `crates/oauth/tests/oauth_contracts.rs::access_token_is_redacted_in_debug_and_display`; `::refresh_token_is_redacted_in_debug_and_display`; `::authorization_code_is_redacted_in_debug`; `::oauth_state_value_is_redacted_in_debug`; `crates/application/src/observability.rs` (redacted `CostGuardMetric`/`StageProgress` emit no provider payloads); `tests/integration/tests/cost_guard.rs::cost_observability_monitoring_covers_required_failure_and_threshold_signals` (OAuth error alarm) | ✅ |
| 3 | Tagging the bot in a forum topic starts/resumes that topic's workflow | `crates/telegram/tests/update_contracts.rs::normalize_mention_fixture`; `tests/integration/tests/quotation_e2e.rs::forum_mention_routes_to_topic_and_renders_canonical_pdf` | ✅ |
| 4 | Topic sessions don't cross-associate; no workflows in private chat or non-forum groups | `crates/telegram/tests/update_contracts.rs::unsupported_group_chat_is_rejected`; `::channel_chat_is_rejected`; `crates/telegram/tests/delivery_contracts.rs::all_commands_rejected_in_private_chat`; `::all_five_commands_parse_in_forum_topic`; `tests/integration/tests/security_e2e.rs::unsupported_chat_rejected_at_intake` | ✅ |
| 5 | Up to 10 supported image/PDF/CSV/Word/Excel attachments of 20 MB each collected and stored | `crates/application/tests/attachment_collection.rs::accepts_attachment_within_limits`; `::rejects_over_count`; `::rejects_over_size`; `::rejects_disallowed_mime_type`; `::duplicate_attachment_produces_one_object`; `crates/application/tests/office_security.rs::valid_docx_passes`; `::valid_xlsx_passes`; `crates/application/tests/image_pdf_security.rs::valid_single_page` | ✅ |
| 6 | Every processing stage produces a topic update; OAuth confined to private config | `crates/telegram/tests/delivery_contracts.rs::all_five_commands_parse_in_forum_topic`; `crates/oauth/tests/oauth_contracts.rs::callback_participant_mismatch_is_rejected`; `crates/application/src/observability.rs` (`StageProgress` carries redacted stage labels only) | ✅ |
| 7 | Missing info causes a resumable clarification wait expiring after 12 hours | `crates/application/tests/resumable_confirmation.rs::enter_wait_from_extraction_completed`; `::resume_via_provide_clarification`; `::timeout_after_deadline_elapsed`; `::duplicate_resume_rejected`; `tests/integration/tests/step_functions.rs::confirmation_uses_durable_task_tokens`; `::all_workflows_have_timeout` | ✅ |
| 8 | AI returns schema-valid extracted values and calculations | `services/agent-harness/test/extraction.test.ts`; `services/agent-harness/test/contracts.test.ts`; `crates/application/tests/resumable_confirmation.rs::total_not_equal_subtotal_plus_tax_rejected`; `::negative_amount_rejected`; `::negative_total_rejected`; `::tax_rate_out_of_range_rejected` | ✅ |
| 9 | Any approved participant can correct/confirm/cancel, including a preview started by another employee | `crates/domain/tests/authorization_policy.rs::authorization_allows_an_approved_non_owner_but_keeps_google_bound_to_the_owner`; `::authorization_rejects_non_members_and_mismatched_evidence`; `crates/domain/tests/confirmation_policy.rs::approved_non_owner_consumes_confirmation_and_keeps_the_owner_principal`; `crates/application/tests/resumable_confirmation.rs::cancel_via_authorized_participant` | ✅ |
| 10 | No original or AI-calculated amount is written before confirmation | `crates/application/tests/quotation_flow.rs::happy_path_s3_drive_telegram_all_succeed` (asserts Drive/Telegram invoked only after confirmation proof); `crates/application/tests/existing_file_flow.rs::stale_confirmation_rejected_before_any_provider_write`; `crates/domain/tests/confirmation_policy.rs::issuing_confirmation_enters_waiting_state_and_binds_the_resulting_revision` | ✅ |
| 11 | New and existing Google files handled under documented confirmation rules | `crates/google/tests/create_contracts.rs::from_consumed_with_correct_action_succeeds`; `::from_consumed_with_pending_record_rejected`; `::from_consumed_with_wrong_action_rejected`; `crates/google/tests/existing_file_contracts.rs::proof_from_consumed_with_correct_action_succeeds`; `::target_mismatch_rejected_without_client_call`; `crates/application/tests/existing_file_flow.rs::fault_injection_replay_does_not_invoke_provider_again` | ✅ |
| 12 | Drive defaults, employee overrides, My Drive, and Shared Drive supported | `crates/google/tests/create_contracts.rs::company_default_uses_config_folder`; `::employee_override_wins_over_company_default`; `::workflow_override_wins_over_employee_and_company`; `::my_drive_has_no_folder_or_shared_drive`; `::shared_drive_with_permission`; `::shared_drive_without_permission_rejected` | ✅ |
| 13 | Confirmed Calendar event uses owner timezone and obeys confirmed attendee-invitation choice | `crates/application/tests/calendar_flow.rs::extraction_defaults_to_owner_timezone_primary_calendar_and_reminder`; `crates/google/tests/calendar_contracts.rs::invitation_on_and_off_each_create_one_owner_event`; `::changed_invitation_choice_cannot_consume_confirmed_preview`; `tests/integration/tests/calendar_e2e.rs::preview_defaults_to_owner_timezone_and_primary_calendar`; `::changed_invitation_choice_changes_digest` | ✅ |
| 14 | Confirmed Hostinger email sent with approved recipient/content, PDF attachment, 7-day S3 link | `crates/email/tests/smtp_contracts.rs::success_accepted`; `::validate_is_usable_for_pure_checks`; `crates/application/tests/quotation_flow.rs::happy_path_s3_drive_telegram_all_succeed` (S3 artifact + link); `tests/integration/tests/email_e2e.rs::preview_binds_seven_day_link_and_pdf_attachment` | ✅ |
| 15 | Incoming email replies are not processed | `crates/telegram/tests/update_contracts.rs::channel_chat_is_rejected`; webhook normalization rejects non-forum/private update sources; email service has no inbound-reply path (design: SMTP client is send-only). Manual confirmation required that no Lambda listens on an inbound mailbox. | 🔶 |
| 16 | Every unambiguously failed external call gets ≤3 attempts, then durable failure + user notification | `crates/application/tests/external_operation.rs::retryable_failures_exhaust_at_three_and_replay_without_a_fourth_call`; `::ambiguous_and_terminal_outcomes_never_retry`; `::started_but_unfinished_attempt_requires_manual_review_without_invocation`; `crates/email/tests/smtp_contracts.rs::terminal_permanent_rejection`; `::pre_terminator_transient_retryable`; `::post_terminator_ambiguity`; `tests/integration/tests/step_functions.rs::retry_policies_enforce_three_attempt_maximum`; `::ambiguous_outcomes_enter_manual_review_not_retry` | ✅ |
| 17 | Duplicate webhooks and retries do not duplicate confirmed writes | `crates/telegram/tests/update_contracts.rs::deduplicate_by_update_id`; `tests/integration/tests/quotation_e2e.rs::duplicate_webhook_update_id_is_stable_for_dedup`; `crates/application/tests/quotation_flow.rs::re_running_deliver_does_not_invoke_drive_or_telegram_again`; `crates/application/tests/quotation_recovery.rs::resume_targets_correct_topic`; `crates/application/tests/existing_file_flow.rs::duplicate_consume_and_prepare_returns_conflict`; `crates/application/tests/calendar_flow.rs::reminder_event_cannot_duplicate_on_retry` | ✅ |
| 18 | Raw inputs and generated artifacts remain available indefinitely while access is authorized | `crates/storage/tests/dynamodb_integration.rs::dynamodb_history_and_object_metadata_are_immutable_and_page_in_order`; `crates/storage/tests/object_integration.rs::s3_objects_presigning_and_secure_parameters_round_trip`; `crates/application/tests/attachment_collection.rs::duplicate_attachment_produces_one_object` (immutable raw storage) | ✅ |
| 19 | System warns at configured budget threshold and suspends intake before projected cost reaches ₹300 | `crates/domain/tests/cost_policy.rs::intake_and_deployment_stop_at_the_inclusive_suspension_boundary`; `::new_workflow_reservations_cannot_consume_the_operational_reserve`; `::existing_confirmed_reservations_may_finish_below_the_hard_cap`; `crates/application/src/cost_guard.rs` (`CostGuardService::reserve` enforces thresholds); `tests/integration/tests/cost_guard.rs::cost_observability_monitoring_covers_required_failure_and_threshold_signals` (₹240/₹270/₹300 alarm thresholds); `tests/integration/tests/budget_guard_integration.rs::concurrent_budget_reservations_never_cross_hard_cap` | ✅ |
| 20 | Dev, staging, production pass required automated and e2e verification gates | `.github/workflows/rust.yml`, `.github/workflows/typescript.yml`, `.github/workflows/sam.yml` (CI gates); `tests/integration/tests/local_sandbox.rs::local_sandbox_exercises_seeded_aws_and_provider_paths`; `tests/integration/tests/cost_guard.rs::sam_build_stages_only_the_infrastructure_build_driver`; Task 43 deployment gate (`scripts/verify-release.sh`) pending. | ⚠️ |
| 21 | Confirmed quotation matching `docs/sample-quotation.pdf` layout rendered in Rust, stored in S3, copied to Drive, delivered in topic | `crates/pdf/tests/render_golden.rs`; `crates/application/tests/quotation_flow.rs::happy_path_s3_drive_telegram_all_succeed`; `::telegram_delivery_targets_correct_topic`; `crates/application/tests/quotation_recovery.rs::s3_succeeds_drive_retryable_then_recovers`; `tests/integration/tests/quotation_e2e.rs::forum_mention_routes_to_topic_and_renders_canonical_pdf`; Manual: visual overlay of rendered golden against `docs/sample-quotation.pdf` (Task 4 acceptance). | ⚠️ |
| 22 | Local sandbox tests exercise AWS-backed workflow paths before deployment to shared dev/staging | `tests/integration/tests/local_sandbox.rs::local_sandbox_exercises_seeded_aws_and_provider_paths` | ✅ |

## Gaps and manual evidence

### Criterion 15 — Incoming email replies are not processed (🔶 Manual)

The system is send-only by design: the email crate exposes an SMTP client with
no inbound mailbox listener, and no Lambda is configured to poll an inbox.
Automated evidence: webhook normalization rejects non-forum/private update
sources (`channel_chat_is_rejected`). **Remaining manual evidence:** confirm
in the deployment runbook (Task 43) that no inbound email Lambda or EventBridge
rule is provisioned, and that the Hostinger mailbox is not configured for
programmatic read access.

### Criterion 20 — Environment verification gates (⚠️ Partial)

CI workflows (`.github/workflows/*.yml`) run formatting, lint, unit tests,
builds, and schema validation on every pull request, and the local sandbox
suite (`tests/integration/tests/local_sandbox.rs`) exercises AWS-backed paths.
**Remaining gap:** the unified deployment gate command `scripts/verify-release.sh`
(Task 43) and the per-environment staging sign-off (Task 44) are not yet
implemented. Until Task 43 lands, environment promotion relies on the existing
CI jobs plus manual `sam validate`/`sam build` checks.

### Criterion 21 — Quotation layout match (⚠️ Partial)

The renderer produces a golden PDF (`crates/pdf/tests/render_golden.rs`) and the
delivery flow proves S3/Drive/topic publication
(`crates/application/tests/quotation_flow.rs`). **Remaining manual evidence:**
a human must overlay the rendered golden against `docs/sample-quotation.pdf`
(Task 4 acceptance criterion) to confirm visual fidelity before production.
Final signature and company-specific details are supplied during development
per §17.1 and gated on production deployment.

## Verification commands

```bash
# Rust workspace (criteria 1-19, 22)
cargo test --workspace --all-features
cargo test --test local_sandbox --features integration   # requires sandbox up

# TypeScript agent harness (criterion 8)
pnpm --filter @novus/agent-harness test --run

# Integration / e2e (criteria 4, 9, 10, 13, 14, 15, 17, 19, 21)
cargo test -p integration-tests --test cost_guard
cargo test -p integration-tests --test quotation_e2e --test calendar_e2e \
    --test email_e2e --test security_e2e
```
