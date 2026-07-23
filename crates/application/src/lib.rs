#![deny(unsafe_code)]

//! Application services and infrastructure-independent configuration.

pub mod artifact_delivery;
pub mod attachments;
pub mod calendar;
pub mod config;
pub mod config_loader;
pub mod cost_guard;
pub mod email;
pub mod existing_files;
pub mod external_operation;
pub mod normalization;
pub mod observability;
pub mod ports;
pub mod publication;
pub mod quotation;
pub mod repositories;
pub mod resumable_confirmation;
pub mod sanitized_history;
