#![deny(unsafe_code)]

//! Shared event/result DTOs and pure processing for the external-actions
//! Lambda package (Task 37A). Three thin binaries: google, calendar, email.
//!
//! Each binary deserializes a versioned event, calls one pure processing
//! function with an injected adapter runner, and returns a typed result.
//! Adapter wiring (Google / SMTP clients) is injected so the processing
//! functions are unit-testable without credentials.

pub mod calendar;
pub mod email;
pub mod google;

pub use calendar::{CalendarActionEvent, CalendarActionRunner, process_calendar_action};
pub use email::{EmailActionEvent, EmailActionRunner, process_email_action};
pub use google::{
    ExternalActionResultDto, GoogleCreateEvent, GoogleCreateRunner, GoogleMutationEvent,
    GoogleMutationRunner, process_google_create, process_google_mutation,
};

/// Schema version stamp carried by every event. Handlers reject mismatched
/// versions before any work.
pub const EVENT_SCHEMA_VERSION: &str = "novus.external-actions.v1";
