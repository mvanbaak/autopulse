use crate::{db::models::ScanEvent, utils::settings::TargetProcess};
use reqwest::header;
use serde::{Deserialize, Serialize};
use tracing::debug;

#[derive(Deserialize, Clone, Debug)]
pub struct Jellyfin {
    pub url: String,
    pub token: String,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
struct Library {
    #[allow(dead_code)]
    name: String,
    locations: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
struct UpdateRequest {
    path: String,
    update_type: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
struct ScanPayload {
    updates: Vec<UpdateRequest>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
struct Item {
    id: String,
    path: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
struct ItemsResponse {
    items: Vec<Item>,
}

impl Jellyfin {
    fn get_client(&self) -> anyhow::Result<reqwest::Client> {
        let mut headers = header::HeaderMap::new();

        headers.insert("X-Emby-Token", self.token.parse().unwrap());
        headers.insert("Accept", "application/json".parse().unwrap());

        reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .map_err(Into::into)
    }

    async fn libraries(&self) -> anyhow::Result<Vec<Library>> {
        let client = self.get_client()?;
        let url = url::Url::parse(&self.url)?
            .join("/Library/VirtualFolders")?
            .to_string();

        let res = client.get(&url).send().await?;
        let libraries: Vec<Library> = res.json().await?;

        Ok(libraries)
    }

    // sadly this is quite memory intensive, maybe a stream option is possible
    async fn find_item(&self, path: &str) -> anyhow::Result<Option<Item>> {
        let client = self.get_client()?;
        let mut url = url::Url::parse(&self.url)?.join("/Items")?;

        url.query_pairs_mut().append_pair("Recursive", "true");
        url.query_pairs_mut().append_pair("Fields", "Path");
        url.query_pairs_mut().append_pair("EnableImages", "false");

        let res = client.get(url.to_string()).send().await?;

        let res = res.json::<ItemsResponse>().await?;

        let item = res
            .items
            .iter()
            .find(|item| item.path == Some(path.to_string()));

        Ok(item.cloned())
    }

    // not as effective as refreshing the item, but good enough
    async fn scan(&self, ev: &ScanEvent) -> anyhow::Result<()> {
        let client = self.get_client()?;
        let url = url::Url::parse(&self.url)?
            .join("/Library/Media/Updated")?
            .to_string();

        let req = UpdateRequest {
            path: ev.file_path.clone(),
            update_type: "Modified".to_string(),
        };

        let body = ScanPayload { updates: vec![req] };

        let res = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if res.status().is_success() {
            Ok(())
        } else {
            let body = res.text().await?;
            Err(anyhow::anyhow!("Failed to send scan: {}", body))
        }
    }

    async fn refresh_item(&self, item: &Item) -> anyhow::Result<()> {
        let client = self.get_client()?;
        let mut url = url::Url::parse(&self.url)?.join(&format!("/Items/{}/Refresh", item.id))?;

        // TODO: make this a setting the user can choose, along with the other options
        url.query_pairs_mut()
            .append_pair("metadataRefreshMode", "FullRefresh");

        let res = client.post(url.to_string()).send().await?;

        if res.status().is_success() {
            Ok(())
        } else {
            let body = res.text().await?;
            Err(anyhow::anyhow!("Failed to refresh item: {}", body))
        }
    }
}

impl TargetProcess for Jellyfin {
    async fn process(&self, ev: &ScanEvent) -> anyhow::Result<()> {
        let libraries = self.libraries().await?;

        // check if the file path is in any of the library locations
        libraries
            .iter()
            .find(|library| {
                library
                    .locations
                    .iter()
                    .any(|location| ev.file_path.starts_with(location))
            })
            .ok_or_else(|| {
                anyhow::anyhow!("File path {} not in any jellyfin library", ev.file_path)
            })?;

        if let Some(item) = self.find_item(&ev.file_path).await? {
            debug!("Found item: {:?}", item);
            self.refresh_item(&item).await?;
        } else {
            debug!("Item not found, scanning instead");
            self.scan(ev).await?;
        }

        Ok(())
    }
}
