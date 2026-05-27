//! Per-host-call multi-gas dimension assignment for Stylus execution: each host
//! op contributes the same per-dimension gas as the equivalent EVM opcode.
//! Framework gas (host ink, the EVM-API surcharge, the per-call storage cache)
//! is left undimensioned here and falls into the `WasmComputation` residual.

use alloy_primitives::U256;
use arb_primitives::multigas::{MultiGas, ResourceKind};

const WARM: u64 = 100; // WarmStorageReadCostEIP2929
const COLD_SLOAD: u64 = 2_100; // ColdSloadCostEIP2929
const COLD_ACCOUNT: u64 = 2_600; // ColdAccountAccessCostEIP2929
const SSTORE_SET: u64 = 20_000; // SstoreSetGasEIP2200
const SSTORE_RESET: u64 = 5_000; // SstoreResetGasEIP2200
const LOG_TOPIC_HISTORY: u64 = 256; // LogTopicHistoryGas (LogDataGas * 32)
const LOG_DATA: u64 = 8; // LogDataGas

/// Dimension split of a Stylus `storage_load`, matching SLOAD.
pub fn state_load(is_cold: bool) -> MultiGas {
    if is_cold {
        MultiGas::from_pairs(&[
            (ResourceKind::StorageAccessRead, COLD_SLOAD - WARM),
            (ResourceKind::Computation, WARM),
        ])
    } else {
        MultiGas::computation_gas(WARM)
    }
}

/// Dimension split of a Stylus `storage_store`, matching SSTORE. The reentrancy
/// sentry check is the caller's responsibility.
pub fn state_store(is_cold: bool, original: U256, present: U256, new: U256) -> MultiGas {
    use ResourceKind::*;
    let mut pairs = Vec::with_capacity(2);
    if is_cold {
        pairs.push((StorageAccessRead, COLD_SLOAD));
    }
    if present == new {
        pairs.push((Computation, WARM));
    } else if original == present {
        if original.is_zero() {
            pairs.push((StorageGrowth, SSTORE_SET));
        } else {
            pairs.push((StorageAccessWrite, SSTORE_RESET - COLD_SLOAD));
        }
    } else {
        pairs.push((Computation, WARM));
    }
    MultiGas::from_pairs(&pairs)
}

/// Dimension split of touching an account in Stylus, matching EIP-2929 account
/// access. `ext_code_cost` is the extra read charged when the code is loaded.
pub fn account_touch(is_cold: bool, ext_code_cost: u64) -> MultiGas {
    let read = if is_cold {
        ext_code_cost + (COLD_ACCOUNT - WARM)
    } else {
        ext_code_cost
    };
    MultiGas::from_pairs(&[
        (ResourceKind::StorageAccessRead, read),
        (ResourceKind::Computation, WARM),
    ])
}

/// History-growth dimension of a Stylus `emit_log`.
pub fn log(num_topics: u64, data_bytes: u64) -> MultiGas {
    MultiGas::history_growth_gas(LOG_TOPIC_HISTORY * num_topics + LOG_DATA * data_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dims(mg: &MultiGas) -> (u64, u64, u64, u64, u64) {
        (
            mg.get(ResourceKind::Computation),
            mg.get(ResourceKind::StorageAccessRead),
            mg.get(ResourceKind::StorageAccessWrite),
            mg.get(ResourceKind::StorageGrowth),
            mg.get(ResourceKind::HistoryGrowth),
        )
    }

    #[test]
    fn load_cold_warm() {
        assert_eq!(dims(&state_load(true)), (100, 2_000, 0, 0, 0));
        assert_eq!(state_load(true).single_gas(), COLD_SLOAD);
        assert_eq!(dims(&state_load(false)), (100, 0, 0, 0, 0));
    }

    #[test]
    fn store_cold_create() {
        let mg = state_store(true, U256::ZERO, U256::ZERO, U256::from(1));
        assert_eq!(dims(&mg), (0, 2_100, 0, 20_000, 0));
        assert_eq!(mg.single_gas(), COLD_SLOAD + SSTORE_SET);
    }

    #[test]
    fn store_warm_write_and_noop_and_dirty() {
        let write = state_store(false, U256::from(7), U256::from(7), U256::from(9));
        assert_eq!(dims(&write), (0, 0, 2_900, 0, 0));
        let noop = state_store(false, U256::from(5), U256::from(5), U256::from(5));
        assert_eq!(dims(&noop), (100, 0, 0, 0, 0));
        let dirty = state_store(false, U256::from(1), U256::from(2), U256::from(3));
        assert_eq!(dims(&dirty), (100, 0, 0, 0, 0));
    }

    #[test]
    fn account_cold_warm_and_code() {
        assert_eq!(dims(&account_touch(true, 0)), (100, 2_500, 0, 0, 0));
        assert_eq!(dims(&account_touch(false, 0)), (100, 0, 0, 0, 0));
        assert_eq!(dims(&account_touch(true, 700)), (100, 3_200, 0, 0, 0));
        assert_eq!(account_touch(true, 700).single_gas(), 700 + COLD_ACCOUNT);
    }

    #[test]
    fn log_history_growth() {
        let mg = log(2, 10);
        assert_eq!(dims(&mg), (0, 0, 0, 0, 256 * 2 + 8 * 10));
    }
}
