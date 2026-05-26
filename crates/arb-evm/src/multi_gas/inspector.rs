//! revm inspector that accumulates per-transaction multi-gas by dimension.
//!
//! Each opcode's observed gas delta (`step` to `step_end`) is split with
//! [`classify`]. Frame-spawning opcodes are special: their delta includes the
//! gas forwarded to the child frame, whose own opcodes are observed separately,
//! so the forwarded portion is removed via the `call`/`create` hooks. Contract
//! code-deposit gas is charged at frame return rather than at an opcode, so it
//! is added in `create_end`.

use alloy_evm::{eth::EthEvmContext, Database};
use alloy_primitives::{Address, B256, U256};
use arb_primitives::multigas::MultiGas;
use parking_lot::Mutex;
use revm::{
    bytecode::opcode,
    interpreter::{
        interpreter::EthInterpreter,
        interpreter_types::{InputsTr, Jumps},
        CallInputs, CallOutcome, CallValue, CreateInputs, CreateOutcome, Interpreter,
    },
    Inspector,
};
use std::sync::Arc;

use crate::multi_gas::classify::{classify, OpKind};

/// Shared slot a [`MultiGasInspector`] writes each transaction's multi-gas to,
/// read by the block executor after execution.
pub type MultiGasSink = Arc<Mutex<Option<MultiGas>>>;

const WARM: u64 = 100; // WarmStorageReadCostEIP2929
const CALL_STIPEND: u64 = 2_300; // CallStipend
const CREATE_DATA_GAS: u64 = 200; // CreateDataGas (code storage, per byte)

/// Accumulates per-transaction multi-gas across every executed frame.
#[derive(Debug, Default)]
pub struct MultiGasInspector {
    prev_gas: u64,
    pending: Pending,
    accumulated: MultiGas,
    sink: Option<MultiGasSink>,
}

#[derive(Debug, Default)]
enum Pending {
    #[default]
    None,
    /// SLOAD (`account = false`) or BALANCE/EXTCODESIZE/EXTCODEHASH
    /// (`account = true`): the cold surcharge is the whole dynamic cost, so
    /// cold is read off the step delta.
    DeltaCold {
        account: bool,
    },
    Log {
        topics: u8,
        data_len: u64,
    },
    ExtCodeCopy {
        cold: bool,
        words: u64,
    },
    SelfDestruct {
        cold: bool,
        new_account: bool,
    },
    /// SSTORE needs the committed value, only reliable after the write; the new
    /// value, warmth, and pre-write current value are captured at step time.
    SStore {
        cold: bool,
        contract: Address,
        key: U256,
        new: U256,
        current: Option<U256>,
    },
    /// CALL/CREATE family. `delta` is filled at `step_end`; the own cost is
    /// resolved in the matching `call`/`create` hook.
    Frame {
        cold: bool,
        is_create: bool,
        is_plain_call: bool,
        delta: u64,
    },
    Other,
}

impl MultiGasInspector {
    /// Creates an inspector that publishes each transaction's multi-gas to a
    /// shared sink when the top-level frame returns.
    pub fn with_sink(sink: MultiGasSink) -> Self {
        Self {
            sink: Some(sink),
            ..Default::default()
        }
    }

    /// Returns the accumulated multi-gas and resets for the next transaction.
    pub fn take_multi_gas(&mut self) -> MultiGas {
        self.flush_dangling_frame();
        self.pending = Pending::None;
        self.prev_gas = 0;
        core::mem::replace(&mut self.accumulated, MultiGas::zero())
    }

    /// Publishes the accumulated multi-gas to the sink and resets, called when
    /// the outermost frame returns (depth zero).
    fn publish(&mut self) {
        if self.sink.is_some() {
            let gas = self.take_multi_gas();
            if let Some(sink) = &self.sink {
                *sink.lock() = Some(gas);
            }
        }
    }

    fn add(&mut self, gas: MultiGas) {
        self.accumulated = self.accumulated.saturating_add(gas);
    }

    /// A frame opcode that halted before forwarding (e.g. out of gas) never
    /// reaches its `call`/`create` hook; classify it from the full delta.
    fn flush_dangling_frame(&mut self) {
        if let Pending::Frame {
            cold,
            is_create,
            delta,
            ..
        } = self.pending
        {
            let gas = if is_create {
                MultiGas::computation_gas(delta)
            } else {
                classify(
                    OpKind::Call {
                        cold,
                        new_account: false,
                    },
                    delta,
                )
            };
            self.add(gas);
            self.pending = Pending::None;
        }
    }
}

impl<DB: Database> Inspector<EthEvmContext<DB>, EthInterpreter> for MultiGasInspector {
    fn step(&mut self, interp: &mut Interpreter<EthInterpreter>, ctx: &mut EthEvmContext<DB>) {
        self.flush_dangling_frame();
        self.prev_gas = interp.gas.remaining();
        let op = interp.bytecode.opcode();
        self.pending = match op {
            opcode::SLOAD => Pending::DeltaCold { account: false },
            opcode::BALANCE | opcode::EXTCODESIZE | opcode::EXTCODEHASH => {
                Pending::DeltaCold { account: true }
            }
            opcode::EXTCODECOPY => Pending::ExtCodeCopy {
                cold: address_cold(ctx, addr_arg(interp, 0)),
                words: word_count(peek(interp, 3)),
            },
            opcode::LOG0..=opcode::LOG4 => Pending::Log {
                topics: op - opcode::LOG0,
                data_len: to_u64(peek(interp, 1)),
            },
            opcode::SSTORE => {
                let contract = interp.input.target_address();
                let key = peek(interp, 0);
                Pending::SStore {
                    cold: slot_cold(ctx, contract, key),
                    contract,
                    key,
                    new: peek(interp, 1),
                    current: slot_values(ctx, contract, key).map(|(_, present)| present),
                }
            }
            opcode::SELFDESTRUCT => {
                let beneficiary = addr_arg(interp, 0);
                let contract = interp.input.target_address();
                Pending::SelfDestruct {
                    cold: address_cold(ctx, beneficiary),
                    new_account: account_empty(ctx, beneficiary)
                        && !account_balance_zero(ctx, contract),
                }
            }
            opcode::CALL | opcode::CALLCODE => Pending::Frame {
                cold: address_cold(ctx, addr_arg(interp, 1)),
                is_create: false,
                is_plain_call: op == opcode::CALL,
                delta: 0,
            },
            opcode::DELEGATECALL | opcode::STATICCALL => Pending::Frame {
                cold: address_cold(ctx, addr_arg(interp, 1)),
                is_create: false,
                is_plain_call: false,
                delta: 0,
            },
            opcode::CREATE | opcode::CREATE2 => Pending::Frame {
                cold: false,
                is_create: true,
                is_plain_call: false,
                delta: 0,
            },
            _ => Pending::Other,
        };
    }

    fn step_end(&mut self, interp: &mut Interpreter<EthInterpreter>, ctx: &mut EthEvmContext<DB>) {
        let delta = self.prev_gas.saturating_sub(interp.gas.remaining());
        let pending = core::mem::replace(&mut self.pending, Pending::None);
        let gas = match pending {
            Pending::Frame {
                cold,
                is_create,
                is_plain_call,
                ..
            } => {
                self.pending = Pending::Frame {
                    cold,
                    is_create,
                    is_plain_call,
                    delta,
                };
                return;
            }
            Pending::None => return,
            Pending::DeltaCold { account } => {
                let cold = delta > WARM;
                let kind = if account {
                    OpKind::AccountAccess { cold }
                } else {
                    OpKind::StorageRead { cold }
                };
                classify(kind, delta)
            }
            Pending::Log { topics, data_len } => classify(OpKind::Log { topics, data_len }, delta),
            Pending::ExtCodeCopy { cold, words } => {
                classify(OpKind::ExtCodeCopy { cold, words }, delta)
            }
            Pending::SelfDestruct { cold, new_account } => {
                classify(OpKind::SelfDestruct { cold, new_account }, delta)
            }
            Pending::SStore {
                cold,
                contract,
                key,
                new,
                current,
            } => {
                let original = slot_values(ctx, contract, key)
                    .map(|(original, _)| original)
                    .unwrap_or(U256::ZERO);
                let present = current.unwrap_or(original);
                classify(
                    OpKind::StorageWrite {
                        cold,
                        original,
                        present,
                        new,
                    },
                    delta,
                )
            }
            Pending::Other => classify(OpKind::Other, delta),
        };
        self.add(gas);
    }

    fn call(
        &mut self,
        ctx: &mut EthEvmContext<DB>,
        inputs: &mut CallInputs,
    ) -> Option<CallOutcome> {
        if let Pending::Frame {
            cold,
            is_create: false,
            is_plain_call,
            delta,
        } = self.pending
        {
            self.pending = Pending::None;
            let value_transfer = matches!(inputs.value, CallValue::Transfer(v) if !v.is_zero());
            let own = call_own_cost(delta, inputs.gas_limit, value_transfer);
            let new_account =
                is_plain_call && value_transfer && account_empty(ctx, inputs.target_address);
            self.add(classify(OpKind::Call { cold, new_account }, own));
        }
        None
    }

    fn call_end(
        &mut self,
        ctx: &mut EthEvmContext<DB>,
        _inputs: &CallInputs,
        _outcome: &mut CallOutcome,
    ) {
        if ctx.journaled_state.inner.depth == 0 {
            self.publish();
        }
    }

    fn create(
        &mut self,
        _ctx: &mut EthEvmContext<DB>,
        inputs: &mut CreateInputs,
    ) -> Option<CreateOutcome> {
        if let Pending::Frame {
            is_create: true,
            delta,
            ..
        } = self.pending
        {
            self.pending = Pending::None;
            let own = delta.saturating_sub(inputs.gas_limit());
            self.add(MultiGas::computation_gas(own));
        }
        None
    }

    fn create_end(
        &mut self,
        ctx: &mut EthEvmContext<DB>,
        _inputs: &CreateInputs,
        outcome: &mut CreateOutcome,
    ) {
        if outcome.result.is_ok() {
            let deposit = (outcome.result.output.len() as u64).saturating_mul(CREATE_DATA_GAS);
            self.add(MultiGas::storage_growth_gas(deposit));
        }
        if ctx.journaled_state.inner.depth == 0 {
            self.publish();
        }
    }
}

/// Own cost of a call opcode: the step delta minus the gas forwarded to the
/// child. The stipend is added to the child's limit without being charged to
/// the caller, so it is excluded from the forwarded amount.
fn call_own_cost(delta: u64, child_gas_limit: u64, value_transfer: bool) -> u64 {
    let stipend = if value_transfer { CALL_STIPEND } else { 0 };
    delta.saturating_sub(child_gas_limit.saturating_sub(stipend))
}

fn peek(interp: &Interpreter<EthInterpreter>, from_top: usize) -> U256 {
    let data = interp.stack.data();
    data.len()
        .checked_sub(from_top + 1)
        .map(|i| data[i])
        .unwrap_or(U256::ZERO)
}

fn addr_arg(interp: &Interpreter<EthInterpreter>, from_top: usize) -> Address {
    Address::from_word(B256::from(peek(interp, from_top).to_be_bytes::<32>()))
}

fn word_count(len: U256) -> u64 {
    to_u64(len).div_ceil(32)
}

fn to_u64(v: U256) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

fn address_cold<DB: Database>(ctx: &EthEvmContext<DB>, addr: Address) -> bool {
    let journal = &ctx.journaled_state.inner;
    if journal.warm_addresses.is_warm(&addr) {
        return false;
    }
    match journal.state.get(&addr) {
        Some(account) => account.is_cold_transaction_id(journal.transaction_id),
        None => true,
    }
}

fn slot_cold<DB: Database>(ctx: &EthEvmContext<DB>, addr: Address, key: U256) -> bool {
    let journal = &ctx.journaled_state.inner;
    match journal.state.get(&addr).and_then(|a| a.storage.get(&key)) {
        Some(slot) => slot.is_cold_transaction_id(journal.transaction_id),
        None => true,
    }
}

fn slot_values<DB: Database>(
    ctx: &EthEvmContext<DB>,
    addr: Address,
    key: U256,
) -> Option<(U256, U256)> {
    let slot = ctx
        .journaled_state
        .inner
        .state
        .get(&addr)?
        .storage
        .get(&key)?;
    Some((slot.original_value, slot.present_value))
}

fn account_empty<DB: Database>(ctx: &EthEvmContext<DB>, addr: Address) -> bool {
    match ctx.journaled_state.inner.state.get(&addr) {
        Some(account) => {
            account.info.balance.is_zero()
                && account.info.nonce == 0
                && account.info.code_hash == revm::primitives::KECCAK_EMPTY
        }
        None => true,
    }
}

fn account_balance_zero<DB: Database>(ctx: &EthEvmContext<DB>, addr: Address) -> bool {
    match ctx.journaled_state.inner.state.get(&addr) {
        Some(account) => account.info.balance.is_zero(),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_own_cost_strips_forwarded_gas() {
        // delta = own (2600 cold + 9000 value + 100 warm) + forwarded 50000.
        let delta = 2_600 + 9_000 + 100 + 50_000;
        assert_eq!(call_own_cost(delta, 50_000, false), 2_600 + 9_000 + 100);
    }

    #[test]
    fn call_own_cost_excludes_stipend_from_forwarded() {
        // A value call forwards `gas_limit` to the child but the 2300 stipend is
        // added on top without being charged to the caller.
        let own = 2_700;
        let forwarded_charged = 40_000;
        let child_gas_limit = forwarded_charged + CALL_STIPEND;
        let delta = own + forwarded_charged;
        assert_eq!(call_own_cost(delta, child_gas_limit, true), own);
    }

    #[test]
    fn call_own_cost_saturates() {
        assert_eq!(call_own_cost(100, 50_000, false), 0);
    }

    #[test]
    fn word_count_rounds_up() {
        assert_eq!(word_count(U256::ZERO), 0);
        assert_eq!(word_count(U256::from(1)), 1);
        assert_eq!(word_count(U256::from(32)), 1);
        assert_eq!(word_count(U256::from(33)), 2);
    }
}
