mod agent_popup;
pub(crate) mod action_panel;
pub mod agents_view;
mod auth;
pub(crate) mod card;
pub(crate) mod chat;
pub(crate) mod command_format;
mod command_popup;
mod config_popup;
mod debug_panel;
mod input;
mod layout;
mod model_popup;
mod permission;
mod popup;
mod recommendations;
pub mod setup;
pub mod shimmer;
mod user_input;

pub use agent_popup::AgentPopupState;
pub use command_popup::{CommandCandidate, PopupCandidates, PopupState};
pub use config_popup::ConfigPopupState;
#[cfg(test)]
pub(crate) use input::input_height;
pub use layout::render;
pub use model_popup::ModelPopupState;
pub use shimmer::CYCLE_FRAMES as ACTIVITY_CYCLE_FRAMES;
