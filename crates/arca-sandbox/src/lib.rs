// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! # arca-sandbox — the second isolation ring
//!
//! Every document is parsed by a WebAssembly *guest* (a `wasm32-unknown-unknown`
//! module, see [`arca_abi`]) executed by the [`wasmi`] interpreter inside a
//! fresh, single-use [`wasmi::Store`] on a dedicated worker thread.
//!
//! ## Guarantees
//!
//! | property | mechanism |
//! |---|---|
//! | **no system access** | modules with *any* import are refused before instantiation; WebAssembly has no ambient authority, so a zero-import module cannot reach the network, file system, clock, randomness or IPC |
//! | **no code injection** | only modules whose SHA-256 is on the allow-list ([`ModulePin`]) are executed; validation is always on; `start` functions and non-essential proposals are disabled |
//! | **bounded CPU** | fuel metering: every instruction costs fuel; budget = `base + per_byte × input` capped at `max` ([`FuelPolicy`]); exhaustion traps deterministically |
//! | **bounded wall-clock time** | the budget is handed to the guest in slices ([`SandboxPolicy::slice_fuel`]); between slices the host checks a monotonic deadline ([`SandboxPolicy::max_wall_time`], [`RunOptions::deadline`]) — a slow device cannot turn a large fuel budget into an unbounded wait |
//! | **cancellable** | a [`CancelToken`] is polled at every slice boundary, so the UI can abandon a document within one slice (milliseconds) without killing the process |
//! | **bounded memory** | a [`wasmi::ResourceLimiter`] caps linear memory and table growth; the cap is proportional to the document (`base + factor × input`, never above a hard ceiling — [`SandboxPolicy::memory_limit_for`]); exceeding it traps immediately |
//! | **bounded stack** | interpreter recursion depth and value-stack height limits |
//! | **bounded I/O** | input and output sizes are capped; the output region is bounds-checked against the guest memory before it is copied |
//! | **ephemeral** | engine, store and memory exist only for one call; the guest memory is zeroised before it is released, and returned buffers are [`zeroize::Zeroizing`] |
//! | **fault containment** | the guest runs on its own thread; even a panic in the interpreter is reported as [`SandboxError::WorkerPanicked`] instead of unwinding into the app |
//!
//! `wasmi` was chosen over JIT runtimes because iOS forbids third-party
//! executable memory (W^X), and because an interpreter keeps guest code off
//! the native instruction stream entirely.

#![forbid(unsafe_code)]

use core::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use arca_abi::envelope::{self, PayloadKind};
use arca_abi::{ABI_VERSION, exports};
use arca_digest::Digest;
use wasmi::{
    CompilationMode, Config, EnforcedLimits, Engine, ExternType, Instance, Linker, Memory, Module,
    Store, TrapCode, TypedFunc, TypedResumableCall, ValType, WasmResults,
};
use wasmi_core::{LimiterError, ResourceLimiter};
use zeroize::{Zeroize, Zeroizing};

/// CPU budget for one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuelPolicy {
    /// Fixed allowance (start-up, small documents).
    pub base: u64,
    /// Additional fuel per input byte.
    pub per_input_byte: u64,
    /// Absolute ceiling.
    pub max: u64,
}

impl FuelPolicy {
    /// The default budget, calibrated against measurements of the real guest
    /// (`cargo run --release -p arca-guest-xray --example measure`):
    ///
    /// | document | input bytes | fuel per input byte |
    /// |---|---|---|
    /// | word-processing text, 10:1 deflate | 750 KB | ≈ 4 500 |
    /// | spreadsheet, 5:1 deflate (random numbers) | 0.1 – 2 MB | ≈ 6 000 |
    /// | spreadsheet, 10:1 deflate (repetitive cells) | 650 KB | ≈ 15 000 |
    ///
    /// The engine adds to this budget for a preview or a sterile copy once a scan has said how
    /// much the document expands to (`arca-engine`, `policy_for`).
    ///
    /// Spreadsheets are the worst case (many tiny elements, ≈ 1 400 fuel per
    /// XML byte). `32 000` per input byte therefore admits tables compressing
    /// up to roughly 20:1 and still stops a runaway guest after seconds, not
    /// minutes; the wall-clock deadline ([`SandboxPolicy::max_wall_time`]) and
    /// the cancel token bound the *time* regardless of the device speed.
    pub const DEFAULT: Self =
        Self { base: 2_000_000_000, per_input_byte: 32_000, max: 2_000_000_000_000 };

    /// Fuel granted for an input of `len` bytes.
    #[must_use]
    pub fn for_input(&self, len: usize) -> u64 {
        let len = u64::try_from(len).unwrap_or(u64::MAX);
        self.base.saturating_add(self.per_input_byte.saturating_mul(len)).min(self.max)
    }
}

/// Resource policy of the sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxPolicy {
    /// Hard ceiling of the guest's linear memory, whatever the input size.
    pub max_memory_bytes: usize,
    /// Linear memory every run may use regardless of input size (the guest's
    /// own code, stack and allocator metadata).
    pub memory_base_bytes: usize,
    /// Additional linear memory per input byte: the guest holds a copy of the
    /// document plus its decompressed parts and parse state, so a document of
    /// `n` bytes legitimately needs a small multiple of `n`.
    pub memory_per_input_byte: usize,
    /// Maximum number of table elements.
    pub max_table_elements: usize,
    /// CPU budget.
    pub fuel: FuelPolicy,
    /// Largest accepted document.
    pub max_input_bytes: usize,
    /// Largest accepted result envelope.
    pub max_output_bytes: usize,
    /// Largest accepted module.
    pub max_module_bytes: usize,
    /// Maximum guest call depth.
    pub max_recursion_depth: usize,
    /// Maximum interpreter value-stack height in bytes.
    pub max_stack_height: usize,
    /// Native stack of the worker thread.
    pub worker_stack_bytes: usize,
    /// Apply [`EnforcedLimits::strict`] to module compilation.
    pub strict_module_limits: bool,
    /// Fuel handed to the guest per slice. The host checks cancellation, the
    /// deadline and reports progress at every slice boundary, so this is the
    /// responsiveness granularity (smaller = more responsive, slightly more
    /// overhead). Clamped to at least [`MIN_SLICE_FUEL`]. It never changes
    /// *how much* the guest may compute — only how often the host looks.
    pub slice_fuel: u64,
    /// Absolute wall-clock ceiling of one run, measured from the start of
    /// [`Sandbox::run_with`] (module hashing and compilation included).
    /// `None` disables it, leaving fuel as the only CPU bound.
    pub max_wall_time: Option<Duration>,
}

/// Smallest fuel slice the sandbox will use (see [`SandboxPolicy::slice_fuel`]).
pub const MIN_SLICE_FUEL: u64 = 10_000;

impl SandboxPolicy {
    /// Linear-memory limit for an input of `len` bytes:
    /// `min(max_memory_bytes, memory_base_bytes + memory_per_input_byte × len)`.
    ///
    /// Scaling the limit with the document keeps the cap tight for the common
    /// small file (a hostile 100 KiB document cannot take hundreds of MiB)
    /// while a legitimately large file is not refused for a fixed, arbitrary
    /// number.
    #[must_use]
    pub fn memory_limit_for(&self, len: usize) -> usize {
        self.memory_base_bytes
            .saturating_add(self.memory_per_input_byte.saturating_mul(len))
            .min(self.max_memory_bytes)
    }
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            max_memory_bytes: 1 << 30,
            memory_base_bytes: 96 << 20,
            memory_per_input_byte: 4,
            max_table_elements: 100_000,
            fuel: FuelPolicy::DEFAULT,
            max_input_bytes: 512 << 20,
            max_output_bytes: 64 << 20,
            max_module_bytes: 32 << 20,
            max_recursion_depth: 4_096,
            max_stack_height: 8 << 20,
            worker_stack_bytes: 8 << 20,
            strict_module_limits: false,
            slice_fuel: 25_000_000,
            max_wall_time: Some(Duration::from_secs(300)),
        }
    }
}

/// A pinned module identity: only modules whose bytes hash to `digest` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModulePin {
    /// Human-readable name (e.g. `arca-guest-xray`).
    pub name: String,
    /// SHA-256 of the exact module bytes.
    pub digest: Digest,
}

/// A guest module and its digest.
#[derive(Clone)]
pub struct GuestModule {
    name: String,
    bytes: Arc<[u8]>,
    digest: Digest,
}

impl fmt::Debug for GuestModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuestModule")
            .field("name", &self.name)
            .field("len", &self.bytes.len())
            .field("digest", &self.digest)
            .finish()
    }
}

impl GuestModule {
    /// Wraps module bytes and computes their digest.
    #[must_use]
    pub fn new(name: impl Into<String>, bytes: impl Into<Arc<[u8]>>) -> Self {
        let bytes = bytes.into();
        let digest = arca_digest::sha256(&bytes);
        Self { name: name.into(), bytes, digest }
    }

    /// SHA-256 of the module bytes.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Module name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The pin that allows exactly this module.
    #[must_use]
    pub fn pin(&self) -> ModulePin {
        ModulePin { name: self.name.clone(), digest: self.digest }
    }
}

/// Why a sandboxed run failed. Every variant is a *contained* failure: the
/// document is treated as unreadable/hostile, the app keeps running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// The document exceeds [`SandboxPolicy::max_input_bytes`].
    InputTooLarge(usize),
    /// The module exceeds [`SandboxPolicy::max_module_bytes`].
    ModuleTooLarge(usize),
    /// The module's digest is not on the allow-list.
    ModuleNotPinned(Digest),
    /// The module failed validation or compilation.
    ModuleInvalid(String),
    /// The module declares an import (the sandbox grants none).
    ForbiddenImport {
        /// Import module name.
        module: String,
        /// Import field name.
        name: String,
    },
    /// A required export is missing.
    MissingExport(&'static str),
    /// A required export has the wrong type.
    ExportSignature(&'static str),
    /// The guest implements a different ABI version.
    AbiMismatch(i32),
    /// The CPU budget was exhausted.
    FuelExhausted {
        /// Fuel granted for this run.
        limit: u64,
    },
    /// The run was abandoned through its [`CancelToken`].
    Cancelled,
    /// The wall-clock deadline passed before the guest finished.
    DeadlineExceeded {
        /// The deadline that was in force.
        limit: Duration,
    },
    /// The guest tried to exceed its memory or table limit.
    MemoryLimitExceeded,
    /// The guest exceeded its call depth / stack.
    StackOverflow,
    /// The guest trapped (e.g. `unreachable`, out-of-bounds access, a Rust panic).
    GuestTrap(String),
    /// The guest could not allocate the input buffer.
    GuestAllocationFailed,
    /// The guest reported an error status.
    GuestStatus(i32),
    /// The output region is outside the guest memory or malformed.
    OutputInvalid(&'static str),
    /// The output exceeds [`SandboxPolicy::max_output_bytes`].
    OutputTooLarge(usize),
    /// The worker thread could not be started.
    WorkerSpawn(String),
    /// The worker thread panicked (interpreter fault).
    WorkerPanicked,
    /// Unexpected interpreter error.
    Internal(String),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge(n) => write!(f, "input of {n} bytes exceeds the sandbox limit"),
            Self::ModuleTooLarge(n) => write!(f, "module of {n} bytes exceeds the sandbox limit"),
            Self::ModuleNotPinned(d) => write!(f, "module {d} is not on the allow-list"),
            Self::ModuleInvalid(e) => write!(f, "module rejected: {e}"),
            Self::ForbiddenImport { module, name } => {
                write!(f, "module imports {module}::{name}; guests may not import anything")
            }
            Self::MissingExport(n) => write!(f, "module does not export {n}"),
            Self::ExportSignature(n) => write!(f, "export {n} has the wrong type"),
            Self::AbiMismatch(v) => {
                write!(f, "guest implements ABI version {v}, host expects {ABI_VERSION}")
            }
            Self::FuelExhausted { limit } => write!(f, "CPU budget of {limit} fuel exhausted"),
            Self::Cancelled => f.write_str("run cancelled"),
            Self::DeadlineExceeded { limit } => {
                write!(f, "wall-clock deadline of {} ms exceeded", limit.as_millis())
            }
            Self::MemoryLimitExceeded => f.write_str("guest exceeded its memory limit"),
            Self::StackOverflow => f.write_str("guest exceeded its call stack"),
            Self::GuestTrap(t) => write!(f, "guest trapped: {t}"),
            Self::GuestAllocationFailed => f.write_str("guest could not allocate the input buffer"),
            Self::GuestStatus(s) => write!(f, "guest returned status {s}"),
            Self::OutputInvalid(why) => write!(f, "invalid guest output: {why}"),
            Self::OutputTooLarge(n) => write!(f, "guest output of {n} bytes exceeds the limit"),
            Self::WorkerSpawn(e) => write!(f, "could not start sandbox worker: {e}"),
            Self::WorkerPanicked => f.write_str("sandbox worker panicked"),
            Self::Internal(e) => write!(f, "sandbox internal error: {e}"),
        }
    }
}

impl core::error::Error for SandboxError {}

/// Resource accounting of one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionStats {
    /// Fuel granted.
    pub fuel_limit: u64,
    /// Fuel consumed.
    pub fuel_consumed: u64,
    /// Peak linear-memory size in bytes.
    pub peak_memory_bytes: usize,
    /// Number of fuel slices the guest needed (1 = it finished in the first).
    pub slices: u64,
    /// Wall-clock time of the whole run.
    pub wall_time: Duration,
}

/// A successful run.
#[derive(Debug)]
pub struct GuestOutput {
    /// Status returned by `arca_run` (0 = OK).
    pub status: i32,
    /// Kind of the payload.
    pub kind: PayloadKind,
    /// Wire-encoded payload (zeroised on drop).
    pub payload: Zeroizing<Vec<u8>>,
    /// Resource accounting.
    pub stats: ExecutionStats,
}

/// A cloneable flag that abandons a run from another thread.
///
/// The guest cannot observe or influence it: the host polls it at every fuel
/// slice boundary and then drops the whole store.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A fresh, un-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation; idempotent and safe from any thread.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether [`cancel`](Self::cancel) was called on any clone of this token.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// A snapshot of a running guest, delivered at fuel slice boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Fuel consumed so far.
    pub fuel_consumed: u64,
    /// Fuel granted for the whole run (an upper bound, not a forecast: most
    /// documents finish well below it).
    pub fuel_limit: u64,
    /// Wall-clock time since the run started.
    pub elapsed: Duration,
}

type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

/// Per-run controls: cancellation, a tighter deadline, progress reporting.
///
/// Options can only make a run *stricter* than the [`SandboxPolicy`]; the
/// policy ceilings always apply.
#[derive(Clone, Default)]
pub struct RunOptions {
    cancel: Option<CancelToken>,
    deadline: Option<Duration>,
    progress: Option<ProgressFn>,
}

impl fmt::Debug for RunOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunOptions")
            .field("cancel", &self.cancel)
            .field("deadline", &self.deadline)
            .field("progress", &self.progress.as_ref().map(|_| "<callback>"))
            .finish()
    }
}

impl RunOptions {
    /// No cancellation, no extra deadline, no progress callback.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Abandon the run when `token` is cancelled.
    #[must_use]
    pub fn cancel_token(mut self, token: CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// Fail with [`SandboxError::DeadlineExceeded`] after `limit` of wall-clock
    /// time (never longer than [`SandboxPolicy::max_wall_time`]).
    #[must_use]
    pub fn deadline(mut self, limit: Duration) -> Self {
        self.deadline = Some(limit);
        self
    }

    /// Call `callback` at every slice boundary, on the sandbox worker thread.
    /// Keep it cheap and non-blocking; a panic inside it is contained like any
    /// other worker fault.
    #[must_use]
    pub fn on_progress(mut self, callback: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        self.progress = Some(Arc::new(callback));
        self
    }
}

/// Hands the fuel budget to the guest slice by slice and watches the clock.
///
/// Invariants: `consumed + (slice_grant - remaining)` is the exact fuel the
/// guest has used; it never exceeds `total`; each slice grants at least the
/// fuel the pending instruction requires, so every slice consumes at least one
/// unit and the run ends after at most `total` slices.
struct Meter<'a> {
    total: u64,
    /// Fuel consumed in slices that have already ended.
    consumed: u64,
    /// Fuel placed in the store for the current slice.
    slice_grant: u64,
    slice: u64,
    slices: u64,
    started: Instant,
    deadline: Option<(Instant, Duration)>,
    cancel: Option<&'a CancelToken>,
    progress: Option<&'a ProgressFn>,
}

impl<'a> Meter<'a> {
    fn new(policy: &SandboxPolicy, options: &'a RunOptions, total: u64, started: Instant) -> Self {
        let limit = match (policy.max_wall_time, options.deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Self {
            total,
            consumed: 0,
            slice_grant: 0,
            slice: policy.slice_fuel.max(MIN_SLICE_FUEL),
            slices: 0,
            started,
            deadline: limit.and_then(|d| started.checked_add(d).map(|at| (at, d))),
            cancel: options.cancel.as_ref(),
            progress: options.progress.as_ref(),
        }
    }

    /// Fails if the run was cancelled or its deadline has passed.
    fn check(&self) -> Result<(), SandboxError> {
        if self.cancel.is_some_and(CancelToken::is_cancelled) {
            return Err(SandboxError::Cancelled);
        }
        match self.deadline {
            Some((at, limit)) if Instant::now() >= at => {
                Err(SandboxError::DeadlineExceeded { limit })
            }
            _ => Ok(()),
        }
    }

    /// Fuel used so far, given the fuel still in the store.
    fn used(&self, store: &Store<Guard>) -> u64 {
        let remaining = store.get_fuel().unwrap_or(0);
        self.consumed.saturating_add(self.slice_grant.saturating_sub(remaining))
    }

    /// Loads the next slice into the store.
    ///
    /// `required` is the fuel the instruction that ran dry needs; the slice is
    /// never smaller, so that instruction (e.g. a large `memory.fill`) can run.
    fn refill(&mut self, store: &mut Store<Guard>, required: u64) -> Result<(), SandboxError> {
        self.consumed = self.used(store);
        self.check()?;
        let left = self.total.saturating_sub(self.consumed);
        if required > left {
            return Err(SandboxError::FuelExhausted { limit: self.total });
        }
        if let Some(report) = self.progress {
            report(Progress {
                fuel_consumed: self.consumed,
                fuel_limit: self.total,
                elapsed: self.started.elapsed(),
            });
        }
        let grant = self.slice.max(required).min(left);
        store.set_fuel(grant).map_err(|e| SandboxError::Internal(e.to_string()))?;
        self.slice_grant = grant;
        self.slices = self.slices.saturating_add(1);
        Ok(())
    }
}

/// Store data: the resource limiter and its observations.
#[derive(Debug)]
struct Guard {
    max_memory: usize,
    max_table: usize,
    memory_denied: bool,
    peak_memory: usize,
}

impl ResourceLimiter for Guard {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, LimiterError> {
        if desired > self.max_memory || maximum.is_some_and(|m| desired > m) {
            self.memory_denied = true;
            return Err(LimiterError::ResourceLimiterDeniedAllocation);
        }
        self.peak_memory = self.peak_memory.max(desired);
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, LimiterError> {
        if desired > self.max_table || maximum.is_some_and(|m| desired > m) {
            self.memory_denied = true;
            return Err(LimiterError::ResourceLimiterDeniedAllocation);
        }
        Ok(true)
    }

    fn instances(&self) -> usize {
        1
    }

    fn tables(&self) -> usize {
        1
    }

    fn memories(&self) -> usize {
        1
    }
}

/// The sandbox: a policy plus an allow-list of module digests.
#[derive(Debug, Clone)]
pub struct Sandbox {
    policy: SandboxPolicy,
    pins: Vec<ModulePin>,
}

impl Sandbox {
    /// Creates a sandbox that only executes the pinned modules.
    #[must_use]
    pub fn new(policy: SandboxPolicy, pins: impl IntoIterator<Item = ModulePin>) -> Self {
        Self { policy, pins: pins.into_iter().collect() }
    }

    /// The active policy.
    #[must_use]
    pub const fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }

    /// Runs `module` over `input` on a dedicated worker thread.
    ///
    /// Equivalent to [`run_with`](Self::run_with) with default [`RunOptions`].
    ///
    /// # Errors
    /// See [`SandboxError`]; every failure is contained.
    pub fn run(&self, module: &GuestModule, input: &[u8]) -> Result<GuestOutput, SandboxError> {
        self.run_with(module, input, &RunOptions::default())
    }

    /// Runs `module` over `input` on a dedicated worker thread, honouring the
    /// cancellation token, deadline and progress callback in `options`.
    ///
    /// # Errors
    /// See [`SandboxError`]; every failure is contained. Cancellation and
    /// deadline expiry surface as [`SandboxError::Cancelled`] and
    /// [`SandboxError::DeadlineExceeded`]; in both cases the guest memory is
    /// wiped exactly as after a normal run.
    pub fn run_with(
        &self,
        module: &GuestModule,
        input: &[u8],
        options: &RunOptions,
    ) -> Result<GuestOutput, SandboxError> {
        self.run_request(module, &[], input, options)
    }

    /// Like [`run_with`](Self::run_with), but the guest's input region holds
    /// `prefix` immediately followed by `input` (the ABI [`request`](arca_abi::request)
    /// header and the document). Taking the two parts separately spares the
    /// host a second copy of a document that can be tens of megabytes.
    ///
    /// Fuel and memory limits are computed from the combined length.
    ///
    /// # Errors
    /// See [`SandboxError`]; every failure is contained.
    pub fn run_request(
        &self,
        module: &GuestModule,
        prefix: &[u8],
        input: &[u8],
        options: &RunOptions,
    ) -> Result<GuestOutput, SandboxError> {
        let started = Instant::now();
        std::thread::scope(|scope| {
            let handle = std::thread::Builder::new()
                .name(String::from("arca-sandbox"))
                .stack_size(self.policy.worker_stack_bytes)
                .spawn_scoped(scope, || self.execute(module, prefix, input, options, started))
                .map_err(|e| SandboxError::WorkerSpawn(e.to_string()))?;
            handle.join().map_err(|_| SandboxError::WorkerPanicked)?
        })
    }

    /// Runs `module` over `input` on the calling thread (no panic isolation).
    ///
    /// # Errors
    /// See [`SandboxError`].
    pub fn run_on_current_thread(
        &self,
        module: &GuestModule,
        input: &[u8],
    ) -> Result<GuestOutput, SandboxError> {
        self.execute(module, &[], input, &RunOptions::default(), Instant::now())
    }

    fn engine(&self) -> Engine {
        let p = &self.policy;
        let mut config = Config::default();
        config
            .consume_fuel(true)
            .allow_start_fn(false)
            .ignore_custom_sections(true)
            .compilation_mode(CompilationMode::Eager)
            .wasm_multi_memory(false)
            .wasm_tail_call(false)
            .wasm_extended_const(false)
            .wasm_custom_page_sizes(false)
            .wasm_wide_arithmetic(false)
            .set_max_recursion_depth(p.max_recursion_depth);
        // Raising the maximum never conflicts with the default minimum.
        let default_min = 1 << 10;
        config.set_max_stack_height(p.max_stack_height.max(default_min));
        if p.strict_module_limits {
            config.enforced_limits(EnforcedLimits::strict());
        }
        Engine::new(&config)
    }

    fn execute(
        &self,
        module: &GuestModule,
        prefix: &[u8],
        input: &[u8],
        options: &RunOptions,
        started: Instant,
    ) -> Result<GuestOutput, SandboxError> {
        let p = &self.policy;
        let total_len =
            prefix.len().checked_add(input.len()).ok_or(SandboxError::InputTooLarge(usize::MAX))?;
        if total_len > p.max_input_bytes {
            return Err(SandboxError::InputTooLarge(total_len));
        }
        if module.bytes.len() > p.max_module_bytes {
            return Err(SandboxError::ModuleTooLarge(module.bytes.len()));
        }
        // Re-hash: never trust a cached digest for the bytes we execute.
        let digest = arca_digest::sha256(&module.bytes);
        if digest != module.digest || !self.pins.iter().any(|pin| pin.digest == digest) {
            return Err(SandboxError::ModuleNotPinned(digest));
        }
        let input_len =
            i32::try_from(total_len).map_err(|_| SandboxError::InputTooLarge(total_len))?;

        let fuel = p.fuel.for_input(total_len);
        let mut meter = Meter::new(p, options, fuel, started);
        // A run that is already cancelled never pays for compilation.
        meter.check()?;

        let engine = self.engine();
        let wasm = Module::new(&engine, &*module.bytes)
            .map_err(|e| SandboxError::ModuleInvalid(e.to_string()))?;
        if let Some(import) = wasm.imports().next() {
            return Err(SandboxError::ForbiddenImport {
                module: import.module().to_owned(),
                name: import.name().to_owned(),
            });
        }
        check_exports(&wasm)?;
        meter.check()?;

        let guard = Guard {
            max_memory: p.memory_limit_for(total_len),
            max_table: p.max_table_elements,
            memory_denied: false,
            peak_memory: 0,
        };
        let mut store = Store::new(&engine, guard);
        store.limiter(|g| g as &mut dyn ResourceLimiter);
        meter.refill(&mut store, 0)?;

        let linker: Linker<Guard> = Linker::new(&engine);
        let instance = linker
            .instantiate_and_start(&mut store, &wasm)
            .map_err(|e| classify(&e, store.data(), fuel))?;
        let memory = instance
            .get_memory(&store, exports::MEMORY)
            .ok_or(SandboxError::MissingExport(exports::MEMORY))?;

        let guard = MemoryZeroizeGuard { memory, store: &mut store };
        let result = protocol(
            guard.store,
            &instance,
            guard.memory,
            (prefix, input),
            input_len,
            p,
            &mut meter,
        );
        drop(guard);
        result
    }
}

struct MemoryZeroizeGuard<'a> {
    memory: Memory,
    store: &'a mut Store<Guard>,
}

impl Drop for MemoryZeroizeGuard<'_> {
    fn drop(&mut self) {
        self.memory.data_mut(&mut *self.store).zeroize();
    }
}

fn check_exports(module: &Module) -> Result<(), SandboxError> {
    let want: [(&'static str, &[ValType], &[ValType]); 5] = [
        (exports::ABI_VERSION, &[], &[ValType::I32]),
        (exports::INPUT_RESERVE, &[ValType::I32], &[ValType::I32]),
        (exports::RUN, &[ValType::I32], &[ValType::I32]),
        (exports::OUTPUT_PTR, &[], &[ValType::I32]),
        (exports::OUTPUT_LEN, &[], &[ValType::I32]),
    ];
    let mut has_memory = false;
    for (name, params, results) in want {
        let export =
            module.exports().find(|e| e.name() == name).ok_or(SandboxError::MissingExport(name))?;
        match export.ty() {
            ExternType::Func(ft) if ft.params() == params && ft.results() == results => {}
            _ => return Err(SandboxError::ExportSignature(name)),
        }
    }
    for e in module.exports() {
        if e.name() == exports::MEMORY {
            if !matches!(e.ty(), ExternType::Memory(_)) {
                return Err(SandboxError::ExportSignature(exports::MEMORY));
            }
            has_memory = true;
        }
    }
    if has_memory { Ok(()) } else { Err(SandboxError::MissingExport(exports::MEMORY)) }
}

fn classify(e: &wasmi::Error, guard: &Guard, fuel: u64) -> SandboxError {
    if guard.memory_denied {
        return SandboxError::MemoryLimitExceeded;
    }
    match e.as_trap_code() {
        Some(TrapCode::OutOfFuel) => SandboxError::FuelExhausted { limit: fuel },
        Some(TrapCode::StackOverflow) => SandboxError::StackOverflow,
        Some(TrapCode::GrowthOperationLimited) => SandboxError::MemoryLimitExceeded,
        Some(code) => SandboxError::GuestTrap(code.to_string()),
        None => SandboxError::GuestTrap(e.to_string()),
    }
}

fn typed<P: wasmi::WasmParams, R: WasmResults>(
    store: &Store<Guard>,
    instance: &Instance,
    name: &'static str,
) -> Result<TypedFunc<P, R>, SandboxError> {
    instance.get_typed_func::<P, R>(store, name).map_err(|_| SandboxError::ExportSignature(name))
}

/// Runs one guest export to completion, refilling fuel slice by slice.
fn drive<R: WasmResults>(
    store: &mut Store<Guard>,
    meter: &mut Meter<'_>,
    start: impl FnOnce(&mut Store<Guard>) -> Result<TypedResumableCall<R>, wasmi::Error>,
) -> Result<R, SandboxError> {
    let mut step = start(store).map_err(|e| classify(&e, store.data(), meter.total))?;
    loop {
        match step {
            TypedResumableCall::Finished(result) => return Ok(result),
            // The sandbox links no host functions, so a host trap is impossible;
            // refuse to continue if it ever happens.
            TypedResumableCall::HostTrap(_) => {
                return Err(SandboxError::Internal(String::from("unexpected host trap")));
            }
            TypedResumableCall::OutOfFuel(pending) => {
                meter.refill(store, pending.required_fuel())?;
                step = pending
                    .resume(&mut *store)
                    .map_err(|e| classify(&e, store.data(), meter.total))?;
            }
        }
    }
}

fn protocol(
    store: &mut Store<Guard>,
    instance: &Instance,
    memory: Memory,
    (prefix, input): (&[u8], &[u8]),
    input_len: i32,
    policy: &SandboxPolicy,
    meter: &mut Meter<'_>,
) -> Result<GuestOutput, SandboxError> {
    let version: TypedFunc<(), i32> = typed(store, instance, exports::ABI_VERSION)?;
    let reserve: TypedFunc<i32, i32> = typed(store, instance, exports::INPUT_RESERVE)?;
    let run: TypedFunc<i32, i32> = typed(store, instance, exports::RUN)?;
    let out_ptr: TypedFunc<(), i32> = typed(store, instance, exports::OUTPUT_PTR)?;
    let out_len: TypedFunc<(), i32> = typed(store, instance, exports::OUTPUT_LEN)?;

    let v = drive(store, meter, |s| version.call_resumable(s, ()))?;
    if v != ABI_VERSION {
        return Err(SandboxError::AbiMismatch(v));
    }
    let ptr = drive(store, meter, |s| reserve.call_resumable(s, input_len))?;
    let ptr = usize::try_from(ptr).map_err(|_| SandboxError::GuestAllocationFailed)?;
    if ptr == 0 && input_len != 0 {
        return Err(SandboxError::GuestAllocationFailed);
    }
    memory.write(&mut *store, ptr, prefix).map_err(|_| SandboxError::GuestAllocationFailed)?;
    let input_at = ptr.checked_add(prefix.len()).ok_or(SandboxError::GuestAllocationFailed)?;
    memory.write(&mut *store, input_at, input).map_err(|_| SandboxError::GuestAllocationFailed)?;
    meter.check()?;

    let status = drive(store, meter, |s| run.call_resumable(s, input_len))?;

    let optr = drive(store, meter, |s| out_ptr.call_resumable(s, ()))?;
    let olen = drive(store, meter, |s| out_len.call_resumable(s, ()))?;
    let optr = usize::try_from(optr)
        .map_err(|_| SandboxError::OutputInvalid("negative output pointer"))?;
    let olen =
        usize::try_from(olen).map_err(|_| SandboxError::OutputInvalid("negative output length"))?;
    if olen > policy.max_output_bytes {
        return Err(SandboxError::OutputTooLarge(olen));
    }
    let end =
        optr.checked_add(olen).ok_or(SandboxError::OutputInvalid("output region overflows"))?;
    if end > memory.data_size(&*store) {
        return Err(SandboxError::OutputInvalid("output region outside guest memory"));
    }
    let mut out = Zeroizing::new(vec![0u8; olen]);
    memory
        .read(&*store, optr, &mut out)
        .map_err(|_| SandboxError::OutputInvalid("unreadable output region"))?;

    let (kind, payload) = envelope::open(&out)
        .map_err(|_| SandboxError::OutputInvalid("malformed result envelope"))?;
    let payload = Zeroizing::new(payload.to_vec());
    if !arca_abi::status::is_success(status) && kind != PayloadKind::Error {
        return Err(SandboxError::GuestStatus(status));
    }

    let stats = ExecutionStats {
        fuel_limit: meter.total,
        fuel_consumed: meter.used(store),
        peak_memory_bytes: store.data().peak_memory.max(memory.data_size(&*store)),
        slices: meter.slices,
        wall_time: meter.started.elapsed(),
    };
    Ok(GuestOutput { status, kind, payload, stats })
}
