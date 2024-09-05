use std::path::PathBuf;

use crate::{
    db::{
        models::{FoundStatus, NewScanEvent, ScanEvent},
        schema::{
            self,
            scan_events::{
                dsl::scan_events, found_at, found_status, id, next_retry_at, process_status,
            },
        },
    },
    service::webhooks::WebhookManager,
    utils::{conn::get_conn, settings::Settings},
    DbPool,
};
use diesel::{
    dsl::count, BoolExpressionMethods, ExpressionMethods, QueryDsl, RunQueryDsl, SaveChangesDsl,
    SelectableHelper,
};
use serde::Serialize;
use tracing::{error, info};
use webhooks::EventType;

pub mod targets;
pub mod triggers;
pub mod webhooks;

#[derive(Clone, Serialize)]
pub struct Stats {
    total: i64,
    found: i64,
    pending: i64,
    processed: i64,
    failed: i64,
}

#[derive(Clone)]
pub struct PulseService {
    pub settings: Settings,
    pub pool: DbPool,
    pub webhooks: WebhookManager,
}

struct PulseRunner {
    webhooks: WebhookManager,
    settings: Settings,
    pool: DbPool,
}

impl PulseRunner {
    pub const fn new(settings: Settings, pool: DbPool, webhooks: WebhookManager) -> Self {
        Self {
            webhooks,
            settings,
            pool,
        }
    }

    async fn update_found_status(&self) -> anyhow::Result<()> {
        if !self.settings.opts.check_path {
            return Ok(());
        }

        let mut count = vec![];

        let mut conn = get_conn(&self.pool);
        let mut evs = scan_events
            .filter(found_status.ne(FoundStatus::Found))
            .load::<ScanEvent>(&mut conn)?;

        for ev in &mut evs {
            let file_path = PathBuf::from(&ev.file_path);

            if file_path.exists() {
                let file_hash = crate::utils::checksum::sha256checksum(&file_path);

                ev.found_status = FoundStatus::Found;

                if let Some(hash) = ev.file_hash.clone() {
                    if hash != file_hash {
                        ev.found_status = FoundStatus::HashMismatch;
                        ev.found_at = Some(chrono::Utc::now().naive_utc());
                    }
                } else {
                    ev.found_at = Some(chrono::Utc::now().naive_utc());
                    count.push(ev.file_path.clone());
                }
            }

            ev.updated_at = chrono::Utc::now().naive_utc();
            ev.save_changes::<ScanEvent>(&mut conn)?;
        }

        if !count.is_empty() {
            info!(
                "found {} new file{}",
                count.len(),
                if count.len() > 1 { "s" } else { "" }
            );

            self.webhooks.send(EventType::Found, None, &count).await;
        }

        Ok(())
    }

    pub async fn update_process_status(&mut self) -> anyhow::Result<()> {
        let mut processed = vec![];
        let mut failed = vec![];

        let mut conn = get_conn(&self.pool);
        let mut evs = {
            let base_query = scan_events
                .filter(process_status.ne(crate::db::models::ProcessStatus::Complete))
                .filter(process_status.ne(crate::db::models::ProcessStatus::Failed))
                .filter(
                    next_retry_at
                        .is_null()
                        .or(next_retry_at.lt(chrono::Utc::now().naive_utc())),
                );

            if self.settings.opts.check_path {
                base_query
                    .filter(found_status.eq(FoundStatus::Found))
                    .load::<ScanEvent>(&mut conn)?
            } else {
                base_query.load::<ScanEvent>(&mut conn)?
            }
        };

        for ev in &mut evs {
            let res = self.process_event(ev).await;

            if let Ok((succeeded, _)) = &res {
                ev.targets_hit.append(&mut succeeded.clone());
            }

            if res.is_err() || !res.as_ref().unwrap().1.is_empty() {
                ev.failed_times += 1;

                if ev.failed_times >= self.settings.opts.max_retries {
                    ev.process_status = crate::db::models::ProcessStatus::Failed;
                    ev.next_retry_at = None;
                    failed.push(ev.file_path.clone());
                } else {
                    let next_retry = chrono::Utc::now().naive_utc()
                        + chrono::Duration::seconds(2_i64.pow(ev.failed_times as u32 + 1));

                    ev.process_status = crate::db::models::ProcessStatus::Retry;
                    ev.next_retry_at = Some(next_retry);
                }
            } else {
                ev.process_status = crate::db::models::ProcessStatus::Complete;
                ev.processed_at = Some(chrono::Utc::now().naive_utc());
                processed.push(ev.file_path.clone());
            }

            ev.updated_at = chrono::Utc::now().naive_utc();
            ev.save_changes::<ScanEvent>(&mut conn)?;
        }

        if !processed.is_empty() {
            info!(
                "sent {} file{} to targets",
                processed.len(),
                if processed.len() > 1 { "s" } else { "" }
            );

            self.webhooks
                .send(EventType::Processed, None, &processed)
                .await;
        }

        if !failed.is_empty() {
            error!(
                "failed to send {} file{} to targets",
                failed.len(),
                if failed.len() > 1 { "s" } else { "" }
            );

            self.webhooks.send(EventType::Error, None, &failed).await;
        }

        Ok(())
    }

    async fn process_event(
        &mut self,
        ev: &ScanEvent,
    ) -> anyhow::Result<(Vec<String>, Vec<String>)> {
        let mut succeeded = vec![];
        let mut failed = vec![];

        for (name, target) in &mut self.settings.targets {
            if !ev.targets_hit.is_empty() && ev.targets_hit.contains(name) {
                continue;
            }

            let res = target.process(ev).await;

            match res {
                Ok(()) => succeeded.push(name.clone()),
                Err(e) => {
                    failed.push(name.clone());
                    error!("failed to process target '{}': {:?}", name, e);
                }
            }
        }

        Ok((succeeded, failed))
    }

    fn cleanup(&self) -> anyhow::Result<()> {
        let mut conn = get_conn(&self.pool);

        // TODO: make this a setting
        let time_before_cleanup = chrono::Utc::now().naive_utc() - chrono::Duration::days(10);

        let _ = diesel::delete(
            scan_events
                .filter(found_status.eq(crate::db::models::FoundStatus::NotFound))
                .filter(found_at.lt(time_before_cleanup)),
        );

        let _ = diesel::delete(
            scan_events
                .filter(process_status.eq(crate::db::models::ProcessStatus::Failed))
                .filter(found_at.lt(time_before_cleanup)),
        )
        .execute(&mut conn)?;

        Ok(())
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        self.update_found_status().await?;
        self.update_process_status().await?;
        self.cleanup()?;

        Ok(())
    }
}

impl PulseService {
    pub fn new(settings: Settings, pool: DbPool) -> Self {
        Self {
            settings: settings.clone(),
            pool,
            webhooks: WebhookManager::new(settings),
        }
    }

    pub fn get_stats(&self) -> anyhow::Result<Stats> {
        let mut conn = get_conn(&self.pool);

        let result = scan_events
            .select((
                count(id),
                count(found_status.eq(FoundStatus::Found)),
                count(
                    found_status
                        .eq(crate::db::models::FoundStatus::Found)
                        .and(process_status.eq(crate::db::models::ProcessStatus::Pending)),
                ),
                count(process_status.eq(crate::db::models::ProcessStatus::Complete)),
                count(
                    process_status
                        .eq(crate::db::models::ProcessStatus::Failed)
                        .or(process_status.eq(crate::db::models::ProcessStatus::Retry)),
                ),
            ))
            .first::<(i64, i64, i64, i64, i64)>(&mut conn)?;

        let (total, found, pending, processed, failed) = result;

        Ok(Stats {
            total,
            pending,
            found,
            processed,
            failed,
        })
    }

    pub fn add_event(&self, ev: &NewScanEvent) -> anyhow::Result<ScanEvent> {
        let mut conn = get_conn(&self.pool);

        diesel::insert_into(schema::scan_events::table)
            .values(ev)
            .returning(ScanEvent::as_returning())
            .get_result::<ScanEvent>(&mut conn)
            .map_err(Into::into)
    }

    pub fn get_event(&self, scan_id: &i32) -> Option<ScanEvent> {
        let mut conn = get_conn(&self.pool);

        scan_events.find(scan_id).first::<ScanEvent>(&mut conn).ok()
    }

    pub fn start(&self) {
        let settings = self.settings.clone();
        let pool = self.pool.clone();
        let webhooks = self.webhooks.clone();

        tokio::spawn(async move {
            let mut runner = PulseRunner::new(settings, pool, webhooks);
            let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));

            loop {
                if let Err(e) = runner.run().await {
                    error!("unable to run pulse: {:?}", e);
                }

                timer.tick().await;
            }
        });
    }
}
