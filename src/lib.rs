pub mod bootstrap;
pub mod browser_install;
pub mod cdp;
pub mod config;
pub mod copilot;
pub mod doctor;
pub mod logging;
pub mod models;
pub mod onboarding;
pub mod openapi;
pub mod routes;
pub mod runtime;
pub mod runtime_status;
pub mod session_store;
pub mod substrate_client;
pub mod token_store;
pub mod translator;
pub mod tray;
pub mod tui;

pub use config::{AppConfig, ServeOverrides, Settings};
pub use copilot::FakeCopilotClient;
pub use routes::{
    app_state_with_client, create_router, default_app_state, default_app_state_simple, AppState,
};
