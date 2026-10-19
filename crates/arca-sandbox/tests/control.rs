//! Run control: sliced fuel, cancellation, deadlines and progress.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arca_sandbox::{
    CancelToken, FuelPolicy, GuestModule, Progress, RunOptions, Sandbox, SandboxError,
    SandboxPolicy,
};
use common::{I32, ModuleBuilder, RunBody, code, guest, op};

/// A budget no test run is expected to reach: only the controls can stop it.
const ENDLESS: FuelPolicy =
    FuelPolicy { base: 400_000_000_000, per_input_byte: 0, max: 400_000_000_000 };

fn policy(slice_fuel: u64) -> SandboxPolicy {
    SandboxPolicy { slice_fuel, ..SandboxPolicy::default() }
}

fn pinned(body: RunBody, policy: SandboxPolicy) -> (Sandbox, GuestModule) {
    let m = GuestModule::new("test-guest", guest(body, 1, None, 2));
    (Sandbox::new(policy, [m.pin()]), m)
}

#[test]
fn cancelling_from_another_thread_stops_an_endless_guest() {
    let (sb, m) =
        pinned(RunBody::InfiniteLoop, SandboxPolicy { fuel: ENDLESS, ..policy(1_000_000) });
    let token = CancelToken::new();
    let options = RunOptions::new().cancel_token(token.clone());
    let started = Instant::now();
    let result = std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(50));
            token.cancel();
        });
        sb.run_with(&m, b"x", &options)
    });
    assert_eq!(result.unwrap_err(), SandboxError::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(30), "cancellation must be prompt");
    assert!(token.is_cancelled());
}

#[test]
fn a_cancelled_token_prevents_any_execution() {
    let (sb, m) = pinned(RunBody::Echo, policy(1_000_000));
    let token = CancelToken::new();
    token.cancel();
    let options = RunOptions::new().cancel_token(token);
    assert_eq!(sb.run_with(&m, b"x", &options).unwrap_err(), SandboxError::Cancelled);
}

#[test]
fn an_unused_token_does_not_disturb_a_normal_run() {
    let (sb, m) = pinned(RunBody::Echo, policy(1_000_000));
    let options = RunOptions::new().cancel_token(CancelToken::new());
    let out = sb.run_with(&m, b"payload", &options).unwrap();
    assert_eq!(&out.payload[..], b"payload");
    assert_eq!(out.stats.slices, 1);
}

#[test]
fn the_deadline_stops_an_endless_guest() {
    let (sb, m) =
        pinned(RunBody::InfiniteLoop, SandboxPolicy { fuel: ENDLESS, ..policy(1_000_000) });
    let limit = Duration::from_millis(100);
    let started = Instant::now();
    let err = sb.run_with(&m, b"x", &RunOptions::new().deadline(limit)).unwrap_err();
    assert_eq!(err, SandboxError::DeadlineExceeded { limit });
    let took = started.elapsed();
    assert!(took >= limit, "stopped before the deadline: {took:?}");
    assert!(took < Duration::from_secs(30), "deadline overshoot: {took:?}");
}

#[test]
fn the_policy_wall_time_is_a_ceiling_that_options_cannot_raise() {
    let ceiling = Duration::from_millis(80);
    let policy = SandboxPolicy { fuel: ENDLESS, max_wall_time: Some(ceiling), ..policy(1_000_000) };
    let (sb, m) = pinned(RunBody::InfiniteLoop, policy);
    let options = RunOptions::new().deadline(Duration::from_secs(3600));
    assert_eq!(
        sb.run_with(&m, b"x", &options).unwrap_err(),
        SandboxError::DeadlineExceeded { limit: ceiling }
    );
    // And the plain `run` honours the policy ceiling as well.
    assert_eq!(sb.run(&m, b"x").unwrap_err(), SandboxError::DeadlineExceeded { limit: ceiling });
}

#[test]
fn a_tighter_option_deadline_wins_over_a_looser_policy_wall_time() {
    let policy = SandboxPolicy {
        fuel: ENDLESS,
        max_wall_time: Some(Duration::from_secs(3600)),
        ..policy(1_000_000)
    };
    let (sb, m) = pinned(RunBody::InfiniteLoop, policy);
    let limit = Duration::from_millis(60);
    assert_eq!(
        sb.run_with(&m, b"x", &RunOptions::new().deadline(limit)).unwrap_err(),
        SandboxError::DeadlineExceeded { limit }
    );
}

#[test]
fn progress_is_reported_monotonically_and_within_the_budget() {
    let seen: Arc<Mutex<Vec<Progress>>> = Arc::default();
    let sink = Arc::clone(&seen);
    let options = RunOptions::new().on_progress(move |p| {
        if let Ok(mut v) = sink.lock() {
            v.push(p);
        }
    });
    let fuel = FuelPolicy { base: 20_000_000, per_input_byte: 0, max: 20_000_000 };
    let (sb, m) = pinned(RunBody::InfiniteLoop, SandboxPolicy { fuel, ..policy(1_000_000) });
    assert_eq!(
        sb.run_with(&m, b"x", &options).unwrap_err(),
        SandboxError::FuelExhausted { limit: 20_000_000 }
    );
    let seen = seen.lock().unwrap();
    assert!(seen.len() >= 15, "expected a report per slice, got {}", seen.len());
    assert_eq!(seen[0].fuel_consumed, 0);
    for pair in seen.windows(2) {
        assert!(pair[1].fuel_consumed >= pair[0].fuel_consumed);
        assert!(pair[1].elapsed >= pair[0].elapsed);
    }
    for p in seen.iter() {
        assert_eq!(p.fuel_limit, 20_000_000);
        assert!(p.fuel_consumed <= p.fuel_limit);
    }
}

#[test]
fn slicing_never_changes_the_fuel_budget() {
    // ~1M fuel of real work, then a valid result.
    let body = || RunBody::Burn(100_000);
    let big = SandboxPolicy { slice_fuel: u64::MAX, ..SandboxPolicy::default() };
    let (sb_big, m_big) = pinned(body(), big);
    let reference = sb_big.run(&m_big, b"data").unwrap();
    assert_eq!(reference.stats.slices, 1);
    let used = reference.stats.fuel_consumed;
    assert!(used > 500_000, "the burn loop should cost real fuel, got {used}");

    for slice in [0, 10_000, 77_777, 1_000_000] {
        let (sb, m) = pinned(body(), policy(slice));
        let out = sb.run(&m, b"data").unwrap();
        assert_eq!(out.stats.fuel_consumed, used, "slice {slice}: accounting drifted");
        assert_eq!(&out.payload[..], b"data");
        if slice != 1_000_000 {
            assert!(out.stats.slices > 1, "slice {slice}: expected several slices");
        }

        // Exactly enough fuel succeeds, one unit less is refused — for every slice size.
        let exact = FuelPolicy { base: used, per_input_byte: 0, max: used };
        let (sb, m) = pinned(body(), SandboxPolicy { fuel: exact, ..policy(slice) });
        sb.run(&m, b"data").unwrap();
        let short = FuelPolicy { base: used - 1, per_input_byte: 0, max: used - 1 };
        let (sb, m) = pinned(body(), SandboxPolicy { fuel: short, ..policy(slice) });
        assert_eq!(
            sb.run(&m, b"data").unwrap_err(),
            SandboxError::FuelExhausted { limit: used - 1 },
            "slice {slice}"
        );
    }
}

#[test]
fn starvation_is_still_caught_with_tiny_slices() {
    let fuel = FuelPolicy { base: 1_000_000, per_input_byte: 0, max: 1_000_000 };
    // `slice_fuel: 0` is clamped, never a livelock.
    let (sb, m) = pinned(RunBody::InfiniteLoop, SandboxPolicy { fuel, ..policy(0) });
    assert_eq!(sb.run(&m, b"x").unwrap_err(), SandboxError::FuelExhausted { limit: 1_000_000 });
}

/// `memory.fill` over `pages` pages, then a result that is deliberately not an
/// envelope (so the run ends in `OutputInvalid` *after* the fill completed).
fn bulk_fill_guest(pages: u32) -> Vec<u8> {
    let mut b = ModuleBuilder::new();
    let t0 = b.ty(&[], &[I32]);
    let t1 = b.ty(&[I32], &[I32]);
    b.memory(pages, Some(pages), true);
    b.func(t0, op::i32_const(1), Some("arca_abi_version"));
    b.func(t1, op::i32_const(1024), Some("arca_input_reserve"));
    let fill = code(&[
        &op::i32_const(0),
        &op::i32_const(0xAA),
        &op::i32_const(i32::try_from(pages * 65_536).unwrap_or(i32::MAX)),
        &[0xFC, 0x0B, 0x00],
        &op::i32_const(0),
    ]);
    b.func(t1, fill, Some("arca_run"));
    b.func(t0, op::i32_const(0), Some("arca_output_ptr"));
    b.func(t0, op::i32_const(16), Some("arca_output_len"));
    b.build()
}

#[test]
fn an_instruction_larger_than_a_slice_still_runs() {
    // Filling 4 MiB costs far more than the 10 000-fuel minimum slice.
    let m = GuestModule::new("bulk", bulk_fill_guest(64));
    let sb = Sandbox::new(policy(0), [m.pin()]);
    assert!(matches!(sb.run(&m, b"x"), Err(SandboxError::OutputInvalid(_))));

    // With too little total fuel it is refused instead.
    let fuel = FuelPolicy { base: 20_000, per_input_byte: 0, max: 20_000 };
    let sb = Sandbox::new(SandboxPolicy { fuel, ..policy(0) }, [m.pin()]);
    assert_eq!(sb.run(&m, b"x").unwrap_err(), SandboxError::FuelExhausted { limit: 20_000 });
}

#[test]
fn a_panicking_progress_callback_is_contained() {
    let (sb, m) = pinned(RunBody::Echo, policy(1_000_000));
    let options = RunOptions::new().on_progress(|_| panic!("hostile callback"));
    assert_eq!(sb.run_with(&m, b"x", &options).unwrap_err(), SandboxError::WorkerPanicked);
    // The sandbox is stateless: the next run is unaffected.
    assert!(sb.run(&m, b"x").is_ok());
}

#[test]
fn stats_report_wall_time_and_slices() {
    let (sb, m) = pinned(RunBody::Burn(200_000), policy(100_000));
    let out = sb.run(&m, b"abc").unwrap();
    assert!(out.stats.slices > 1);
    assert!(out.stats.wall_time > Duration::ZERO);
}

#[test]
fn memory_limit_scales_with_the_input_up_to_the_ceiling() {
    let p = SandboxPolicy {
        max_memory_bytes: 10_000,
        memory_base_bytes: 1_000,
        memory_per_input_byte: 3,
        ..SandboxPolicy::default()
    };
    assert_eq!(p.memory_limit_for(0), 1_000);
    assert_eq!(p.memory_limit_for(100), 1_300);
    assert_eq!(p.memory_limit_for(3_000), 10_000);
    assert_eq!(p.memory_limit_for(usize::MAX), 10_000, "saturating, never wraps");
    // The default policy gives a small document a small cap and a big one the ceiling.
    let d = SandboxPolicy::default();
    assert!(d.memory_limit_for(100 << 10) < 128 << 20);
    assert_eq!(d.memory_limit_for(d.max_input_bytes), d.max_memory_bytes);
    assert!(d.max_memory_bytes >= d.max_input_bytes, "a maximal input must fit in the ceiling");
}

#[test]
fn the_per_run_memory_cap_follows_the_input() {
    // A guest that needs 2 MiB at start-up: refused for an empty-ish document
    // (cap 1 MiB + 1 000 B/byte), admitted once the input is large enough.
    let policy = SandboxPolicy {
        max_memory_bytes: 64 << 20,
        memory_base_bytes: 1 << 20,
        memory_per_input_byte: 1_000,
        ..policy(1_000_000)
    };
    let m = GuestModule::new("two-mib", guest(RunBody::Echo, 1, None, 32));
    let sb = Sandbox::new(policy, [m.pin()]);
    assert_eq!(sb.run(&m, &[7; 10]).unwrap_err(), SandboxError::MemoryLimitExceeded);
    let out = sb.run(&m, &vec![7u8; 10_000]).unwrap();
    assert_eq!(out.payload.len(), 10_000);
    // The hard ceiling still wins over a generous per-byte factor.
    let capped = SandboxPolicy { max_memory_bytes: 1 << 20, ..policy };
    let sb = Sandbox::new(capped, [m.pin()]);
    assert_eq!(sb.run(&m, &vec![7u8; 10_000]).unwrap_err(), SandboxError::MemoryLimitExceeded);
}

#[test]
fn default_fuel_policy_matches_the_documented_calibration() {
    let f = FuelPolicy::DEFAULT;
    assert_eq!(SandboxPolicy::default().fuel, f);
    // 1 MiB admits a 20:1 spreadsheet (≈ 1 400 fuel per XML byte).
    assert!(f.for_input(1 << 20) > 20 * (1 << 20) * 1_400);
    assert_eq!(f.for_input(usize::MAX), f.max, "the ceiling always applies");
}
