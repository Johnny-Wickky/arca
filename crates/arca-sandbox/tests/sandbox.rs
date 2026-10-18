//! Adversarial tests for the WebAssembly sandbox.

mod common;

use arca_abi::envelope::PayloadKind;
use arca_sandbox::{FuelPolicy, GuestModule, Sandbox, SandboxError, SandboxPolicy};
use common::{I32, ModuleBuilder, RunBody, code, guest, op};

fn pinned(bytes: Vec<u8>) -> (Sandbox, GuestModule) {
    pinned_with(bytes, SandboxPolicy::default())
}

fn pinned_with(bytes: Vec<u8>, policy: SandboxPolicy) -> (Sandbox, GuestModule) {
    let m = GuestModule::new("test-guest", bytes);
    (Sandbox::new(policy, [m.pin()]), m)
}

#[test]
fn echo_roundtrip_through_the_abi() {
    let (sb, m) = pinned(guest(RunBody::Echo, 1, None, 2));
    let input = "документ с данными".as_bytes();
    let out = sb.run(&m, input).unwrap();
    assert_eq!(out.status, 0);
    assert_eq!(out.kind, PayloadKind::XRayReport);
    assert_eq!(&out.payload[..], input);
    assert!(out.stats.fuel_consumed > 0 && out.stats.fuel_consumed < out.stats.fuel_limit);
    assert_eq!(out.stats.peak_memory_bytes, 2 * 65_536);
}

#[test]
fn empty_input_is_fine() {
    let (sb, m) = pinned(guest(RunBody::Echo, 1, None, 2));
    let out = sb.run(&m, b"").unwrap();
    assert!(out.payload.is_empty());
}

#[test]
fn unpinned_and_tampered_modules_are_refused() {
    let m = GuestModule::new("g", guest(RunBody::Echo, 1, None, 2));
    let sb = Sandbox::new(SandboxPolicy::default(), []);
    assert!(matches!(sb.run(&m, b"x"), Err(SandboxError::ModuleNotPinned(_))));
    // A pin for different bytes does not authorise this module.
    let other = GuestModule::new("o", guest(RunBody::Echo, 1, None, 3));
    let sb = Sandbox::new(SandboxPolicy::default(), [other.pin()]);
    assert!(matches!(sb.run(&m, b"x"), Err(SandboxError::ModuleNotPinned(_))));
}

#[test]
fn infinite_loop_runs_out_of_fuel() {
    let policy = SandboxPolicy {
        fuel: FuelPolicy { base: 5_000_000, per_input_byte: 0, max: 5_000_000 },
        ..SandboxPolicy::default()
    };
    let (sb, m) = pinned_with(guest(RunBody::InfiniteLoop, 1, None, 2), policy);
    assert_eq!(sb.run(&m, b"x").unwrap_err(), SandboxError::FuelExhausted { limit: 5_000_000 });
}

#[test]
fn memory_growth_is_capped() {
    let policy = SandboxPolicy { max_memory_bytes: 4 << 20, ..SandboxPolicy::default() };
    let (sb, m) = pinned_with(guest(RunBody::MemoryHog, 1, None, 2), policy);
    assert_eq!(sb.run(&m, b"x").unwrap_err(), SandboxError::MemoryLimitExceeded);
}

#[test]
fn oversized_initial_memory_is_refused_at_instantiation() {
    let policy = SandboxPolicy { max_memory_bytes: 1 << 20, ..SandboxPolicy::default() };
    let (sb, m) = pinned_with(guest(RunBody::Echo, 1, None, 64), policy);
    assert_eq!(sb.run(&m, b"x").unwrap_err(), SandboxError::MemoryLimitExceeded);
}

#[test]
fn unbounded_recursion_overflows_safely() {
    let (sb, m) = pinned(guest(RunBody::Recurse, 1, None, 2));
    assert_eq!(sb.run(&m, b"x").unwrap_err(), SandboxError::StackOverflow);
}

#[test]
fn traps_are_contained() {
    let (sb, m) = pinned(guest(RunBody::Trap, 1, None, 2));
    assert!(matches!(sb.run(&m, b"x"), Err(SandboxError::GuestTrap(_))));
}

#[test]
fn abi_version_is_checked() {
    let (sb, m) = pinned(guest(RunBody::Echo, 7, None, 2));
    assert_eq!(sb.run(&m, b"x").unwrap_err(), SandboxError::AbiMismatch(7));
}

#[test]
fn output_region_is_bounds_checked() {
    let (sb, m) = pinned(guest(RunBody::Echo, 1, Some(0x7FFF_FFF0), 2));
    assert!(matches!(sb.run(&m, b"x"), Err(SandboxError::OutputInvalid(_))));
    let (sb, m) = pinned(guest(RunBody::Echo, 1, Some(-5), 2));
    assert!(matches!(sb.run(&m, b"x"), Err(SandboxError::OutputInvalid(_))));
    // A region that is in bounds but does not hold an envelope.
    let (sb, m) = pinned(guest(RunBody::Echo, 1, Some(0), 2));
    assert!(matches!(sb.run(&m, b"x"), Err(SandboxError::OutputInvalid(_))));
}

#[test]
fn error_status_without_error_envelope_is_rejected() {
    let (sb, m) = pinned(guest(RunBody::Status(4), 1, None, 2));
    // The envelope region is zeroed memory: invalid.
    assert!(matches!(
        sb.run(&m, b"x"),
        Err(SandboxError::OutputInvalid(_) | SandboxError::GuestStatus(_))
    ));
}

#[test]
fn input_limits() {
    let policy = SandboxPolicy { max_input_bytes: 10, ..SandboxPolicy::default() };
    let (sb, m) = pinned_with(guest(RunBody::Echo, 1, None, 2), policy);
    assert_eq!(sb.run(&m, &[0; 11]).unwrap_err(), SandboxError::InputTooLarge(11));
    // The guest itself refuses > 60000 bytes.
    let (sb, m) = pinned(guest(RunBody::Echo, 1, None, 2));
    assert_eq!(sb.run(&m, &[0; 70_000]).unwrap_err(), SandboxError::GuestAllocationFailed);
}

#[test]
fn any_import_is_forbidden() {
    let mut b = ModuleBuilder::new();
    let t = b.ty(&[I32, I32, I32, I32], &[I32]);
    b.import_func("wasi_snapshot_preview1", "fd_write", t);
    b.memory(1, None, true);
    let (sb, m) = pinned(b.build());
    assert_eq!(
        sb.run(&m, b"x").unwrap_err(),
        SandboxError::ForbiddenImport {
            module: "wasi_snapshot_preview1".into(),
            name: "fd_write".into()
        }
    );
}

#[test]
fn start_functions_are_refused() {
    let mut b = ModuleBuilder::new();
    let t = b.ty(&[], &[]);
    let f = b.func(t, vec![], None);
    b.start(f);
    let (sb, m) = pinned(b.build());
    assert!(matches!(sb.run(&m, b"x"), Err(SandboxError::ModuleInvalid(_))));
}

#[test]
fn exports_are_validated() {
    // Missing everything but memory.
    let mut b = ModuleBuilder::new();
    b.memory(1, None, true);
    let (sb, m) = pinned(b.build());
    assert_eq!(sb.run(&m, b"x").unwrap_err(), SandboxError::MissingExport("arca_abi_version"));

    // Wrong signature for arca_run.
    let mut b = ModuleBuilder::new();
    let t0 = b.ty(&[], &[I32]);
    let t1 = b.ty(&[I32], &[I32]);
    b.memory(1, None, true);
    b.func(t0, op::i32_const(1), Some("arca_abi_version"));
    b.func(t1, op::i32_const(0), Some("arca_input_reserve"));
    b.func(t0, op::i32_const(0), Some("arca_run"));
    b.func(t0, op::i32_const(0), Some("arca_output_ptr"));
    b.func(t0, op::i32_const(0), Some("arca_output_len"));
    let (sb, m) = pinned(b.build());
    assert_eq!(sb.run(&m, b"x").unwrap_err(), SandboxError::ExportSignature("arca_run"));

    // No memory export.
    let mut b = ModuleBuilder::new();
    let t0 = b.ty(&[], &[I32]);
    let t1 = b.ty(&[I32], &[I32]);
    b.memory(1, None, false);
    b.func(t0, op::i32_const(1), Some("arca_abi_version"));
    b.func(t1, op::i32_const(0), Some("arca_input_reserve"));
    b.func(t1, op::i32_const(0), Some("arca_run"));
    b.func(t0, op::i32_const(0), Some("arca_output_ptr"));
    b.func(t0, op::i32_const(0), Some("arca_output_len"));
    let (sb, m) = pinned(b.build());
    assert_eq!(sb.run(&m, b"x").unwrap_err(), SandboxError::MissingExport("memory"));
}

#[test]
fn garbage_modules_are_rejected_without_panicking() {
    let good = guest(RunBody::Echo, 1, None, 2);
    for i in 8..good.len() {
        for mask in [0x01u8, 0x80] {
            let mut bad = good.clone();
            bad[i] ^= mask;
            let (sb, m) = pinned(bad);
            let _ = sb.run(&m, b"probe");
        }
    }
    let (sb, m) = pinned(b"\0asm\x01\0\0\0\xFF\xFF\xFF".to_vec());
    assert!(matches!(sb.run(&m, b""), Err(SandboxError::ModuleInvalid(_))));
}

#[test]
fn hostile_guest_cannot_escape_via_memory_writes() {
    // arca_run scribbles over the whole first page, including the envelope.
    let mut b = ModuleBuilder::new();
    let t0 = b.ty(&[], &[I32]);
    let t1 = b.ty(&[I32], &[I32]);
    b.memory(1, Some(1), true);
    b.func(t0, op::i32_const(1), Some("arca_abi_version"));
    b.func(t1, op::i32_const(1024), Some("arca_input_reserve"));
    // memory.fill(0, 0xAA, 65536)
    let fill = code(&[
        &op::i32_const(0),
        &op::i32_const(0xAA),
        &op::i32_const(65_536),
        &[0xFC, 0x0B, 0x00],
        &op::i32_const(0),
    ]);
    b.func(t1, fill, Some("arca_run"));
    b.func(t0, op::i32_const(65_000), Some("arca_output_ptr"));
    b.func(t0, op::i32_const(10_000), Some("arca_output_len"));
    let (sb, m) = pinned(b.build());
    assert!(matches!(sb.run(&m, b"x"), Err(SandboxError::OutputInvalid(_))));
}

#[test]
fn concurrent_runs_are_isolated() {
    let (sb, m) = pinned(guest(RunBody::Echo, 1, None, 2));
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..8u8)
            .map(|i| {
                let sb = &sb;
                let m = &m;
                s.spawn(move || {
                    let input = vec![i; 1000 + usize::from(i)];
                    let out = sb.run(m, &input).unwrap();
                    assert_eq!(&out.payload[..], &input[..]);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    });
}
