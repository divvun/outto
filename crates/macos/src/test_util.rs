use std::sync::{Mutex, MutexGuard, OnceLock};

static HOME_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) fn lock_home() -> MutexGuard<'static, ()> {
    HOME_MUTEX
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}
