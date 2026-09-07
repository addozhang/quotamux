//! Account occupancy survives publication, including changes in pool membership.
#[cfg(test)]
use crate::config::CapacityPool;
use crate::config::Config;
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
pub struct Admission {
    state: Mutex<Counters>,
}
#[derive(Default)]
struct Counters {
    limits: HashMap<String, usize>,
    accounts: HashMap<String, Account>,
}
struct Account {
    pool: Option<String>,
    in_flight: usize,
}
#[derive(Clone, Serialize)]
pub struct Pool {
    pub id: String,
    pub in_flight: usize,
    pub max_in_flight: usize,
}
pub struct Lease {
    owner: Arc<Admission>,
    account: String,
}
pub fn account_key(backend: &str, credential: &str) -> String {
    format!(
        "{}:{}{}:{}",
        backend.len(),
        backend,
        credential.len(),
        credential
    )
}
impl Counters {
    fn occupied(&self, pool: &str) -> usize {
        self.accounts
            .values()
            .filter(|a| a.pool.as_deref() == Some(pool))
            .map(|a| a.in_flight)
            .sum()
    }
}
impl Admission {
    pub fn publish_config(&self, config: &Config) {
        let mut state = self.state.lock().unwrap();
        for pool in &config.capacity_pools {
            state.limits.insert(pool.id.clone(), pool.max_in_flight);
        }
        for backend in &config.backends {
            for credential in &backend.credentials {
                let account = state
                    .accounts
                    .entry(account_key(&backend.id, &credential.id))
                    .or_insert(Account {
                        pool: None,
                        in_flight: 0,
                    });
                account.pool = credential.pool.clone();
            }
        }
        // Retired accounts remain available to already admitted routing snapshots.
        // Counts are per account, so assigning/renaming a pool includes existing work.
    }
    #[cfg(test)]
    fn publish(&self, limits: &[CapacityPool]) {
        let mut state = self.state.lock().unwrap();
        for pool in limits {
            state.limits.insert(pool.id.clone(), pool.max_in_flight);
        }
    }
    pub fn acquire_account(self: &Arc<Self>, key: &str, pool: Option<&str>) -> Option<Lease> {
        let mut state = self.state.lock().unwrap();
        let pool = state
            .accounts
            .get(key)
            .map(|a| a.pool.clone())
            .unwrap_or_else(|| pool.map(str::to_owned));
        if let Some(id) = &pool {
            let limit = state.limits.get(id)?;
            if state.occupied(id) >= *limit {
                return None;
            }
        }
        state
            .accounts
            .entry(key.to_owned())
            .or_insert(Account { pool, in_flight: 0 })
            .in_flight += 1;
        Some(Lease {
            owner: self.clone(),
            account: key.to_owned(),
        })
    }
    #[cfg(test)]
    fn try_acquire(self: &Arc<Self>, pool: Option<&str>) -> Option<Lease> {
        self.acquire_account(pool.unwrap_or("unlimited"), pool)
    }
    pub fn snapshot(&self, id: &str) -> Option<Pool> {
        let state = self.state.lock().unwrap();
        state.limits.get(id).map(|limit| Pool {
            id: id.into(),
            in_flight: state.occupied(id),
            max_in_flight: *limit,
        })
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.owner.state.lock().unwrap();
        let account = state
            .accounts
            .get_mut(&self.account)
            .expect("leased account retained");
        account.in_flight -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reduction_and_removal_preserve_outstanding_leases() {
        let gate = Arc::new(Admission::default());
        gate.publish(&[CapacityPool {
            id: "account".into(),
            max_in_flight: 2,
        }]);
        let a = gate.try_acquire(Some("account")).unwrap();
        let b = gate.try_acquire(Some("account")).unwrap();
        gate.publish(&[]);
        gate.publish(&[CapacityPool {
            id: "account".into(),
            max_in_flight: 1,
        }]);
        assert!(gate.try_acquire(Some("account")).is_none());
        drop(a);
        assert!(gate.try_acquire(Some("account")).is_none());
        drop(b);
        assert!(gate.try_acquire(Some("account")).is_some());
    }
    #[test]
    fn concurrent_admission_never_exceeds_limit() {
        let gate = Arc::new(Admission::default());
        gate.publish(&[CapacityPool {
            id: "account".into(),
            max_in_flight: 3,
        }]);
        let barrier = Arc::new(std::sync::Barrier::new(32));
        let entered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for _ in 0..32 {
                let (gate, barrier, entered) = (gate.clone(), barrier.clone(), entered.clone());
                scope.spawn(move || {
                    let lease = gate.try_acquire(Some("account"));
                    if lease.is_some() {
                        entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    barrier.wait();
                    assert_eq!(entered.load(std::sync::atomic::Ordering::SeqCst), 3);
                    barrier.wait();
                    drop(lease);
                });
            }
        });
        assert_eq!(gate.snapshot("account").unwrap().in_flight, 0);
    }
}
