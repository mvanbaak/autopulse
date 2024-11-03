#![cfg(test)]
mod tests {
    use crate::service::triggers::notify::Notify;
    use notify::{event::CreateKind, EventKind};
    use std::{env, fs::create_dir, time::Duration};
    use tokio::time::timeout;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_notify() -> anyhow::Result<()> {
        let path = env::temp_dir().join(Uuid::new_v4().to_string());
        create_dir(&path)?;

        let notifier = Notify {
            paths: vec![path.clone().to_string_lossy().to_string()],
            rewrite: None,
            recursive: None,
            excludes: vec![],
            timer: Default::default(),
        };

        let (_, mut rx) = notifier.async_watcher()?;

        let file = path.join("test.txt");
        std::fs::File::create(&file)?;

        let _ = timeout(Duration::from_secs(3), async {
            if let Some(event) = rx.recv().await {
                let event = event?;
                assert!(event.kind == EventKind::Create(CreateKind::File));
                return Ok(());
            }
            anyhow::bail!("Event not received within timeout");
        })
        .await?;

        Ok(())
    }
}