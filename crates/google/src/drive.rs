use std::fmt::{self, Display};

use application::config::DriveConfig;
use application::external_operation::ExternalResourceId;

pub use crate::sheets_docs::FileKind;

/// A validated Google Drive folder identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FolderId(String);

impl FolderId {
    pub fn new(value: impl Into<String>) -> Result<Self, DriveDestinationError> {
        let value = value.into();
        validate_drive_id(&value, "folder_id")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated Google shared-drive identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SharedDriveId(String);

impl SharedDriveId {
    pub fn new(value: impl Into<String>) -> Result<Self, DriveDestinationError> {
        let value = value.into();
        validate_drive_id(&value, "shared_drive_id")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn validate_drive_id(value: &str, field: &'static str) -> Result<(), DriveDestinationError> {
    if value.is_empty() || value.len() > 128 {
        return Err(DriveDestinationError::InvalidFolderId { field });
    }
    if value.bytes().any(|b| b.is_ascii_control()) {
        return Err(DriveDestinationError::InvalidFolderId { field });
    }
    if value.bytes().any(|b| b.is_ascii_whitespace()) {
        return Err(DriveDestinationError::InvalidFolderId { field });
    }
    Ok(())
}

/// A user-visible destination selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriveDestination {
    CompanyDefault,
    EmployeeOverride(FolderId),
    WorkflowOverride(FolderId),
    MyDrive,
    SharedDrive(SharedDriveId),
}

/// The resolved folder and shared-drive identifiers for a Google Drive destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDestination {
    folder_id: Option<FolderId>,
    shared_drive: Option<SharedDriveId>,
}

impl ResolvedDestination {
    pub const fn new(folder_id: Option<FolderId>, shared_drive: Option<SharedDriveId>) -> Self {
        Self {
            folder_id,
            shared_drive,
        }
    }

    pub const fn folder_id(&self) -> Option<&FolderId> {
        self.folder_id.as_ref()
    }

    pub const fn shared_drive(&self) -> Option<&SharedDriveId> {
        self.shared_drive.as_ref()
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum DriveDestinationError {
    #[error("invalid folder id: {field}")]
    InvalidFolderId { field: &'static str },
    #[error("no usable drive default configured")]
    NoUsableDefault,
    #[error("shared drive not permitted by oauth scopes")]
    SharedDriveNotPermitted,
}

/// Resolve a destination selection against the deployment config and optional employee default.
///
/// Precedence: WorkflowOverride > EmployeeOverride > CompanyDefault > MyDrive.
pub fn resolve_destination(
    selection: &DriveDestination,
    config: &DriveConfig,
    employee_default: Option<&FolderId>,
) -> Result<ResolvedDestination, DriveDestinationError> {
    match selection {
        DriveDestination::WorkflowOverride(folder) => {
            Ok(ResolvedDestination::new(Some(folder.clone()), None))
        }
        DriveDestination::EmployeeOverride(folder) => {
            Ok(ResolvedDestination::new(Some(folder.clone()), None))
        }
        DriveDestination::SharedDrive(shared) => {
            let config_sd = config.shared_drive_id.as_deref().filter(|s| !s.is_empty());
            if config_sd.is_none() {
                return Err(DriveDestinationError::SharedDriveNotPermitted);
            }
            Ok(ResolvedDestination::new(None, Some(shared.clone())))
        }
        DriveDestination::CompanyDefault => {
            if let Some(emp) = employee_default {
                return Ok(ResolvedDestination::new(Some(emp.clone()), None));
            }
            let default = config.default_folder_id.as_str();
            if !default.is_empty() {
                let folder = FolderId::new(default.to_owned())?;
                return Ok(ResolvedDestination::new(Some(folder), None));
            }
            Err(DriveDestinationError::NoUsableDefault)
        }
        DriveDestination::MyDrive => Ok(ResolvedDestination::new(None, None)),
    }
}

/// A file that was successfully created in Google Drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedFile {
    pub resource_id: ExternalResourceId,
    pub kind: FileKind,
    pub title: String,
}

#[allow(async_fn_in_trait)]
pub trait DriveClient {
    type Error: Display + fmt::Debug;

    async fn copy_file(
        &self,
        token: &super::auth::GoogleAccessToken,
        source_resource_id: &str,
        destination: &ResolvedDestination,
        name: &str,
    ) -> Result<ExternalResourceId, Self::Error>;
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum DriveError {
    #[error("drive client error")]
    Client,
    #[error("invalid file reference")]
    InvalidReference,
}
