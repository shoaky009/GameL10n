//! Replaces the displayed log with the latest complete project snapshot.

use crate::{Context, view::App};
use gamel10n_core::CoreError;
use slint::ComponentHandle;
use std::{sync::atomic::Ordering, time::Duration};

pub async fn refresh(ctx: &Context, id: &str) -> Result<(), CoreError> {
    let epoch = ctx.root_generation.load(Ordering::Acquire);
    let revision = {
        let session = ctx
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if session.requested_project != id {
            return Ok(());
        }
        ctx.log_generation.fetch_add(1, Ordering::AcqRel) + 1
    };
    tokio::time::sleep(Duration::from_millis(25)).await;
    if ctx.log_generation.load(Ordering::Acquire) != revision
        || ctx.root_generation.load(Ordering::Acquire) != epoch
    {
        return Ok(());
    }
    let text = ctx.app().await.log_tail(id).await?;
    let id = id.to_owned();
    let generation = ctx.log_generation.clone();
    let root_generation = ctx.root_generation.clone();
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if generation.load(Ordering::Acquire) == revision
            && root_generation.load(Ordering::Acquire) == epoch
            && app.get_project_id().as_str() == id
        {
            app.set_log_text(text.into());
        }
    });
    Ok(())
}
