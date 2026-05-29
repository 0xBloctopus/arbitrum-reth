//! Under v60 the multi-gas inspector drives the per-transaction refund, so any
//! mis-attribution becomes a state difference. The inspector path and the plain
//! path must produce identical post-state across multi-transaction blocks —
//! including blocks whose transactions perform internal creates, whose frames
//! must not bleed multi-gas into the following transaction. The header base fee
//! is set above the `base_fee_wei` floor so the refund is active and exercised.

#[cfg(target_arch = "x86_64")]
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn __rust_probestack() {}

use alloy_consensus::{
    crypto::secp256k1::sign_message, transaction::Recovered, EthereumTxEnvelope,
    SignableTransaction, TxLegacy,
};
use alloy_evm::{
    block::{BlockExecutor, BlockExecutorFactory},
    eth::EthBlockExecutionCtx,
    EvmFactory,
};
use alloy_primitives::{address, keccak256, Address, TxKind, B256, U256};
use arb_evm::{
    config::ArbEvmConfig,
    multi_gas::{MultiGasInspector, MultiGasSink},
};
use arb_primitives::ArbTransactionSigned;
use arb_test_utils::{ArbosHarness, EmptyDb};
use reth_chainspec::ChainSpec;
use reth_evm::{ConfigureEvm, EvmEnv};
use revm::{
    context::{BlockEnv, CfgEnv},
    database::{states::account_status::AccountStatus, PlainAccount, State},
    primitives::hardfork::SpecId,
    state::{AccountInfo, Bytecode},
};
use std::sync::Arc;

const SECRET_KEY: [u8; 32] = [
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10,
    0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20,
];
const CHAIN_ID: u64 = 421614;
const ARBOS_VERSION: u64 = 60;
// Header base fee, above the harness's initial `base_fee_wei` floor (0.1 Gwei)
// so the v60 multi-gas refund is active for every transaction.
const HEADER_BASE_FEE: u64 = 150_000_000;
const STORE: Address = address!("00000000000000000000000000000000c0c0c0c0");
const FACTORY: Address = address!("00000000000000000000000000000000fac70217");

fn sender() -> Address {
    use k256::ecdsa::SigningKey;
    let sk = SigningKey::from_slice(&SECRET_KEY).unwrap();
    let encoded = sk.verifying_key().to_encoded_point(false);
    Address::from_slice(&keccak256(&encoded.as_bytes()[1..])[12..])
}

fn set_account(state: &mut State<EmptyDb>, addr: Address, info: AccountInfo) {
    let _ = state.load_cache_account(addr);
    if let Some(cached) = state.cache.accounts.get_mut(&addr) {
        cached.account = Some(PlainAccount {
            info,
            storage: Default::default(),
        });
        cached.status = AccountStatus::InMemoryChange;
    }
}

/// Stores 1 at slot 0: PUSH1 1; PUSH1 0; SSTORE; STOP.
fn store_code() -> Vec<u8> {
    vec![0x60, 0x01, 0x60, 0x00, 0x55, 0x00]
}

/// Performs one internal CREATE of a 32-byte-runtime child, then STOP. The
/// create runs as a nested frame — the case whose gas must stay attributed to
/// this transaction.
fn factory_code() -> Vec<u8> {
    vec![
        0x64, 0x60, 0x20, 0x60, 0x00, 0xf3, // PUSH5 <init: PUSH1 0x20 PUSH1 0 RETURN>
        0x60, 0x00, // PUSH1 0
        0x52, // MSTORE (init code at mem[27..32])
        0x60, 0x05, // PUSH1 5   (init size)
        0x60, 0x1b, // PUSH1 27  (init offset)
        0x60, 0x00, // PUSH1 0   (value)
        0xf0, // CREATE
        0x50, // POP
        0x00, // STOP
    ]
}

fn call_tx(to: Address, nonce: u64) -> Recovered<ArbTransactionSigned> {
    let tx = TxLegacy {
        chain_id: Some(CHAIN_ID),
        nonce,
        gas_price: 1_000_000_000,
        gas_limit: 500_000,
        to: TxKind::Call(to),
        value: U256::ZERO,
        input: Default::default(),
    };
    let sig = sign_message(B256::from(SECRET_KEY), tx.signature_hash()).unwrap();
    let envelope = EthereumTxEnvelope::Legacy(tx.into_signed(sig));
    Recovered::new_unchecked(ArbTransactionSigned::from_envelope(envelope), sender())
}

fn block_env() -> EvmEnv<SpecId> {
    let mut env = EvmEnv {
        cfg_env: CfgEnv::default(),
        block_env: BlockEnv::default(),
    };
    env.cfg_env.chain_id = CHAIN_ID;
    env.cfg_env.disable_base_fee = true;
    env.block_env.timestamp = U256::from(1_700_000_000u64);
    env.block_env.basefee = HEADER_BASE_FEE;
    env.block_env.gas_limit = 30_000_000;
    env.block_env.number = U256::from(1u64);
    env
}

fn exec_ctx() -> EthBlockExecutionCtx<'static> {
    EthBlockExecutionCtx {
        tx_count_hint: Some(4),
        parent_hash: B256::ZERO,
        parent_beacon_block_root: None,
        ommers: &[],
        withdrawals: None,
        extra_data: vec![0u8; 32].into(),
    }
}

/// (sender balance, STORE slot 0, FACTORY nonce).
type PostState = (U256, U256, u64);

fn read_post_state(h: &mut ArbosHarness) -> PostState {
    let bal = |h: &mut ArbosHarness, a: Address| {
        h.state()
            .cache
            .accounts
            .get(&a)
            .and_then(|c| c.account.as_ref())
            .map(|x| x.info.balance)
            .unwrap_or(U256::ZERO)
    };
    let slot0 = h
        .state()
        .cache
        .accounts
        .get(&STORE)
        .and_then(|a| a.account.as_ref())
        .and_then(|a| a.storage.get(&U256::ZERO).copied())
        .unwrap_or(U256::ZERO);
    let factory_nonce = h
        .state()
        .cache
        .accounts
        .get(&FACTORY)
        .and_then(|a| a.account.as_ref())
        .map(|a| a.info.nonce)
        .unwrap_or(0);
    (bal(h, sender()), slot0, factory_nonce)
}

fn harness() -> ArbosHarness {
    let mut h = ArbosHarness::new()
        .with_arbos_version(ARBOS_VERSION)
        .with_chain_id(CHAIN_ID)
        .initialize();
    set_account(
        h.state(),
        sender(),
        AccountInfo {
            balance: U256::from(10u64).pow(U256::from(19u64)),
            ..Default::default()
        },
    );
    for (addr, bytes) in [(STORE, store_code()), (FACTORY, factory_code())] {
        let code = Bytecode::new_raw(bytes.into());
        set_account(
            h.state(),
            addr,
            AccountInfo {
                nonce: 1,
                code_hash: code.hash_slow(),
                code: Some(code),
                ..Default::default()
            },
        );
    }
    h
}

/// Runs `targets` as a sequence of single-call transactions from one sender,
/// without the multi-gas inspector.
fn run_plain(targets: &[Address]) -> PostState {
    let mut h = harness();
    let cfg = ArbEvmConfig::new(Arc::new(ChainSpec::default()));
    let factory = cfg.block_executor_factory();
    let evm = factory.evm_factory().create_evm(h.state(), block_env());
    let mut executor = factory.create_arb_executor(evm, exec_ctx(), CHAIN_ID);
    executor.arb_ctx.basefee = U256::from(HEADER_BASE_FEE);
    executor.apply_pre_execution_changes().unwrap();
    for (nonce, target) in targets.iter().enumerate() {
        let result = executor
            .execute_transaction_without_commit(call_tx(*target, nonce as u64))
            .unwrap();
        executor.commit_transaction(result).unwrap();
    }
    executor.finish().unwrap();
    read_post_state(&mut h)
}

/// Same as [`run_plain`] but with the multi-gas inspector installed, switching
/// block execution to revm's inspect path.
fn run_inspected(targets: &[Address]) -> PostState {
    let mut h = harness();
    let cfg = ArbEvmConfig::new(Arc::new(ChainSpec::default()));
    let factory = cfg.block_executor_factory();
    let sink = MultiGasSink::default();
    let evm = factory.evm_factory().create_evm_with_inspector(
        h.state(),
        block_env(),
        MultiGasInspector::with_sink(sink.clone()),
    );
    let mut executor = factory.create_arb_executor(evm, exec_ctx(), CHAIN_ID);
    executor.set_multi_gas_sink(sink);
    executor.arb_ctx.basefee = U256::from(HEADER_BASE_FEE);
    executor.apply_pre_execution_changes().unwrap();
    for (nonce, target) in targets.iter().enumerate() {
        let result = executor
            .execute_transaction_without_commit(call_tx(*target, nonce as u64))
            .unwrap();
        executor.commit_transaction(result).unwrap();
    }
    executor.finish().unwrap();
    read_post_state(&mut h)
}

/// Each ordering: the inspector path must equal the plain path, and the
/// transactions must actually have executed (store written, create performed).
fn assert_equivalent(targets: &[Address], creates: u64) {
    let plain = run_plain(targets);
    let inspected = run_inspected(targets);
    assert_eq!(
        plain, inspected,
        "inspector path diverged from plain path for {targets:?}",
    );
    if targets.contains(&STORE) {
        assert_eq!(plain.1, U256::from(1u64), "STORE must have executed");
    }
    assert_eq!(
        plain.2,
        1 + creates,
        "FACTORY nonce must reflect {creates} create(s)",
    );
    assert!(
        plain.0 < U256::from(10u64).pow(U256::from(19u64)),
        "sender must have been charged",
    );
}

#[test]
fn create_then_call_is_consensus_equivalent() {
    assert_equivalent(&[FACTORY, STORE], 1);
}

#[test]
fn call_then_create_is_consensus_equivalent() {
    assert_equivalent(&[STORE, FACTORY], 1);
}

#[test]
fn repeated_creates_then_call_is_consensus_equivalent() {
    assert_equivalent(&[FACTORY, FACTORY, STORE], 2);
}

#[test]
fn plain_multi_call_is_consensus_equivalent() {
    assert_equivalent(&[STORE, STORE], 0);
}
