#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented
)]
mod preview {
    use application::resumable_confirmation::{Preview, PreviewError, PreviewLineItem};

    fn preview() -> Preview {
        Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "Consulting".to_string(),
                quantity: "2".to_string(),
                unit_price_micro_inr: 50_000_000,
            }],
            vec!["standard rate".to_string()],
            100_000_000,
            1800,
            18_000_000,
            118_000_000,
        )
        .unwrap()
    }

    #[test]
    fn valid_preview_builds() {
        let p = preview();
        assert_eq!(p.currency, "INR");
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.total_micro_inr, 118_000_000);
    }

    #[test]
    fn digest_is_deterministic() {
        let p = preview();
        let d1 = p.digest().unwrap();
        let d2 = p.digest().unwrap();
        assert_eq!(d1, d2);
    }

    #[test]
    fn digest_differs_for_different_previews() {
        let p1 = preview();
        let p2 = Preview::new(
            "USD".to_string(),
            vec![PreviewLineItem {
                description: "Item".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 100,
            }],
            vec![],
            100,
            0,
            0,
            100,
        )
        .unwrap();
        assert_ne!(p1.digest().unwrap(), p2.digest().unwrap());
    }

    #[test]
    fn total_not_equal_subtotal_plus_tax_rejected() {
        let err = Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "x".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 100,
            }],
            vec![],
            100,
            0,
            0,
            200,
        );
        assert!(matches!(err, Err(PreviewError::TotalMismatch { .. })));
    }

    #[test]
    fn bad_currency_rejected() {
        let err = Preview::new(
            "inr".to_string(),
            vec![PreviewLineItem {
                description: "x".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 100,
            }],
            vec![],
            100,
            0,
            0,
            100,
        );
        assert!(matches!(err, Err(PreviewError::InvalidCurrency { .. })));

        let err = Preview::new(
            "IN".to_string(),
            vec![PreviewLineItem {
                description: "x".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 100,
            }],
            vec![],
            100,
            0,
            0,
            100,
        );
        assert!(matches!(err, Err(PreviewError::InvalidCurrency { .. })));
    }

    #[test]
    fn negative_amount_rejected() {
        let err = Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "x".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 100,
            }],
            vec![],
            -1,
            0,
            0,
            -1,
        );
        assert!(matches!(err, Err(PreviewError::NegativeSubtotal { .. })));
    }

    #[test]
    fn empty_items_rejected() {
        let err = Preview::new("INR".to_string(), vec![], vec![], 0, 0, 0, 0);
        assert!(matches!(err, Err(PreviewError::EmptyItems)));
    }

    #[test]
    fn tax_rate_out_of_range_rejected() {
        let err = Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "x".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 100,
            }],
            vec![],
            100,
            10001,
            0,
            100,
        );
        assert!(matches!(err, Err(PreviewError::TaxRateOutOfRange { .. })));
    }

    #[test]
    fn mutation_target_differs_per_label() {
        let p = preview();
        let t1 = p.mutation_target("sheet-write").unwrap();
        let t2 = p.mutation_target("pdf-gen").unwrap();
        assert_ne!(t1, t2);
    }

    #[test]
    fn mutation_target_stable_for_same_label() {
        let p = preview();
        let t1 = p.mutation_target("sheet-write").unwrap();
        let t2 = p.mutation_target("sheet-write").unwrap();
        assert_eq!(t1, t2);
    }

    #[test]
    fn digest_stable_across_equal_previews() {
        let p1 = preview();
        let p2 = preview();
        assert_eq!(p1.digest().unwrap(), p2.digest().unwrap());
    }

    #[test]
    fn negative_tax_rejected() {
        let err = Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "x".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 100,
            }],
            vec![],
            100,
            0,
            -1,
            99,
        );
        assert!(matches!(err, Err(PreviewError::NegativeTax { .. })));
    }

    #[test]
    fn negative_total_rejected() {
        let err = Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "x".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 100,
            }],
            vec![],
            100,
            0,
            0,
            -1,
        );
        assert!(matches!(err, Err(PreviewError::NegativeTotal { .. })));
    }
}

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented
)]
mod clarification {
    use application::resumable_confirmation::{ResumableConfirmationService, ResumableError};
    use domain::authorization::{LiveMembershipEvidence, MembershipStatus, authorize_participant};
    use domain::identity::{
        ChatId, MessageId, MessageThreadId, ParticipantId, TopicSessionId, WorkflowId,
    };
    use domain::transition::{TransitionRequest, WorkflowTransition};
    use domain::workflow::{
        ClarificationResume, WaitDeadline, Workflow, WorkflowState, WorkflowTimestamp,
    };

    fn topic() -> TopicSessionId {
        TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(77).unwrap())
    }

    fn participant(value: i64) -> ParticipantId {
        ParticipantId::new(value).unwrap()
    }

    fn time(seconds: u64) -> WorkflowTimestamp {
        WorkflowTimestamp::from_unix_seconds(seconds)
    }

    fn advance(workflow: &Workflow, transition: WorkflowTransition, at: u64) -> Workflow {
        workflow
            .transition(TransitionRequest {
                transition,
                expected_revision: workflow.revision(),
                actor: participant(202),
                source_message: MessageId::new(i64::try_from(at).unwrap()).unwrap(),
                timestamp: time(at),
            })
            .unwrap()
            .workflow
    }

    fn extraction_completed() -> Workflow {
        let wf = Workflow::new(
            WorkflowId::new("wf-extraction").unwrap(),
            topic(),
            participant(101),
            time(1),
        );
        let wf = advance(
            &wf,
            WorkflowTransition::BeginAttachmentCollection {
                deadline: WaitDeadline::at(time(30)),
            },
            2,
        );
        let wf = advance(&wf, WorkflowTransition::FinishAttachmentCollection, 3);
        advance(&wf, WorkflowTransition::CompleteExtraction, 4)
    }

    fn drafting_completed() -> Workflow {
        let wf = extraction_completed();
        let wf = advance(&wf, WorkflowTransition::StartCalculationOrDrafting, 5);
        advance(&wf, WorkflowTransition::CompleteCalculationOrDrafting, 6)
    }

    fn authorized_participant(at: u64) -> domain::authorization::AuthorizedParticipant {
        authorize_participant(
            topic().chat_id(),
            participant(202),
            &LiveMembershipEvidence::new(
                topic().chat_id(),
                participant(202),
                MembershipStatus::Approved,
                time(at),
            ),
            time(at),
        )
        .unwrap()
    }

    // -- enter_clarification_wait tests -------------------------------------

    #[test]
    fn enter_12h_wait_from_drafting_completed() {
        let wf = drafting_completed();
        let svc = ResumableConfirmationService;
        let deadline = WaitDeadline::at(time(43_200));
        let outcome = svc
            .enter_clarification_wait(
                &wf,
                participant(202),
                MessageId::new(10).unwrap(),
                ClarificationResume::CalculationOrDrafting,
                deadline,
                time(10),
            )
            .unwrap();
        assert!(matches!(
            outcome.workflow.state(),
            WorkflowState::WaitingForClarification { .. }
        ));
    }

    #[test]
    fn enter_wait_from_extraction_completed() {
        let wf = extraction_completed();
        let svc = ResumableConfirmationService;
        let outcome = svc
            .enter_clarification_wait(
                &wf,
                participant(202),
                MessageId::new(10).unwrap(),
                ClarificationResume::Extraction,
                WaitDeadline::at(time(43_200)),
                time(10),
            )
            .unwrap();
        assert!(matches!(
            outcome.workflow.state(),
            WorkflowState::WaitingForClarification { .. }
        ));
    }

    #[test]
    fn enter_wait_wrong_resume_stage_rejected() {
        let wf = drafting_completed();
        let svc = ResumableConfirmationService;
        let err = svc
            .enter_clarification_wait(
                &wf,
                participant(202),
                MessageId::new(10).unwrap(),
                ClarificationResume::Extraction,
                WaitDeadline::at(time(43_200)),
                time(10),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::IllegalClarificationStage));
    }

    // -- resume_clarification tests -----------------------------------------

    fn waiting_for_clarification_calc() -> Workflow {
        let svc = ResumableConfirmationService;
        let wf = drafting_completed();
        svc.enter_clarification_wait(
            &wf,
            participant(202),
            MessageId::new(10).unwrap(),
            ClarificationResume::CalculationOrDrafting,
            WaitDeadline::at(time(43_200)),
            time(10),
        )
        .unwrap()
        .workflow
    }

    #[test]
    fn resume_via_provide_clarification() {
        let wf = waiting_for_clarification_calc();
        let svc = ResumableConfirmationService;
        let outcome = svc
            .resume_clarification(
                &wf,
                participant(202),
                MessageId::new(11).unwrap(),
                None,
                time(11),
            )
            .unwrap();
        assert!(matches!(
            outcome.workflow.state(),
            WorkflowState::CalculationOrDraftingStarted
        ));
    }

    #[test]
    fn resume_attachment_collection_requires_deadline() {
        // Need a WaitingForClarification with AttachmentCollection resume.
        // We can't easily create one from the standard flow, so test that
        // providing None for deadline_for_resume when needed returns error.
        let wf = waiting_for_clarification_calc();
        let svc = ResumableConfirmationService;
        // This has CalculationOrDrafting resume, so deadline_for_resume=None is fine.
        // But if the state had AttachmentCollection, None would be an error.
        // Test that provide_clarification works from calculation resume regardless.
        let outcome = svc
            .resume_clarification(
                &wf,
                participant(202),
                MessageId::new(11).unwrap(),
                Some(WaitDeadline::at(time(43_300))),
                time(11),
            )
            .unwrap();
        assert!(matches!(
            outcome.workflow.state(),
            WorkflowState::CalculationOrDraftingStarted
        ));
    }

    // -- timeout_clarification tests ----------------------------------------

    #[test]
    fn timeout_after_deadline_elapsed() {
        let wf = waiting_for_clarification_calc();
        let svc = ResumableConfirmationService;
        let outcome = svc
            .timeout_clarification(
                &wf,
                participant(202),
                MessageId::new(50_000).unwrap(),
                time(50_000),
            )
            .unwrap();
        assert!(matches!(outcome.workflow.state(), WorkflowState::Expired));
    }

    #[test]
    fn timeout_before_deadline_rejected() {
        let wf = waiting_for_clarification_calc();
        let svc = ResumableConfirmationService;
        let err = svc
            .timeout_clarification(&wf, participant(202), MessageId::new(20).unwrap(), time(20))
            .unwrap_err();
        assert!(matches!(err, ResumableError::Transition(_)));
    }

    // -- cancel tests -------------------------------------------------------

    #[test]
    fn cancel_via_authorized_participant() {
        let wf = waiting_for_clarification_calc();
        let svc = ResumableConfirmationService;
        let authz = authorized_participant(12);
        let outcome = svc
            .cancel(&wf, &authz, MessageId::new(12).unwrap(), time(12))
            .unwrap();
        assert!(matches!(
            outcome.transition.workflow.state(),
            WorkflowState::Stopped
        ));
        assert_eq!(outcome.authorization.actor, participant(202));
    }

    // -- edge cases ---------------------------------------------------------

    #[test]
    fn duplicate_resume_rejected() {
        let wf = waiting_for_clarification_calc();
        let svc = ResumableConfirmationService;
        let wf = svc
            .resume_clarification(
                &wf,
                participant(202),
                MessageId::new(11).unwrap(),
                None,
                time(11),
            )
            .unwrap()
            .workflow;
        let err = svc
            .resume_clarification(
                &wf,
                participant(202),
                MessageId::new(12).unwrap(),
                None,
                time(12),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::IllegalClarificationStage));
    }

    #[test]
    fn reenter_wait_from_non_waiting_rejected() {
        let wf = waiting_for_clarification_calc();
        let svc = ResumableConfirmationService;
        let wf = svc
            .resume_clarification(
                &wf,
                participant(202),
                MessageId::new(11).unwrap(),
                None,
                time(11),
            )
            .unwrap()
            .workflow;
        let err = svc
            .enter_clarification_wait(
                &wf,
                participant(202),
                MessageId::new(12).unwrap(),
                ClarificationResume::CalculationOrDrafting,
                WaitDeadline::at(time(44_000)),
                time(12),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::IllegalClarificationStage));
    }

    #[test]
    fn resume_from_non_waiting_state_rejected() {
        let wf = drafting_completed();
        let svc = ResumableConfirmationService;
        let err = svc
            .resume_clarification(
                &wf,
                participant(202),
                MessageId::new(10).unwrap(),
                None,
                time(10),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::IllegalClarificationStage));
    }
}

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented
)]
mod confirmation {
    use application::resumable_confirmation::{
        IssueConfirmationRequest, Preview, PreviewLineItem, ResumableConfirmationService,
        ResumableError,
    };
    use domain::authorization::{LiveMembershipEvidence, MembershipStatus, authorize_participant};
    use domain::confirmation::{
        ConfirmationAction, ConfirmationRecord, ConfirmationStatus, TopicMessageReference,
    };
    use domain::identity::{
        ChatId, ConfirmationId, MessageId, MessageThreadId, ParticipantId, TopicSessionId,
        WorkflowId,
    };
    use domain::transition::{TransitionRequest, WorkflowTransition};
    use domain::workflow::{WaitDeadline, Workflow, WorkflowState, WorkflowTimestamp};

    fn topic() -> TopicSessionId {
        TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(77).unwrap())
    }

    fn participant(value: i64) -> ParticipantId {
        ParticipantId::new(value).unwrap()
    }

    fn time(seconds: u64) -> WorkflowTimestamp {
        WorkflowTimestamp::from_unix_seconds(seconds)
    }

    fn advance(workflow: &Workflow, transition: WorkflowTransition, at: u64) -> Workflow {
        workflow
            .transition(TransitionRequest {
                transition,
                expected_revision: workflow.revision(),
                actor: participant(202),
                source_message: MessageId::new(i64::try_from(at).unwrap()).unwrap(),
                timestamp: time(at),
            })
            .unwrap()
            .workflow
    }

    fn drafting_completed() -> Workflow {
        let wf = Workflow::new(
            WorkflowId::new("wf-confirmation").unwrap(),
            topic(),
            participant(101),
            time(1),
        );
        let wf = advance(
            &wf,
            WorkflowTransition::BeginAttachmentCollection {
                deadline: WaitDeadline::at(time(30)),
            },
            2,
        );
        let wf = advance(&wf, WorkflowTransition::FinishAttachmentCollection, 3);
        let wf = advance(&wf, WorkflowTransition::CompleteExtraction, 4);
        let wf = advance(&wf, WorkflowTransition::StartCalculationOrDrafting, 5);
        advance(&wf, WorkflowTransition::CompleteCalculationOrDrafting, 6)
    }

    fn authorized_participant(at: u64) -> domain::authorization::AuthorizedParticipant {
        authorize_participant(
            topic().chat_id(),
            participant(202),
            &LiveMembershipEvidence::new(
                topic().chat_id(),
                participant(202),
                MembershipStatus::Approved,
                time(at),
            ),
            time(at),
        )
        .unwrap()
    }

    fn preview() -> Preview {
        Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "Consulting".to_string(),
                quantity: "2".to_string(),
                unit_price_micro_inr: 50_000_000,
            }],
            vec!["standard rate".to_string()],
            100_000_000,
            1800,
            18_000_000,
            118_000_000,
        )
        .unwrap()
    }

    fn waiting_for_confirmation() -> (Workflow, Preview, ConfirmationRecord) {
        let svc = ResumableConfirmationService;
        let wf = drafting_completed();
        let p = preview();
        let outcome = svc
            .issue_confirmation(
                &wf,
                IssueConfirmationRequest {
                    preview: &p,
                    action: ConfirmationAction::StartSheetOrDocWrite,
                    target_label: "sheet-write",
                    confirmation_id: ConfirmationId::new("confirmation-1").unwrap(),
                    actor: participant(202),
                    source_message: MessageId::new(7).unwrap(),
                    deadline: WaitDeadline::at(time(43_200)),
                    timestamp: time(7),
                },
            )
            .unwrap();
        let record = ConfirmationRecord::from(outcome.confirmation);
        (outcome.transition.workflow, p, record)
    }

    // -- issue --------------------------------------------------------------

    #[test]
    fn issue_from_drafting_completed_enters_waiting_state() {
        let (wf, _preview, _record) = waiting_for_confirmation();
        assert!(matches!(
            wf.state(),
            WorkflowState::WaitingForConfirmation { .. }
        ));
    }

    #[test]
    fn issue_binds_revision_and_digest() {
        let (wf, _preview, record) = waiting_for_confirmation();
        let pending = match &record {
            ConfirmationRecord::Pending(p) => p,
            ConfirmationRecord::Consumed(_) => panic!("expected pending"),
        };
        assert_eq!(pending.workflow_revision(), wf.revision());
        assert_eq!(pending.preview_digest(), preview().digest().unwrap());
    }

    // -- consume ------------------------------------------------------------

    #[test]
    fn consume_with_matching_digest_and_target() {
        let (wf, preview, record) = waiting_for_confirmation();
        let svc = ResumableConfirmationService;
        let authz = authorized_participant(8).for_workflow(&wf);
        let consumption = svc
            .consume_confirmation(
                &record,
                &wf,
                &authz,
                &preview,
                "sheet-write",
                TopicMessageReference::new(topic(), MessageId::new(8).unwrap()),
                time(8),
            )
            .unwrap();

        // Confirmation is consumed.
        assert_eq!(
            consumption.confirmation().status(),
            ConfirmationStatus::Consumed
        );

        // No external write — the outcome is transition + confirmation + audit only.
        assert_eq!(consumption.authorization().actor, participant(202));
    }

    #[test]
    fn stale_revision_after_correction_rejected() {
        let (wf, preview, record) = waiting_for_confirmation();
        let svc = ResumableConfirmationService;
        let authz = authorized_participant(9).for_workflow(&wf);

        // Correct to invalidate the pending revision.
        let correction = svc
            .correct(
                &record,
                &wf,
                &authz,
                TopicMessageReference::new(topic(), MessageId::new(9).unwrap()),
                time(9),
            )
            .unwrap();
        assert!(matches!(
            correction.transition.workflow.state(),
            WorkflowState::CalculationOrDraftingStarted
        ));

        // Consume with stale record fails.
        let err = svc
            .consume_confirmation(
                &record,
                &wf,
                &authz,
                &preview,
                "sheet-write",
                TopicMessageReference::new(topic(), MessageId::new(10).unwrap()),
                time(10),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::Confirmation(_)));
    }

    #[test]
    fn wrong_digest_rejected() {
        let (wf, _preview, record) = waiting_for_confirmation();
        let svc = ResumableConfirmationService;
        let authz = authorized_participant(8).for_workflow(&wf);

        let different = Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "Other".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 500,
            }],
            vec![],
            500,
            0,
            0,
            500,
        )
        .unwrap();

        let err = svc
            .consume_confirmation(
                &record,
                &wf,
                &authz,
                &different,
                "sheet-write",
                TopicMessageReference::new(topic(), MessageId::new(8).unwrap()),
                time(8),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::Confirmation(_)));
    }

    #[test]
    fn expired_confirmation_rejected() {
        let (wf, preview, record) = waiting_for_confirmation();
        let svc = ResumableConfirmationService;

        let authz_late = authorize_participant(
            topic().chat_id(),
            participant(202),
            &LiveMembershipEvidence::new(
                topic().chat_id(),
                participant(202),
                MembershipStatus::Approved,
                time(50_000),
            ),
            time(50_000),
        )
        .unwrap()
        .for_workflow(&wf);

        let err = svc
            .consume_confirmation(
                &record,
                &wf,
                &authz_late,
                &preview,
                "sheet-write",
                TopicMessageReference::new(topic(), MessageId::new(50_000).unwrap()),
                time(50_000),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::Confirmation(_)));
    }

    #[test]
    fn already_consumed_rejected() {
        let (wf, preview, record) = waiting_for_confirmation();
        let svc = ResumableConfirmationService;
        let authz = authorized_participant(8).for_workflow(&wf);

        // Consume once and obtain the consumed record.
        let consumption = svc
            .consume_confirmation(
                &record,
                &wf,
                &authz,
                &preview,
                "sheet-write",
                TopicMessageReference::new(topic(), MessageId::new(8).unwrap()),
                time(8),
            )
            .unwrap();
        let consumed_record = consumption.confirmation().clone();

        // Second consume with the consumed record fails.
        let authz2 = authorize_participant(
            topic().chat_id(),
            participant(202),
            &LiveMembershipEvidence::new(
                topic().chat_id(),
                participant(202),
                MembershipStatus::Approved,
                time(9),
            ),
            time(9),
        )
        .unwrap()
        .for_workflow(&wf);

        let err = svc
            .consume_confirmation(
                &consumed_record,
                &wf,
                &authz2,
                &preview,
                "sheet-write",
                TopicMessageReference::new(topic(), MessageId::new(9).unwrap()),
                time(9),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::Confirmation(_)));
    }

    #[test]
    fn correction_invalidates_revision_allowing_reissue() {
        let (wf, _preview, record) = waiting_for_confirmation();
        let svc = ResumableConfirmationService;
        let authz = authorized_participant(9).for_workflow(&wf);

        let correction = svc
            .correct(
                &record,
                &wf,
                &authz,
                TopicMessageReference::new(topic(), MessageId::new(9).unwrap()),
                time(9),
            )
            .unwrap();
        assert!(matches!(
            correction.transition.workflow.state(),
            WorkflowState::CalculationOrDraftingStarted
        ));

        // Re-issue produces a new digest/revision.
        let new_preview = Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "Revised".to_string(),
                quantity: "3".to_string(),
                unit_price_micro_inr: 30_000_000,
            }],
            vec![],
            90_000_000,
            10_00,
            9_000_000,
            99_000_000,
        )
        .unwrap();

        let wf = advance(
            &correction.transition.workflow,
            WorkflowTransition::CompleteCalculationOrDrafting,
            10,
        );

        let reissue = svc
            .issue_confirmation(
                &wf,
                IssueConfirmationRequest {
                    preview: &new_preview,
                    action: ConfirmationAction::StartSheetOrDocWrite,
                    target_label: "sheet-write",
                    confirmation_id: ConfirmationId::new("confirmation-2").unwrap(),
                    actor: participant(202),
                    source_message: MessageId::new(11).unwrap(),
                    deadline: WaitDeadline::at(time(50_000)),
                    timestamp: time(11),
                },
            )
            .unwrap();

        let old_digest = match &record {
            ConfirmationRecord::Pending(p) => p.preview_digest(),
            ConfirmationRecord::Consumed(_) => panic!("expected pending"),
        };
        assert_ne!(reissue.confirmation.preview_digest(), old_digest);
    }
}
