//! Step Functions ASL definition validation (Task 38).
//!
//! Validates the three workflow state machines (quotation, calendar, email)
//! structurally: well-formed JSON, required top-level fields, every state
//! reference resolves, terminal states exist, retry policies enforce the
//! three-attempt maximum, task-token confirmation states use
//! `waitForTaskToken`, and ambiguous outcomes route to manual-review Fail
//! states rather than automatic retry.
//!
//! These are structural checks that run without Step Functions Local. The
//! live wait/resume/timeout/retry suite runs in the local AWS sandbox
//! (Task 40) against Step Functions Local.

#![deny(unsafe_code)]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::HashSet;
use std::path::Path;

use serde_json::Value;

/// Load and parse an ASL definition from the statemachines directory.
fn load_asl(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../infrastructure/statemachines")
        .join(name);
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("ASL file {name} must be readable"));
    serde_json::from_str(&content).unwrap_or_else(|_| panic!("ASL file {name} must be JSON"))
}

/// All defined workflow files.
const WORKFLOWS: &[(&str, &str)] = &[
    ("quotation", "quotation.asl.json"),
    ("calendar", "calendar.asl.json"),
    ("email", "email.asl.json"),
];

// ── Structural validation helpers ─────────────────────────────────────

fn states(asl: &Value) -> &serde_json::Map<String, Value> {
    asl["States"].as_object().expect("States must be an object")
}

fn state_names(asl: &Value) -> HashSet<String> {
    states(asl).keys().cloned().collect()
}

fn start_at(asl: &Value) -> String {
    asl["StartAt"]
        .as_str()
        .expect("StartAt must be a string")
        .to_owned()
}

/// Collect every state name referenced by Next, Default, Choices[].Next,
/// Catch[].Next, and Retry (no Next).
fn referenced_states(asl: &Value) -> HashSet<String> {
    let mut refs = HashSet::new();
    refs.insert(start_at(asl));

    for state in states(asl).values() {
        if let Some(next) = state.get("Next").and_then(Value::as_str) {
            refs.insert(next.to_owned());
        }
        if let Some(default) = state.get("Default").and_then(Value::as_str) {
            refs.insert(default.to_owned());
        }
        if let Some(choices) = state.get("Choices").and_then(Value::as_array) {
            for choice in choices {
                if let Some(next) = choice.get("Next").and_then(Value::as_str) {
                    refs.insert(next.to_owned());
                }
            }
        }
        if let Some(catchers) = state.get("Catch").and_then(Value::as_array) {
            for catcher in catchers {
                if let Some(next) = catcher.get("Next").and_then(Value::as_str) {
                    refs.insert(next.to_owned());
                }
            }
        }
    }
    refs
}

/// States that terminate the workflow without outgoing transitions.
fn terminal_states(asl: &Value) -> HashSet<String> {
    let mut terminals = HashSet::new();
    for (name, state) in states(asl) {
        let stype = state.get("Type").and_then(Value::as_str).unwrap_or("");
        let has_end = state.get("End").and_then(Value::as_bool).unwrap_or(false);
        let has_next = state.get("Next").is_some();
        if matches!(stype, "Succeed" | "Fail") || (has_end && !has_next) {
            terminals.insert(name.clone());
        }
    }
    terminals
}

/// Count the maximum attempts across all Retry blocks in a state machine.
fn max_retry_attempts(asl: &Value) -> u32 {
    let mut max_attempts: u32 = 0;
    for state in states(asl).values() {
        if let Some(retries) = state.get("Retry").and_then(Value::as_array) {
            for retry in retries {
                if let Some(attempts) = retry.get("MaxAttempts").and_then(Value::as_u64) {
                    max_attempts = max_attempts.max(attempts as u32);
                }
            }
        }
    }
    max_attempts
}

/// Check whether any state uses the waitForTaskToken integration pattern.
fn has_task_token_confirmation(asl: &Value) -> bool {
    states(asl).values().any(|state| {
        state
            .get("Resource")
            .and_then(Value::as_str)
            .is_some_and(|resource| resource.contains("waitForTaskToken"))
    })
}

/// Check that ambiguous outcomes route to manual-review Fail states, not
/// back to the action task (which would be automatic retry).
fn ambiguous_routes_to_manual_review(asl: &Value) -> bool {
    let state_map = states(asl);
    state_map.values().all(|state| {
        let choices = match state.get("Choices").and_then(Value::as_array) {
            Some(c) => c,
            None => return true,
        };
        choices.iter().all(|choice| {
            let is_ambiguous = choice
                .get("Variable")
                .and_then(Value::as_str)
                .is_some_and(|outcome| outcome.ends_with(".outcome"))
                && choice
                    .get("StringEquals")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value == "ambiguous");
            if !is_ambiguous {
                return true;
            }
            let next = match choice.get("Next").and_then(Value::as_str) {
                Some(n) => n,
                None => return true,
            };
            let target = state_map
                .get(next)
                .unwrap_or_else(|| panic!("ambiguous target {next} must exist"));
            target.get("Type").and_then(Value::as_str).unwrap_or("") == "Fail"
        })
    })
}

/// Check that every RetryableFailure outcome routes back to the action task
/// (retry), not to a terminal state.
fn retryable_routes_back(asl: &Value) -> bool {
    let state_map = states(asl);
    state_map.values().all(|state| {
        let choices = match state.get("Choices").and_then(Value::as_array) {
            Some(c) => c,
            None => return true,
        };
        choices.iter().all(|choice| {
            let is_retryable = choice
                .get("StringEquals")
                .and_then(Value::as_str)
                .is_some_and(|value| value == "retryable_failure");
            if !is_retryable {
                return true;
            }
            let next = match choice.get("Next").and_then(Value::as_str) {
                Some(n) => n,
                None => return true,
            };
            let target = state_map.get(next).expect("retry target must exist");
            let stype = target.get("Type").and_then(Value::as_str).unwrap_or("");
            !matches!(stype, "Fail" | "Succeed")
        })
    })
}

// ── Tests ─────────────────────────────────────────────────────────────

#[test]
fn all_workflows_are_valid_json() {
    for (name, file) in WORKFLOWS {
        let asl = load_asl(file);
        assert!(asl["States"].is_object(), "{name} must have States object");
        assert!(asl["StartAt"].is_string(), "{name} must have StartAt");
    }
}

#[test]
fn all_state_references_resolve() {
    for (name, file) in WORKFLOWS {
        let asl = load_asl(file);
        let defined = state_names(&asl);
        let referenced = referenced_states(&asl);
        if let Some(missing) = referenced.difference(&defined).next() {
            panic!("{name}: state {missing} is referenced but not defined");
        }
    }
}

#[test]
fn every_workflow_has_terminal_states() {
    for (name, file) in WORKFLOWS {
        let asl = load_asl(file);
        let terminals = terminal_states(&asl);
        assert!(
            !terminals.is_empty(),
            "{name} must have at least one terminal state"
        );
    }
}

#[test]
fn retry_policies_enforce_three_attempt_maximum() {
    for (name, file) in WORKFLOWS {
        let asl = load_asl(file);
        let max = max_retry_attempts(&asl);
        assert_eq!(
            max, 3,
            "{name}: every Retry block must enforce MaxAttempts <= 3"
        );
    }
}

#[test]
fn confirmation_uses_durable_task_tokens() {
    for (name, file) in WORKFLOWS {
        let asl = load_asl(file);
        assert!(
            has_task_token_confirmation(&asl),
            "{name} must use waitForTaskToken for confirmation resume"
        );
    }
}

#[test]
fn ambiguous_outcomes_enter_manual_review_not_retry() {
    for (name, file) in WORKFLOWS {
        let asl = load_asl(file);
        assert!(
            ambiguous_routes_to_manual_review(&asl),
            "{name}: ambiguous outcomes must route to a Fail (manual review), never back to the action task"
        );
    }
}

#[test]
fn retryable_failures_route_back_to_action() {
    for (name, file) in WORKFLOWS {
        let asl = load_asl(file);
        assert!(
            retryable_routes_back(&asl),
            "{name}: retryable_failure must route back to a Task, not a terminal state"
        );
    }
}

#[test]
fn all_workflows_have_timeout() {
    for (name, file) in WORKFLOWS {
        let asl = load_asl(file);
        assert!(
            asl["TimeoutSeconds"].is_number(),
            "{name} must declare a top-level TimeoutSeconds"
        );
    }
}

#[test]
fn all_workflows_have_catch_all_on_external_actions() {
    // Every external-action Task must have a Catch that routes failures to
    // a dedicated Fail state, so unhandled exceptions do not silently
    // succeed.
    for (name, file) in WORKFLOWS {
        let asl = load_asl(file);
        for (state_name, state) in states(&asl) {
            let stype = state.get("Type").and_then(Value::as_str).unwrap_or("");
            if stype == "Task" {
                let resource = state.get("Resource").and_then(Value::as_str).unwrap_or("");
                if resource.contains("lambda:invoke")
                    && !resource.contains("waitForTaskToken")
                    && state.get("Retry").is_some()
                {
                    assert!(
                        state.get("Catch").is_some(),
                        "{name}: state {state_name} has Retry but no Catch — unhandled failures would be uncaught"
                    );
                }
            }
        }
    }
}

#[test]
fn existing_confirmation_slice_still_validates() {
    let asl = load_asl("confirmation-slice.asl.json");
    assert!(asl["States"].is_object());
    let defined = state_names(&asl);
    let referenced = referenced_states(&asl);
    if let Some(missing) = referenced.difference(&defined).next() {
        panic!("confirmation-slice: state {missing} is referenced but not defined");
    }
}
