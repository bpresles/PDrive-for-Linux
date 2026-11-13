// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Folder listings in flight: one request per folder, however many callers want it.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

/// Runs one operation at a time per key. A caller arriving while another one runs the same key
/// waits for it, then runs its own operation, which is expected to notice that the work is done
/// (e.g. the folder is loaded) and return at once.
#[derive(Default)]
pub struct InFlight {
    locks: Mutex<HashMap<u64, Arc<tokio::sync::Mutex<()>>>>,
}

impl InFlight {
    pub async fn run<T>(&self, key: u64, operation: impl Future<Output = T>) -> T {
        let lock = self.locks().entry(key).or_default().clone();
        let result = {
            let _turn = lock.lock().await;
            operation.await
        };
        // Forget the key unless someone else holds or waits for it (the map and `lock` own it).
        let mut locks = self.locks();
        if Arc::strong_count(&lock) == 2 {
            locks.remove(&key);
        }
        result
    }

    fn locks(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Arc<tokio::sync::Mutex<()>>>> {
        self.locks.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.locks().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// What `Drive::load_folder` does: check, list once, mark as loaded.
    async fn load(flights: Arc<InFlight>, loaded: Arc<Mutex<bool>>, requests: Arc<AtomicUsize>, key: u64) {
        let work = async {
            if *loaded.lock().unwrap() {
                return;
            }
            requests.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            *loaded.lock().unwrap() = true;
        };
        flights.run(key, work).await;
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_request() {
        let flights = Arc::new(InFlight::default());
        let (loaded, requests) = (Arc::new(Mutex::new(false)), Arc::new(AtomicUsize::new(0)));
        let tasks: Vec<_> = (0..5).map(|_| tokio::spawn(load(flights.clone(), loaded.clone(), requests.clone(), 1))).collect();
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(*loaded.lock().unwrap());
        assert_eq!(flights.len(), 0);
    }

    #[tokio::test]
    async fn a_waiter_follows_a_failed_request_with_its_own() {
        let flights = Arc::new(InFlight::default());
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempt = |flights: Arc<InFlight>, attempts: Arc<AtomicUsize>| async move {
            flights
                .run(1, async {
                    let n = attempts.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    if n == 0 { Err(()) } else { Ok(()) }
                })
                .await
        };
        let first = tokio::spawn(attempt(flights.clone(), attempts.clone()));
        tokio::task::yield_now().await;
        let second = tokio::spawn(attempt(flights.clone(), attempts.clone()));
        assert!(first.await.unwrap().is_err());
        assert!(second.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn different_folders_do_not_wait_for_each_other() {
        let flights = Arc::new(InFlight::default());
        let requests = Arc::new(AtomicUsize::new(0));
        let started = std::time::Instant::now();
        let tasks: Vec<_> = (0..4).map(|key| tokio::spawn(load(flights.clone(), Arc::default(), requests.clone(), key))).collect();
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(requests.load(Ordering::SeqCst), 4);
        assert!(started.elapsed() < std::time::Duration::from_millis(110));
        assert_eq!(flights.len(), 0);
    }
}
