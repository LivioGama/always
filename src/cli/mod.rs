pub mod logs;
pub mod menu_bar;
pub mod model;
pub mod update;

pub use logs::{LogsCommand, handle_logs};
pub use model::{ModelAction, handle_model};
