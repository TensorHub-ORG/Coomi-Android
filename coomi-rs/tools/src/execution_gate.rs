//! Cross-session tool coordination. A process-wide gate also covers cwd aliases,
//! overlapping workspaces, shared Git indexes and sub-agents created by CoreTools.
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

static ACCESS: RwLock<()> = RwLock::const_new(());

#[allow(dead_code)] // Guards are held for their Drop behavior.
pub(crate) enum Lease {
    Read(RwLockReadGuard<'static, ()>),
    Write(RwLockWriteGuard<'static, ()>),
}

pub(crate) async fn enter(read_only: bool) -> Lease {
    if read_only {
        Lease::Read(ACCESS.read().await)
    } else {
        Lease::Write(ACCESS.write().await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn readers_overlap_and_mutations_wait_until_release() {
        let first = enter(true).await;
        let second = tokio::time::timeout(std::time::Duration::from_secs(1), enter(true))
            .await
            .unwrap();
        assert!(ACCESS.try_write().is_err());
        drop((first, second));
        let writer = enter(false).await;
        assert!(ACCESS.try_read().is_err());
        assert!(ACCESS.try_write().is_err());
        drop(writer);
        assert!(ACCESS.try_read().is_ok());
    }
}
