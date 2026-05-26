//! Differential test for multi-gas pricing under ACTIVE constraints.
//!
//! Boots a fresh `NitroDocker` + `ArbrethProcess` pair (NOT shared, because the
//! chain owner is test-specific so an owner can configure constraints). The
//! owner installs a constraint weighting `StorageAccessWrite`, then drives
//! `SSTORE`-reset transactions. With per-resource attribution the write gas
//! grows that constraint's backlog and the multi-gas base fee escalates
//! identically on both nodes; lumping execution gas into computation would
//! leave the write backlog flat and diverge.
//!
//! Run:
//!   ARB_SPEC_BINARY=$(pwd)/target/release/arb-reth \
//!     cargo test -p arb-fuzz --test mgas_constraints_dual --release \
//!     -- --ignored --nocapture

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Each test spawns its own Nitro + arbreth pair, so they must not run
/// concurrently (Docker / port / process contention). Serialize them.
static SERIAL: Mutex<()> = Mutex::new(());

use alloy_primitives::{address, Address, Bytes, B256, U256};
use arb_fuzz::{arbitrary_impls::interop::wrap_init_code, scaffolding::selector4};
use arb_test_harness::{
    dual_exec::{DiffReport, DualExec},
    genesis::GenesisBuilder,
    messaging::{
        signed_tx::{derive_address, L2TxKind, SignedL2TxBuilder},
        DepositBuilder, MessageBuilder,
    },
    mock_l1::MockL1,
    node::{
        arbreth::ArbrethProcess, nitro_docker::NitroDocker, BlockId, ExecutionNode, NodeStartCtx,
        TxRequest,
    },
    scenario::{Scenario, ScenarioSetup, ScenarioStep},
};

const L2_CHAIN_ID: u64 = 412_348;
const L1_CHAIN_ID: u64 = 11_155_111;
const FUZZ_L1_BASE_FEE: u64 = 30_000_000_000;
const ARBOS_VERSION: u64 = 60;

const SEQUENCER_ALIAS: Address = address!("a4b000000000000000000073657175656e636572");
const ARBOWNER: Address = Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x70]);
const ARBGASINFO: Address =
    Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x6c]);
const FUNDER: Address = Address::new([0xa1; 20]);

/// StorageAccessWrite resource kind (`ResourceKind` discriminant).
const KIND_STORAGE_WRITE: u8 = 4;

fn owner_key() -> B256 {
    B256::repeat_byte(0x42)
}

fn word(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

/// Hand-encoded `setMultiGasPricingConstraints` with a single constraint and a
/// single resource weight, matching the `(((uint8,uint64)[],uint32,uint64,uint64)[])`
/// layout the precompile decodes.
fn set_constraint_calldata(window: u32, target: u64, backlog: u64, resource: u8, weight: u64) -> Vec<u8> {
    let mut d = Vec::with_capacity(4 + 320);
    d.extend_from_slice(&selector4(
        "setMultiGasPricingConstraints(((uint8,uint64)[],uint32,uint64,uint64)[])",
    ));
    d.extend_from_slice(&word(0x20)); // offset to constraints array
    d.extend_from_slice(&word(1)); // constraints.length
    d.extend_from_slice(&word(0x20)); // element[0] offset (relative to array data)
    d.extend_from_slice(&word(0x80)); // resources offset (relative to struct)
    d.extend_from_slice(&word(window as u64));
    d.extend_from_slice(&word(target));
    d.extend_from_slice(&word(backlog));
    d.extend_from_slice(&word(1)); // resources.length
    d.extend_from_slice(&word(resource as u64));
    d.extend_from_slice(&word(weight));
    d
}

/// Runtime that stores `calldata[0:32]` into slot 0: PUSH1 0, CALLDATALOAD,
/// PUSH1 0, SSTORE, STOP.
const SSTORE_RUNTIME: [u8; 7] = [0x60, 0x00, 0x35, 0x60, 0x00, 0x55, 0x00];

fn create_address(deployer: Address, nonce: u64) -> Address {
    use alloy_primitives::keccak256;
    let mut rlp = Vec::new();
    rlp.push(0xd6);
    rlp.push(0x94);
    rlp.extend_from_slice(deployer.as_slice());
    if nonce == 0 {
        rlp.push(0x80);
    } else if nonce < 0x80 {
        rlp.push(nonce as u8);
    } else {
        let b = nonce.to_be_bytes();
        let start = b.iter().position(|&x| x != 0).unwrap_or(7);
        let trimmed = &b[start..];
        rlp.push(0x80 + trimmed.len() as u8);
        rlp.extend_from_slice(trimmed);
        rlp[0] = 0xd6 + (trimmed.len() as u8);
    }
    Address::from_slice(&keccak256(&rlp)[12..])
}

struct Rig {
    dual: DualExec<NitroDocker, ArbrethProcess>,
}

impl Rig {
    fn spawn(owner: Address) -> Self {
        let mock = MockL1::start(L1_CHAIN_ID).expect("mock l1 start");
        let genesis = GenesisBuilder::new(L2_CHAIN_ID, ARBOS_VERSION)
            .with_initial_chain_owner(owner)
            .build()
            .expect("genesis build");
        let ctx = NodeStartCtx {
            binary: None,
            l2_chain_id: L2_CHAIN_ID,
            l1_chain_id: L1_CHAIN_ID,
            mock_l1_rpc: mock.rpc_url(),
            genesis,
            jwt_hex: String::new(),
            workdir: std::path::PathBuf::new(),
            http_port: 0,
            authrpc_port: 0,
        };
        let nitro = NitroDocker::start(&ctx).expect("nitro docker start");
        let arbreth = ArbrethProcess::start(&ctx).expect("arbreth start");
        std::mem::forget(mock);
        Rig {
            dual: DualExec::new(nitro, arbreth),
        }
    }
}

/// Genesis trie state_root / hashes legitimately differ (Nitro geth-fork zombie
/// nodes); execution fields (gas_used, receipts_root, per-tx receipts) must match.
fn filter_genesis_noise(report: DiffReport) -> DiffReport {
    let DiffReport {
        block_diffs,
        tx_diffs,
        state_diffs,
        log_diffs,
    } = report;
    let block_diffs = block_diffs
        .into_iter()
        .filter(|d| !matches!(d.field.as_str(), "parent_hash" | "block_hash" | "state_root"))
        .collect();
    DiffReport {
        block_diffs,
        tx_diffs,
        state_diffs,
        log_diffs,
    }
}

struct Idx(AtomicU64);
impl Idx {
    fn new() -> Self {
        Self(AtomicU64::new(1))
    }
    fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

fn owner_tx(nonce: u64, to: Option<Address>, data: Vec<u8>, gas: u64) -> SignedL2TxBuilder {
    owner_tx_l1(nonce, to, data, gas, FUZZ_L1_BASE_FEE)
}

fn owner_tx_l1(
    nonce: u64,
    to: Option<Address>,
    data: Vec<u8>,
    gas: u64,
    base_fee_l1: u64,
) -> SignedL2TxBuilder {
    SignedL2TxBuilder {
        chain_id: L2_CHAIN_ID,
        nonce,
        to,
        value: U256::ZERO,
        data: Bytes::from(data),
        gas_limit: gas,
        gas_price: 1_000_000_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        access_list: Vec::new(),
        authorization_list: Vec::new(),
        kind: L2TxKind::Eip1559,
        signing_key: owner_key(),
        l1_block_number: 1,
        timestamp: 1_700_000_000,
        request_id: None,
        sender: SEQUENCER_ALIAS,
        base_fee_l1,
    }
}

fn msg_step(idx: u64, msg: arb_test_harness::messaging::L1Message, dmr: u64) -> ScenarioStep {
    ScenarioStep::Message {
        idx,
        message: msg,
        delayed_messages_read: dmr,
    }
}

#[test]
#[ignore]
fn multi_gas_write_constraint_backlog_matches_nitro() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let owner = derive_address(owner_key());
    let mut rig = Rig::spawn(owner);
    let idx = Idx::new();
    let mut steps = Vec::new();

    // Fund the owner.
    let dep_idx = idx.next();
    let dep = DepositBuilder {
        from: FUNDER,
        to: owner,
        amount: U256::from(10u128).pow(U256::from(20u64)),
        l1_block_number: 1,
        timestamp: 1_700_000_000,
        request_seq: dep_idx,
        base_fee_l1: FUZZ_L1_BASE_FEE,
    }
    .build()
    .expect("deposit");
    steps.push(msg_step(dep_idx, dep, 1));

    // Install a constraint weighting StorageAccessWrite heavily so write gas
    // dominates the backlog and the base fee escalates measurably.
    let cons = set_constraint_calldata(60, 100_000, 0, KIND_STORAGE_WRITE, 10_000);
    let i = idx.next();
    steps.push(msg_step(i, owner_tx(0, Some(ARBOWNER), cons, 2_000_000).build().expect("set"), 1));

    // Deploy the SSTORE contract.
    let i = idx.next();
    steps.push(msg_step(
        i,
        owner_tx(1, None, wrap_init_code(&SSTORE_RUNTIME), 2_000_000)
            .build()
            .expect("deploy"),
        1,
    ));
    let contract = create_address(owner, 1);

    // First write initialises slot 0 (zero→nonzero, a storage-growth create);
    // subsequent writes are resets that price into StorageAccessWrite.
    for (n, v) in [(2u64, 1u64), (3, 2), (4, 3)] {
        let i = idx.next();
        steps.push(msg_step(
            i,
            owner_tx(n, Some(contract), word(v).to_vec(), 2_000_000)
                .build()
                .expect("call"),
            1,
        ));
    }

    let scenario = Scenario {
        name: "mgas_write_constraint".into(),
        description: "write-weighted constraint backlog escalation".into(),
        setup: ScenarioSetup {
            l2_chain_id: L2_CHAIN_ID,
            arbos_version: ARBOS_VERSION,
            genesis: None,
        },
        steps,
    };

    let raw = rig.dual.run(&scenario).expect("dual run");
    let report = filter_genesis_noise(raw);
    assert!(
        report.is_clean(),
        "execution diverged under active write constraint: blocks={:?} txs={:?}",
        report.block_diffs,
        report.tx_diffs,
    );

    // Preconditions: the owner txs must have taken effect, so the write
    // backlog had something to grow. Empty code or an empty constraint set
    // would make the comparison below pass trivially.
    let code = rig.dual.right.code(contract, BlockId::Latest).expect("code");
    assert!(!code.is_empty(), "storage contract did not deploy");
    let nonce = rig.dual.right.nonce(owner, BlockId::Latest).expect("nonce");
    assert!(nonce >= 5, "owner txs did not all execute (nonce {nonce})");

    // Per-resource base fees (uint256[]) derived from the constraint backlog
    // must match byte-for-byte, and the write dimension must have escalated
    // above the floor — proving the SSTORE gas reached StorageAccessWrite and
    // arbreth priced it exactly as Nitro.
    let base_fee_call = TxRequest {
        from: Some(owner),
        to: Some(ARBGASINFO),
        data: Some(Bytes::from(selector4("getMultiGasBaseFee()").to_vec())),
        value: Some(U256::ZERO),
        gas: Some(3_000_000),
    };
    let l = rig.dual.left.eth_call(base_fee_call.clone(), BlockId::Latest).ok();
    let r = rig.dual.right.eth_call(base_fee_call, BlockId::Latest).ok();
    assert_eq!(l, r, "getMultiGasBaseFee diverged: nitro={l:?} arbreth={r:?}");

    let fees = decode_uint256_array(&r.expect("base fee bytes"));
    let floor = rig
        .dual
        .right
        .eth_call(
            TxRequest {
                from: Some(owner),
                to: Some(ARBGASINFO),
                data: Some(Bytes::from(selector4("getMinimumGasPrice()").to_vec())),
                value: Some(U256::ZERO),
                gas: Some(3_000_000),
            },
            BlockId::Latest,
        )
        .ok()
        .map(|b| U256::from_be_slice(&b))
        .unwrap_or(U256::ZERO);
    let max_fee = fees.iter().copied().max().unwrap_or(U256::ZERO);
    eprintln!("[mgas] floor={floor} max_fee={max_fee} fees={fees:?}");
    assert!(
        max_fee > floor,
        "no resource base fee escalated above floor {floor}; the write constraint \
         never bit, so the byte-for-byte comparison would be insensitive"
    );
}

/// Decode an ABI `uint256[]` return (offset, length, then words).
fn decode_uint256_array(b: &[u8]) -> Vec<U256> {
    if b.len() < 64 {
        return Vec::new();
    }
    let len = U256::from_be_slice(&b[32..64]).to::<usize>();
    (0..len)
        .filter_map(|i| {
            let s = 64 + i * 32;
            b.get(s..s + 32).map(U256::from_be_slice)
        })
        .collect()
}

/// EIP-7623 calldata floor parity. With the calldata-price increase enabled, a
/// data-heavy / compute-light transaction pays `floorDataGas` rather than its
/// (lower) execution gas. The receipt gas must match Nitro and equal the floor.
#[test]
#[ignore]
fn eip7623_calldata_floor_matches_nitro() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let owner = derive_address(owner_key());
    let mut rig = Rig::spawn(owner);
    let idx = Idx::new();
    let mut steps = Vec::new();

    let dep_idx = idx.next();
    let dep = DepositBuilder {
        from: FUNDER,
        to: owner,
        amount: U256::from(10u128).pow(U256::from(20u64)),
        l1_block_number: 1,
        timestamp: 1_700_000_000,
        request_seq: dep_idx,
        base_fee_l1: 0,
    }
    .build()
    .expect("deposit");
    steps.push(msg_step(dep_idx, dep, 1));

    // Enable the calldata-price increase (owner-only). base_fee_l1 = 0 on every
    // message keeps the L1 price ~0 so the floor tx incurs no poster gas to
    // lift execution above the floor.
    let mut enable = selector4("setCalldataPriceIncrease(bool)").to_vec();
    enable.extend_from_slice(&word(1));
    let i = idx.next();
    steps.push(msg_step(
        i,
        owner_tx_l1(0, Some(ARBOWNER), enable, 2_000_000, 0).build().expect("enable"),
        1,
    ));

    // Zero the L1 price so the floor tx pays no poster gas, letting the calldata
    // floor (rather than the L1 cost) decide gas.
    let mut set_price = selector4("setL1PricePerUnit(uint256)").to_vec();
    set_price.extend_from_slice(&word(0));
    let i = idx.next();
    steps.push(msg_step(
        i,
        owner_tx_l1(1, Some(ARBOWNER), set_price, 2_000_000, 0).build().expect("set price"),
        1,
    ));

    // Floor-binding tx: many zero calldata bytes to an empty account, with no
    // L1 base fee so there is no poster gas to lift execution above the floor.
    // intrinsic = 21000 + 1000*4 = 25000; floor = 21000 + 1000*10 = 31000.
    const ZERO_BYTES: usize = 1000;
    let eoa = address!("00000000000000000000000000000000deadbeef");
    let expected_floor = 21_000u64 + (ZERO_BYTES as u64) * 10;
    let i = idx.next();
    steps.push(msg_step(
        i,
        owner_tx_l1(2, Some(eoa), vec![0u8; ZERO_BYTES], 2_000_000, 0)
            .build()
            .expect("floor tx"),
        1,
    ));

    let scenario = Scenario {
        name: "eip7623_calldata_floor".into(),
        description: "calldata floor parity with the price increase enabled".into(),
        setup: ScenarioSetup {
            l2_chain_id: L2_CHAIN_ID,
            arbos_version: ARBOS_VERSION,
            genesis: None,
        },
        steps,
    };

    let raw = rig.dual.run(&scenario).expect("dual run");
    let report = filter_genesis_noise(raw);
    assert!(
        report.is_clean(),
        "calldata floor diverged from Nitro: blocks={:?} txs={:?}",
        report.block_diffs,
        report.tx_diffs,
    );

    // The floor tx is the latest block's last tx. Its gas must equal the floor,
    // proving the floor bound (otherwise the test is insensitive).
    let n = rig.dual.right.block(BlockId::Latest).expect("latest").number;
    let b = rig.dual.right.block(BlockId::Number(n)).expect("block");
    let hash = *b.tx_hashes.last().expect("floor tx");
    let gas = rig.dual.right.receipt(hash).expect("receipt").gas_used;
    eprintln!("[floor] block={n} floor_tx_gas={gas} expected_floor={expected_floor}");
    assert_eq!(
        gas, expected_floor,
        "floor did not bind: gas_used {gas} != floor {expected_floor}"
    );
}
