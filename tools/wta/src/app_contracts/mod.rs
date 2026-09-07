//! Data contracts shared by the App reducer and its event producers.
//!
//! These types carry data across application, ACP, input, and infrastructure
//! boundaries. Mutable UI state remains owned by `app`.

mod agent;
mod command;
mod config;
mod diagnostics;
mod event;
mod model;
mod permission;
mod plan;
mod preflight;
mod yolo;

pub use agent::AvailableAgent;
pub use command::{AcpSessionCommand, CompletionBehavior};
pub use config::{AcpSessionConfigOption, AcpSessionConfigValue};
pub use diagnostics::{DebugDir, DebugMessage};
pub use event::AppEvent;
pub use model::AcpModelInfo;
pub use permission::PermOption;
pub use plan::{PlanEntry, PlanEntryStatus};
pub use preflight::{CheckStatus, PreflightResult};
pub use yolo::{SharedYoloState, YoloState};
