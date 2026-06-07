//! Differential tests for ArbSys call-context handling (value transfer and
//! read-only enforcement) vs the Nitro reference.

use alloy_primitives::{Address, Bytes, U256};
use arb_fuzz::{
    arbitrary_impls::{interop::wrap_init_code, message_step},
    guards::GuardedRun,
    scaffolding::{
        eoa_create_addr, fund_interop_eoa, selector4, signed, DEPLOY_GAS_CAP, INVOKE_GAS_CAP,
    },
    shared_nodes::next_msg_idx,
};
use arb_test_harness::messaging::MessageBuilder;

const ARBSYS: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x64,
]);

/// Runtime that `CALL`s `precompile` with `selector` (no args) forwarding all
/// gas and `value` wei, then stores the call's success flag at slot 0.
fn call_with_value_runtime(precompile: u8, selector: [u8; 4], value: u8) -> Vec<u8> {
    let mut c = Vec::new();
    c.push(0x63); // PUSH4 selector
    c.extend_from_slice(&selector);
    c.extend_from_slice(&[0x60, 0xE0, 0x1b, 0x60, 0x00, 0x52]); // PUSH1 0xE0 SHL PUSH1 0 MSTORE
                                                                // CALL operands (reverse): retLen retOff argLen argOff value addr gas
    c.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x60, 0x04, 0x60, 0x00]);
    c.extend_from_slice(&[0x60, value]); // PUSH1 value
    c.extend_from_slice(&[0x60, precompile]); // PUSH1 precompile
    c.push(0x5a); // GAS
    c.extend_from_slice(&[0xf1, 0x60, 0x00, 0x55, 0x00]); // CALL PUSH1 0 SSTORE STOP
    c
}

/// Runtime that `CALL`s `precompile` with `selector` + one 32-byte `arg`
/// forwarding all gas and `value` wei, then stores the success flag at slot 0.
fn call_arg_with_value_runtime(
    precompile: u8,
    selector: [u8; 4],
    arg: [u8; 32],
    value: u8,
) -> Vec<u8> {
    let mut c = Vec::new();
    c.push(0x63); // PUSH4 selector
    c.extend_from_slice(&selector);
    c.extend_from_slice(&[0x60, 0xE0, 0x1b, 0x60, 0x00, 0x52]); // PUSH1 0xE0 SHL PUSH1 0 MSTORE
    c.push(0x7f); // PUSH32 arg
    c.extend_from_slice(&arg);
    c.extend_from_slice(&[0x60, 0x04, 0x52]); // PUSH1 4 MSTORE
                                              // CALL operands (reverse): retLen retOff argLen(0x24) argOff value addr gas
    c.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0x60, 0x24, 0x60, 0x00]);
    c.extend_from_slice(&[0x60, value]);
    c.extend_from_slice(&[0x60, precompile]);
    c.push(0x5a); // GAS
    c.extend_from_slice(&[0xf1, 0x60, 0x00, 0x55, 0x00]); // CALL PUSH1 0 SSTORE STOP
    c
}

/// A payable ArbSys method (`withdrawEth`) called with value must NOT be
/// rejected by the value guard: the withdrawal proceeds identically on both
/// nodes.
#[test]
#[ignore]
fn value_to_payable_withdraweth_matches_nitro() {
    let mut steps = Vec::new();
    fund_interop_eoa(&mut steps);
    let caller = eoa_create_addr(0);

    let mut dest = [0u8; 32];
    dest[12..].copy_from_slice(&[0xde; 20]); // withdraw to 0xdede…de
    let runtime = call_arg_with_value_runtime(0x64, selector4("withdrawEth(address)"), dest, 100);
    let deploy = signed(
        0,
        None,
        Bytes::from(wrap_init_code(&runtime)),
        U256::from(1000u64),
        DEPLOY_GAS_CAP,
    )
    .build()
    .expect("deploy caller");
    let idx = next_msg_idx();
    steps.push(message_step(idx, deploy, idx));

    let invoke = signed(1, Some(caller), Bytes::new(), U256::ZERO, INVOKE_GAS_CAP)
        .build()
        .expect("invoke caller");
    let idx = next_msg_idx();
    steps.push(message_step(idx, invoke, idx));

    GuardedRun::new("value_to_payable_withdraweth", steps)
        .diff_account(caller)
        .diff_account(ARBSYS)
        .diff_storage(caller, vec![U256::ZERO])
        .run();
}

/// A non-payable ArbSys method (`arbBlockNumber`) called with value must behave
/// identically on both nodes: the value transfer, the call's success, and every
/// touched balance must match.
#[test]
#[ignore]
fn value_to_nonpayable_arbsys_matches_nitro() {
    let mut steps = Vec::new();
    fund_interop_eoa(&mut steps);
    let caller = eoa_create_addr(0);

    let runtime = call_with_value_runtime(0x64, selector4("arbBlockNumber()"), 100);
    let deploy = signed(
        0,
        None,
        Bytes::from(wrap_init_code(&runtime)),
        U256::from(1000u64),
        DEPLOY_GAS_CAP,
    )
    .build()
    .expect("deploy caller");
    let idx = next_msg_idx();
    steps.push(message_step(idx, deploy, idx));

    let invoke = signed(1, Some(caller), Bytes::new(), U256::ZERO, INVOKE_GAS_CAP)
        .build()
        .expect("invoke caller");
    let idx = next_msg_idx();
    steps.push(message_step(idx, invoke, idx));

    GuardedRun::new("value_to_nonpayable_arbsys", steps)
        .diff_account(caller)
        .diff_account(ARBSYS)
        .diff_storage(caller, vec![U256::ZERO])
        .run();
}
