#![cfg_attr(test, allow(clippy::items_after_test_module))]

pub use jcode_storage::*;

use anyhow::Result;
use serde::de::DeserializeOwned;
use std::path::Path;

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    jcode_storage::read_json_with_recovery_handler(path, |event| match event {
        jcode_storage::StorageRecoveryEvent::CorruptPrimary { path, error } => {
            crate::logging::warn(&format!(
                "Corrupt JSON at {}, trying backup: {}",
                path.display(),
                error
            ));
        }
        jcode_storage::StorageRecoveryEvent::RecoveredFromBackup { backup_path } => {
            crate::logging::info(&format!("Recovered from backup: {}", backup_path.display()));
        }
    })
}

#[cfg(any(test, feature = "test-support"))]
use std::sync::{Mutex, MutexGuard, OnceLock};

#[cfg(any(test, feature = "test-support"))]
pub fn test_env_lock() -> &'static Mutex<()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// How many live [`TestEnvGuard`]s this thread owns. Only the outermost
    /// one holds the mutex; nested ones are no-ops.
    static TEST_ENV_LOCK_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Whether this thread already owns the test-env lock.
#[cfg(any(test, feature = "test-support"))]
pub fn test_env_lock_held() -> bool {
    TEST_ENV_LOCK_DEPTH.with(|depth| depth.get() > 0)
}

/// Guard for the process-global test-env lock.
///
/// Reentrant per thread: the outermost guard owns the mutex, nested ones only
/// bump a counter. The mutex itself is a plain non-reentrant `Mutex`, and
/// several helpers used to work around that with `try_lock` or by documenting
/// "do not take this here". Tracking depth lets any layer ask for the lock
/// without knowing whether an outer layer already took it, which is what makes
/// a single canonical lock order enforceable (see
/// `jcode_tui::tui::ui::render_state_test_lock`).
#[cfg(any(test, feature = "test-support"))]
pub struct TestEnvGuard {
    _guard: Option<MutexGuard<'static, ()>>,
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for TestEnvGuard {
    fn drop(&mut self) {
        TEST_ENV_LOCK_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn lock_test_env() -> TestEnvGuard {
    let guard = if test_env_lock_held() {
        None
    } else {
        Some(
            test_env_lock()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    };
    TEST_ENV_LOCK_DEPTH.with(|depth| depth.set(depth.get() + 1));
    TestEnvGuard { _guard: guard }
}

#[cfg(test)]
mod tests;
