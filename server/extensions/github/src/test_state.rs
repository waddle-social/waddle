use super::types;
use super::{GitHubConfig, Route};
use std::sync::{Mutex, OnceLock};

fn config_cell() -> &'static Mutex<GitHubConfig> {
    static CONFIG: OnceLock<Mutex<GitHubConfig>> = OnceLock::new();
    CONFIG.get_or_init(|| Mutex::new(GitHubConfig::default()))
}

fn route_cell() -> &'static Mutex<Vec<Route>> {
    static ROUTES: OnceLock<Mutex<Vec<Route>>> = OnceLock::new();
    ROUTES.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn config_fixture() -> GitHubConfig {
    config_cell().lock().expect("config lock").clone()
}

pub fn set_config_fixture(config: GitHubConfig) {
    *config_cell().lock().expect("config lock") = config;
}

pub fn route_fixtures() -> Vec<Route> {
    route_cell().lock().expect("route lock").clone()
}

pub fn set_route_fixtures(routes: Vec<Route>) {
    *route_cell().lock().expect("route lock") = routes;
}

pub fn sent_room_messages() -> &'static Mutex<Vec<types::SendMessageRequest>> {
    static SENT: OnceLock<Mutex<Vec<types::SendMessageRequest>>> = OnceLock::new();
    SENT.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn reset() {
    set_config_fixture(GitHubConfig::default());
    set_route_fixtures(vec![]);
    sent_room_messages().lock().expect("sent lock").clear();
}
