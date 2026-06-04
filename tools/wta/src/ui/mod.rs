mod auth;
pub(crate) mod card;
pub(crate) mod chat;
mod command_popup;
mod debug_panel;
mod input;
mod layout;
mod model_popup;
mod permission;
mod recommendations;
pub mod agents_view;
pub mod setup;
pub mod shimmer;

pub use shimmer::CYCLE_FRAMES as ACTIVITY_CYCLE_FRAMES;
pub use command_popup::PopupState;
pub use layout::render;
pub use model_popup::ModelPopupState;
