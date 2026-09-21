//! Linearizable admission and accounting for work retained outside Tokio tasks.
//!
//! Closing the gate and accepting work share one mutex. The permit moves into
//! the actual blocking read/writer job, so cancelling its caller cannot make a
//! planned replacement mistake a still-running Git operation for drained work.

use std::sync::{Arc, Mutex};

use nits_protocol::UpgradeOperation;
use tokio::sync::{Notify, watch};

#[derive(Debug)]
struct State {
    operation: Option<UpgradeOperation>,
    active: usize,
}

#[derive(Debug)]
pub(crate) struct Admission {
    state: Mutex<State>,
    changed: Notify,
    lifecycle: watch::Sender<Option<UpgradeOperation>>,
}

#[derive(Debug)]
pub(crate) struct Permit(Arc<Admission>);

impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active -= 1;
        self.0.changed.notify_waiters();
    }
}

impl Admission {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                operation: None,
                active: 0,
            }),
            changed: Notify::new(),
            lifecycle: watch::channel(None).0,
        })
    }

    pub(crate) fn accept(self: &Arc<Self>) -> Result<Permit, crate::daemon::DaemonError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(operation) = &state.operation {
            Err(crate::daemon::DaemonError::Restarting(operation.id))
        } else {
            state.active += 1;
            Ok(Permit(Arc::clone(self)))
        }
    }

    /// First owner wins; repeated control requests observe that same operation.
    pub(crate) fn close(&self, operation: UpgradeOperation) -> (UpgradeOperation, bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(current) = &state.operation {
            (current.clone(), false)
        } else {
            state.operation = Some(operation.clone());
            self.lifecycle.send_replace(Some(operation.clone()));
            (operation, true)
        }
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<Option<UpgradeOperation>> {
        self.lifecycle.subscribe()
    }

    pub(crate) async fn drained(&self) {
        loop {
            // Register before checking so a final permit cannot be lost between
            // the count check and waiting for its notification.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active
                == 0
            {
                return;
            }
            changed.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nits_protocol::{UpgradeId, UpgradeProgress, UpgradeStage};

    fn operation() -> UpgradeOperation {
        let mut target = crate::build::running().unwrap();
        let source = target.clone();
        target.digest = nits_protocol::BuildDigest::from_bytes([42; 32]);
        UpgradeOperation {
            id: UpgradeId::from_parts(1, 1),
            source,
            target,
            progress: UpgradeProgress::Active {
                stage: UpgradeStage::Draining,
            },
        }
    }

    #[tokio::test]
    async fn closing_is_linearizable_and_only_the_actual_permit_releases_work() {
        let admission = Admission::new();
        let permit = admission.accept().unwrap();
        let mut notice = admission.subscribe();
        let requested = operation();
        assert_eq!(
            admission.close(requested.clone()),
            (requested.clone(), true)
        );
        notice.changed().await.unwrap();
        assert_eq!(*notice.borrow(), Some(requested.clone()));
        assert!(
            matches!(admission.accept(),Err(crate::daemon::DaemonError::Restarting(id)) if id==requested.id)
        );
        let mut competing = requested.clone();
        competing.id = UpgradeId::from_parts(1, 2);
        assert_eq!(admission.close(competing), (requested, false));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), admission.drained())
                .await
                .is_err()
        );
        drop(permit);
        tokio::time::timeout(std::time::Duration::from_secs(1), admission.drained())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cancelling_the_request_does_not_drop_its_started_blocking_work_permit() {
        let admission = Admission::new();
        let permit = admission.accept().unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, hold) = std::sync::mpsc::channel();
        let caller = tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                started.send(()).unwrap();
                hold.recv().unwrap();
            })
            .await
            .unwrap();
        });
        ready.await.unwrap();
        caller.abort();
        let _ = caller.await;
        admission.close(operation());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), admission.drained())
                .await
                .is_err()
        );
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), admission.drained())
            .await
            .unwrap();
    }
}
