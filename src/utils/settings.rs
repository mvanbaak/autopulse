use std::collections::HashMap;

use config::{Config, File};
use serde::Deserialize;

use crate::{
    db::models::ScanEvent,
    service::{
        targets::{command::Command, jellyfin::Jellyfin, plex::Plex},
        triggers::{radarr::RadarrRequest, sonarr::SonarrRequest},
        webhooks::discord::DiscordWebhook,
    },
};

#[derive(Deserialize, Clone, Debug)]
pub struct App {
    pub hostname: String,
    pub port: u16,
    pub database_url: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Auth {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Opts {
    pub check_path: bool,
    pub max_retries: i32,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Settings {
    pub app: App,
    pub auth: Auth,
    pub opts: Opts,

    pub triggers: HashMap<String, Trigger>,
    pub targets: HashMap<String, Target>,

    pub webhooks: HashMap<String, Webhook>,
}

impl Settings {
    pub fn get_settings() -> anyhow::Result<Settings> {
        let settings = Config::builder()
            .add_source(File::with_name("default.toml"))
            .add_source(config::File::with_name("config").required(false))
            .add_source(config::Environment::with_prefix("AUTOPULSE").separator("__"))
            .build()
            .unwrap();

        settings
            .try_deserialize::<Settings>()
            .map_err(|e| anyhow::anyhow!(e))
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct Rewrite {
    pub from: String,
    pub to: String,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Trigger {
    Manual { rewrite: Option<Rewrite> },
    Radarr { rewrite: Option<Rewrite> },
    Sonarr { rewrite: Option<Rewrite> },
}

impl Trigger {
    pub fn paths(&self, body: serde_json::Value) -> anyhow::Result<Vec<String>> {
        match &self {
            Trigger::Sonarr { .. } => Ok(SonarrRequest::from_json(body)?.paths()),
            Trigger::Radarr { .. } => Ok(RadarrRequest::from_json(body)?.paths()),
            _ => todo!(),
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Webhook {
    Discord(DiscordWebhook),
}

pub trait TargetProcess {
    fn process(
        &mut self,
        file_path: &ScanEvent,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
}

pub trait TriggerRequest {
    fn from_json(json: serde_json::Value) -> anyhow::Result<Self>
    where
        Self: Sized;

    fn paths(&self) -> Vec<String>;
}

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Target {
    Plex(Plex),
    Jellyfin(Jellyfin),
    Command(Command),
}

impl Target {
    pub async fn process(&mut self, ev: &ScanEvent) -> anyhow::Result<()> {
        match self {
            Target::Plex(p) => p.process(ev).await,
            Target::Jellyfin(j) => j.process(ev).await,
            Target::Command(c) => c.process(ev).await,
        }
    }
}
