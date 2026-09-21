//! Scope filtering over bounded store pages. Scanned progress is independent
//! of matching output, including workspace history after a review's deletion.

use nits_protocol::{ReplayPage, ReplayPosition, SubscribeScope};

use crate::{Core, CoreError, store::StoreError};

impl Core {
    pub fn replay_events(
        &self,
        scope: &SubscribeScope,
        position: ReplayPosition,
    ) -> Result<ReplayPage, CoreError> {
        let mut page = match self.store.replay_page(position) {
            Err(StoreError::Replay { reason }) => return Err(CoreError::Invalid { reason }),
            result => result?,
        };
        page.events.retain(|event| {
            scope.matches(event, |review| self.stored_review_workspace(review).ok())
        });
        Ok(page)
    }
}
