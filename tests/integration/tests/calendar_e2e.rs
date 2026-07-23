#![deny(unsafe_code)]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! End-to-end Calendar slice (Task 42).
//!
//! Orchestrates the cross-cutting confirmed Calendar path: an AI-extracted
//! `CalendarResult` is turned into a `CalendarPreview` bound to the workflow
//! owner's timezone defaults, the preview digest is stable across equivalent
//! attendee orderings, and changing the invitation choice produces a different
//! digest (so the prior confirmed preview cannot be consumed). No AWS
//! credentials or Docker required.

use application::calendar::{CalendarPreview, CalendarTarget, OwnerCalendarDefaults};
use domain::contracts::{CalendarReminderResult, CalendarResult};

fn owner_defaults() -> OwnerCalendarDefaults {
    OwnerCalendarDefaults::new("Asia/Kolkata".to_owned(), 30).expect("owner defaults build")
}

fn extracted_calendar(send_invitations: bool, attendees: Vec<String>) -> CalendarResult {
    CalendarResult {
        title: "Quarterly review".to_owned(),
        start: "2025-02-03T10:00:00+05:30".to_owned(),
        end: "2025-02-03T11:00:00+05:30".to_owned(),
        timezone: None, // forces default fallback
        calendar_id: None,
        description: Some("Review Q1 numbers".to_owned()),
        attendees,
        reminders: Some(CalendarReminderResult {
            push_minutes: Some(15),
            email_minutes: None,
        }),
        send_invitations,
    }
}

#[test]
fn preview_defaults_to_owner_timezone_and_primary_calendar() {
    let result = extracted_calendar(false, vec!["a@example.com".to_owned()]);
    let preview = CalendarPreview::from_extraction(&result, &owner_defaults())
        .expect("preview builds from extraction");
    assert_eq!(preview.timezone(), "Asia/Kolkata");
    assert!(matches!(preview.calendar(), CalendarTarget::Primary));
    // Explicit reminders override defaults.
    assert_eq!(preview.reminders().push_minutes, Some(15));
}

#[test]
fn digest_is_stable_for_equivalent_attendee_order() {
    let result_a = extracted_calendar(
        true,
        vec!["a@example.com".to_owned(), "b@example.com".to_owned()],
    );
    let result_b = extracted_calendar(
        true,
        vec!["b@example.com".to_owned(), "a@example.com".to_owned()],
    );
    let preview_a = CalendarPreview::from_extraction(&result_a, &owner_defaults()).expect("a");
    let preview_b = CalendarPreview::from_extraction(&result_b, &owner_defaults()).expect("b");
    assert_eq!(
        preview_a.digest().expect("digest a"),
        preview_b.digest().expect("digest b"),
        "attendee order must not change the bound digest"
    );
}

#[test]
fn changed_invitation_choice_changes_digest() {
    let off = extracted_calendar(false, vec!["a@example.com".to_owned()]);
    let on = extracted_calendar(true, vec!["a@example.com".to_owned()]);
    let preview_off = CalendarPreview::from_extraction(&off, &owner_defaults()).expect("off");
    let preview_on = CalendarPreview::from_extraction(&on, &owner_defaults()).expect("on");
    assert_ne!(
        preview_off.digest().expect("digest off"),
        preview_on.digest().expect("digest on"),
        "invitation choice is bound to the confirmation digest"
    );
}
