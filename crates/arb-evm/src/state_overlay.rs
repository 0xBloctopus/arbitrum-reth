use std::collections::HashMap;

use alloy_primitives::Address;
use revm::{database::State, Database, DatabaseCommit};
use revm_database::{AccountStatus as CacheAccountStatus, TransitionAccount};
use revm_state::{Account, AccountInfo};

#[derive(Clone, Debug)]
struct Entry {
    previous_info: Option<AccountInfo>,
    previous_status: CacheAccountStatus,
}

/// Records pre-mutation snapshots so direct State-cache writes performed
/// outside revm's normal transition flow can be committed back at end of
/// tx.
///
/// Fresh-account credits (previously non-existent) are routed through
/// [`State::commit`] so revm's `apply_account_state` produces a transition
/// with the `Created` flag — required for the bundle's plain-state write
/// to actually insert the new account. Modifications of already-existing
/// accounts use a manually-built [`TransitionAccount`], preserving the
/// diff the overlay captured even though `apply_balance_op` pre-mutated
/// the cache for within-tx visibility.
#[derive(Default, Debug)]
pub struct StateOverlay {
    entries: HashMap<Address, Entry>,
}

impl StateOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset_tx(&mut self) {
        self.entries.clear();
    }

    pub fn record_pre_touch<DB: Database>(&mut self, state: &mut State<DB>, addr: Address) {
        if self.entries.contains_key(&addr) {
            return;
        }
        let _ = state.load_cache_account(addr);
        let cache_entry = state.cache.accounts.get(&addr);
        let previous_info = cache_entry
            .and_then(|c| c.account.as_ref())
            .map(|a| a.info.clone());
        let previous_status = cache_entry
            .map(|c| c.status)
            .unwrap_or(CacheAccountStatus::LoadedNotExisting);
        self.entries.insert(
            addr,
            Entry {
                previous_info,
                previous_status,
            },
        );
    }

    pub fn drain_and_apply<DB: Database>(&mut self, state: &mut State<DB>) {
        if self.entries.is_empty() {
            return;
        }
        let entries: Vec<(Address, Entry)> = self.entries.drain().collect();

        let mut fresh_creates = alloy_primitives::map::HashMap::default();
        let mut existing_transitions: Vec<(Address, TransitionAccount)> = Vec::new();

        for (addr, entry) in entries {
            let current_info = state
                .cache
                .accounts
                .get(&addr)
                .and_then(|c| c.account.as_ref())
                .map(|a| a.info.clone());

            if current_info == entry.previous_info {
                continue;
            }

            let pre_empty = entry
                .previous_info
                .as_ref()
                .map(|i| i.is_empty())
                .unwrap_or(true);
            let cur_empty = current_info.as_ref().map(|i| i.is_empty()).unwrap_or(true);
            if pre_empty && cur_empty {
                continue;
            }

            let was_non_existing = entry.previous_info.is_none()
                || matches!(
                    entry.previous_status,
                    CacheAccountStatus::LoadedNotExisting | CacheAccountStatus::LoadedEmptyEIP161
                );

            if was_non_existing && !cur_empty {
                let mut account = Account {
                    info: current_info.unwrap_or_default(),
                    storage: Default::default(),
                    ..Default::default()
                };
                account.mark_touch();
                account.mark_created();
                fresh_creates.insert(addr, account);
                continue;
            }

            let new_status = match (pre_empty, cur_empty) {
                (true, false) => match entry.previous_status {
                    CacheAccountStatus::Destroyed
                    | CacheAccountStatus::DestroyedAgain
                    | CacheAccountStatus::DestroyedChanged => CacheAccountStatus::DestroyedChanged,
                    _ => CacheAccountStatus::InMemoryChange,
                },
                (false, true) => match entry.previous_status {
                    CacheAccountStatus::LoadedNotExisting => continue,
                    CacheAccountStatus::DestroyedAgain | CacheAccountStatus::DestroyedChanged => {
                        CacheAccountStatus::DestroyedAgain
                    }
                    _ => CacheAccountStatus::Destroyed,
                },
                (false, false) => match entry.previous_status {
                    CacheAccountStatus::Loaded => CacheAccountStatus::Changed,
                    CacheAccountStatus::LoadedNotExisting
                    | CacheAccountStatus::LoadedEmptyEIP161 => CacheAccountStatus::InMemoryChange,
                    CacheAccountStatus::DestroyedAgain
                    | CacheAccountStatus::Destroyed
                    | CacheAccountStatus::DestroyedChanged => CacheAccountStatus::DestroyedChanged,
                    other => other,
                },
                (true, true) => unreachable!(),
            };

            let goes_destroyed = matches!(
                new_status,
                CacheAccountStatus::Destroyed | CacheAccountStatus::DestroyedAgain
            );
            let transition_info = if goes_destroyed {
                None
            } else {
                current_info.clone()
            };
            let storage_was_destroyed = goes_destroyed && !pre_empty;

            if let Some(cached) = state.cache.accounts.get_mut(&addr) {
                cached.status = new_status;
                if goes_destroyed {
                    cached.account = None;
                }
            }

            existing_transitions.push((
                addr,
                TransitionAccount {
                    info: transition_info,
                    status: new_status,
                    previous_info: entry.previous_info,
                    previous_status: entry.previous_status,
                    storage: Default::default(),
                    storage_was_destroyed,
                },
            ));
        }

        if !fresh_creates.is_empty() {
            <State<DB> as DatabaseCommit>::commit(state, fresh_creates);
        }
        if !existing_transitions.is_empty() {
            state.apply_transition(existing_transitions);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, U256};
    use revm::database::{EmptyDB, State};
    use revm_database::states::{
        bundle_state::BundleRetention, cache_account::CacheAccount, plain_account::PlainAccount,
    };

    fn make_state() -> State<EmptyDB> {
        State::builder()
            .with_database(EmptyDB::default())
            .with_bundle_update()
            .build()
    }

    #[test]
    fn fresh_account_credit_lands_in_persisted_bundle() {
        let mut state = make_state();
        let mut overlay = StateOverlay::new();
        let recipient = address!("000000000000000000000000000000000000beef");
        let credit = U256::from(0x6a94d74f430000u64);

        overlay.record_pre_touch(&mut state, recipient);
        let entry = state.cache.accounts.get_mut(&recipient).unwrap();
        entry.account = Some(PlainAccount {
            info: AccountInfo {
                balance: credit,
                ..Default::default()
            },
            storage: Default::default(),
        });

        overlay.drain_and_apply(&mut state);
        state.merge_transitions(BundleRetention::Reverts);

        let bundled = state
            .bundle_state
            .state
            .get(&recipient)
            .and_then(|a| a.info.as_ref())
            .map(|i| i.balance);
        assert_eq!(bundled, Some(credit));
    }

    #[test]
    fn existing_account_change_lands_in_persisted_bundle() {
        let mut state = make_state();
        let mut overlay = StateOverlay::new();
        let acct = address!("000000000000000000000000000000000000c0de");

        state.cache.accounts.insert(
            acct,
            CacheAccount {
                account: Some(PlainAccount {
                    info: AccountInfo {
                        balance: U256::from(10u64),
                        ..Default::default()
                    },
                    storage: Default::default(),
                }),
                status: CacheAccountStatus::Loaded,
            },
        );

        overlay.record_pre_touch(&mut state, acct);
        if let Some(p) = state
            .cache
            .accounts
            .get_mut(&acct)
            .and_then(|c| c.account.as_mut())
        {
            p.info.balance = U256::from(42u64);
        }

        overlay.drain_and_apply(&mut state);
        state.merge_transitions(BundleRetention::Reverts);

        let bundled = state
            .bundle_state
            .state
            .get(&acct)
            .and_then(|a| a.info.as_ref())
            .map(|i| i.balance);
        assert_eq!(bundled, Some(U256::from(42u64)));
    }

    #[test]
    fn created_then_emptied_in_same_tx_produces_no_bundle_entry() {
        let mut state = make_state();
        let mut overlay = StateOverlay::new();
        let transient = address!("0000000000000000000000000000000000007a17");

        overlay.record_pre_touch(&mut state, transient);
        let entry = state.cache.accounts.get_mut(&transient).unwrap();
        entry.account = Some(PlainAccount {
            info: AccountInfo {
                balance: U256::ZERO,
                ..Default::default()
            },
            storage: Default::default(),
        });

        overlay.drain_and_apply(&mut state);
        state.merge_transitions(BundleRetention::Reverts);

        assert!(!state.bundle_state.state.contains_key(&transient));
    }
}
