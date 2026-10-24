// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! The engine and the handle-based [`Host`] API the apps call.
//!
//! An app never touches document bytes or decoded structures directly. It owns
//! small integer *handles* — a document, a job (cancellation), a result — and
//! asks this module for JSON views. Handles are plain counters, never pointers:
//! a stale or forged handle is a [`Status::BadHandle`], not memory corruption.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use arca_abi::envelope::PayloadKind;
use arca_abi::request::{self, Op};
use arca_abi::status as guest_status;
use arca_guest_xray::{DecodeError, Output, decode};
use arca_sandbox::{
    CancelToken, ExecutionStats, GuestModule, RunOptions, Sandbox, SandboxError, SandboxPolicy,
};
use arca_xray::XRayReport;
use arca_xray::sanitize::SanitizedDocument;
use zeroize::Zeroizing;

use crate::json::JsonWriter;
use crate::render::{self, PreviewIndex};

/// An opaque, never-reused identifier of a document, job or result.
pub type Handle = u64;

/// The outcome classes the apps distinguish. The numeric values are part of the
/// contract with the Java side (`Native.java`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum Status {
    /// Success.
    Ok = 0,
    /// The user abandoned the run.
    Cancelled = 1,
    /// The wall-clock deadline passed.
    Timeout = 2,
    /// The document is larger than the configured limit.
    TooLarge = 3,
    /// The document is damaged, hostile or beyond a safety limit.
    Rejected = 4,
    /// There is no such operation for this format (for example no preview of a PDF yet).
    Unsupported = 5,
    /// The document is password protected.
    Encrypted = 6,
    /// The operation failed after the document was accepted (a sterile copy could not
    /// be produced or verified).
    Failed = 7,
    /// The guest used up its CPU, memory or stack budget: treated as a decompression
    /// bomb or similar attack.
    ResourceLimit = 8,
    /// A fault of the engine itself (sandbox, module, wire); never caused by a mere
    /// invalid document.
    Fault = 9,
    /// The handle does not name a live object.
    BadHandle = 10,
    /// Reading or writing a file descriptor failed.
    Io = 11,
    /// An argument is outside its range.
    BadArgument = 12,
    /// The engine has not been initialised.
    NotReady = 13,
    /// The document is encrypted and the password does not open it.
    WrongPassword = 14,
}

impl Status {
    /// The numeric code.
    #[must_use]
    pub const fn code(self) -> i32 {
        self as i32
    }
}

/// Limits the app configures from what the device offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    /// Largest document accepted.
    pub max_input_bytes: usize,
    /// Ceiling of the guest's linear memory.
    pub max_memory_bytes: usize,
    /// Wall-clock limit of an X-ray or a sterilisation.
    pub scan_timeout: Duration,
    /// Wall-clock limit of a preview.
    pub preview_timeout: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_input_bytes: 256 << 20,
            max_memory_bytes: 768 << 20,
            scan_timeout: Duration::from_secs(120),
            preview_timeout: Duration::from_secs(180),
        }
    }
}

impl EngineConfig {
    /// The limits cannot be set to values that would defeat the sandbox.
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            max_input_bytes: self.max_input_bytes.clamp(1 << 20, 512 << 20),
            max_memory_bytes: self.max_memory_bytes.clamp(160 << 20, 1 << 30),
            scan_timeout: self.scan_timeout.clamp(Duration::from_secs(5), Duration::from_secs(600)),
            preview_timeout: self
                .preview_timeout
                .clamp(Duration::from_secs(5), Duration::from_secs(900)),
        }
    }
}

/// The sandbox plus the pinned guest module.
#[derive(Debug, Clone)]
pub struct Engine {
    module: GuestModule,
    config: EngineConfig,
}

/// A decoded, typed guest result or the reason there is none.
#[derive(Debug)]
pub struct RunOutput {
    /// Classification of the outcome.
    pub status: Status,
    /// Whether the document had to be decrypted to be read.
    pub unlock: Unlock,
    /// Technical message (English), empty on success.
    pub message: String,
    /// The decoded result.
    pub data: Option<Output>,
    /// Resource accounting, when the guest ran.
    pub stats: Option<ExecutionStats>,
}

/// How an encrypted document was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Unlock {
    /// The document was not encrypted.
    Not = 0,
    /// The password the reader gave opened it.
    Password = 1,
    /// It was locked but needed no password (a PDF that only restricts editing).
    NoPassword = 2,
}

impl RunOutput {
    fn failure(status: Status, message: impl Into<String>) -> Self {
        Self { status, unlock: Unlock::Not, message: message.into(), data: None, stats: None }
    }
}

fn classify(error: &SandboxError) -> Status {
    match error {
        SandboxError::InputTooLarge(_) => Status::TooLarge,
        SandboxError::Cancelled => Status::Cancelled,
        SandboxError::DeadlineExceeded { .. } => Status::Timeout,
        SandboxError::FuelExhausted { .. }
        | SandboxError::MemoryLimitExceeded
        | SandboxError::StackOverflow
        | SandboxError::OutputTooLarge(_) => Status::ResourceLimit,
        SandboxError::ModuleTooLarge(_)
        | SandboxError::ModuleNotPinned(_)
        | SandboxError::ModuleInvalid(_)
        | SandboxError::ForbiddenImport { .. }
        | SandboxError::MissingExport(_)
        | SandboxError::ExportSignature(_)
        | SandboxError::AbiMismatch(_)
        | SandboxError::GuestTrap(_)
        | SandboxError::GuestAllocationFailed
        | SandboxError::GuestStatus(_)
        | SandboxError::OutputInvalid(_)
        | SandboxError::WorkerSpawn(_)
        | SandboxError::WorkerPanicked
        | SandboxError::Internal(_) => Status::Fault,
    }
}

fn classify_guest(code: i32) -> Status {
    match code {
        guest_status::DOCUMENT_REJECTED => Status::Rejected,
        guest_status::UNSUPPORTED_FORMAT => Status::Unsupported,
        guest_status::OPERATION_FAILED => Status::Failed,
        guest_status::ENCRYPTED => Status::Encrypted,
        guest_status::WRONG_PASSWORD => Status::WrongPassword,
        _ => Status::Fault,
    }
}

impl Engine {
    /// An engine over the guest module `wasm` (checked against its own digest, which the
    /// sandbox re-verifies before every run).
    #[must_use]
    pub fn new(wasm: impl Into<Arc<[u8]>>, config: EngineConfig) -> Self {
        Self { module: GuestModule::new("arca-guest-xray", wasm), config: config.clamped() }
    }

    /// SHA-256 of the guest module, lowercase hex.
    #[must_use]
    pub fn guest_sha256(&self) -> String {
        self.module.digest().to_string()
    }

    /// The configuration in force.
    #[must_use]
    pub const fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Sandbox policy for one operation on a document of `len` bytes that an earlier scan found
    /// to expand to `inflated` bytes (`0` if unknown).
    ///
    /// A preview or a sterile copy touches everything the scan unpacked again, so a large,
    /// well-compressed workbook needs time and memory in proportion to what it *expands to*,
    /// not to its size on disk. The scan's own figure is used to scale both; it comes out of the
    /// sandbox, so it cannot raise anything above the fixed ceilings (fuel and the device's
    /// memory limit), and the wall-clock limits stay what they were.
    pub(crate) fn policy_for(&self, op: Op, len: usize, inflated: u64) -> SandboxPolicy {
        let c = &self.config;
        let mut policy = SandboxPolicy {
            max_input_bytes: c
                .max_input_bytes
                .saturating_add(request::HEADER_LEN + request::MAX_PASSWORD),
            max_memory_bytes: c.max_memory_bytes,
            max_wall_time: Some(match op {
                Op::Scan | Op::Sanitize => c.scan_timeout,
                Op::Preview | Op::Render | Op::Image => c.preview_timeout,
            }),
            // A sterile copy is about as large as the original.
            max_output_bytes: (64usize << 20).saturating_add(len),
            ..SandboxPolicy::default()
        };
        if op == Op::Image {
            // A picture costs by its pixels, not by the bytes of its file: a flat 60-megapixel PNG is
            // a few hundred kilobytes. The wall-clock limit still bounds the time.
            policy.memory_base_bytes = 256 << 20;
            policy.fuel.base = policy.fuel.base.max(IMAGE_FUEL_BASE);
        }
        let len_u64 = u64::try_from(len).unwrap_or(u64::MAX);
        let inflated = inflated.min(MAX_INFLATED_HINT);

        if op == Op::Scan {
            // If the document is highly compressed (> 20:1 ratio), scale fuel and memory for the
            // excess uncompressed content that the XML parser must process.
            let covered_by_base_ratio = len_u64.saturating_mul(DEFAULT_MAX_RATIO);
            if inflated > covered_by_base_ratio {
                let excess = inflated.saturating_sub(covered_by_base_ratio);
                policy.fuel.base = policy.fuel.base.saturating_add(excess.saturating_mul(FUEL_PER_INFLATED_SCAN));
                let excess_mem = usize::try_from(excess.min(256 << 20)).unwrap_or(usize::MAX);
                policy.memory_base_bytes = policy.memory_base_bytes.saturating_add(excess_mem);
            }
        } else if matches!(op, Op::Preview | Op::Sanitize | Op::Render) {
            // Extraction and rebuilding keep the decompressed parts next to the result.
            policy.memory_base_bytes = 192 << 20;
            policy.memory_per_input_byte = 8;
            let (fuel, memory) = match op {
                Op::Sanitize => (FUEL_PER_INFLATED_SANITIZE, MEMORY_PER_INFLATED_SANITIZE),
                _ => (FUEL_PER_INFLATED_PREVIEW, MEMORY_PER_INFLATED_PREVIEW),
            };
            policy.fuel.base = policy.fuel.base.saturating_add(inflated.saturating_mul(fuel));
            policy.memory_base_bytes = policy.memory_base_bytes.saturating_add(
                usize::try_from(inflated).unwrap_or(usize::MAX).saturating_mul(memory),
            );
        }

        // Dynamically scale policy.fuel.max so that large documents (10-30 MB) and high-inflation
        // documents are not clamped to a static ceiling.
        let expected_fuel = policy.fuel.base.saturating_add(policy.fuel.per_input_byte.saturating_mul(len_u64));
        let adaptive_max = expected_fuel.saturating_mul(12) / 10;
        policy.fuel.max = policy.fuel.max.max(adaptive_max).min(MAX_FUEL_CEILING);

        policy
    }

    /// A sterile copy of a picture decodes it, writes it again and reads that back, all of it
    /// interpreted: the cost is in its pixels, not in the bytes of its file, and a full-size copy
    /// of a photograph takes minutes on a phone. Only a file that is a picture by its first bytes
    /// gets the larger budget, and the time limit still bounds it.
    pub(crate) fn size_for_picture_copy(
        &self,
        policy: &mut SandboxPolicy,
        op: Op,
        op_flags: u32,
        document: &[u8],
    ) {
        if op != Op::Sanitize || arca_image::sniff(document).is_none() {
            return;
        }
        let full = op_flags & request::flags::FULL_SIZE != 0;
        policy.memory_base_bytes = policy.memory_base_bytes.max(256 << 20);
        let (fuel, time) = if full {
            (
                FULL_COPY_FUEL,
                self.config.preview_timeout.saturating_mul(3).min(Duration::from_secs(900)),
            )
        } else {
            (COPY_FUEL, self.config.preview_timeout)
        };
        policy.fuel.base = policy.fuel.base.max(fuel);
        policy.max_wall_time = Some(time);
    }

    /// Runs `op` over `document` in the sandbox and decodes the result.
    #[must_use]
    pub fn run(
        &self,
        op: Op,
        op_flags: u32,
        password: &[u8],
        document: &[u8],
        cancel: &CancelToken,
    ) -> RunOutput {
        self.run_scaled(op, op_flags, password, document, cancel, 0)
    }

    /// As [`Engine::run`], for a document an earlier scan found to expand to `inflated` bytes:
    /// the budget of a preview or a sterile copy grows with it (see `policy_for`).
    #[must_use]
    pub fn run_scaled(
        &self,
        op: Op,
        op_flags: u32,
        password: &[u8],
        document: &[u8],
        cancel: &CancelToken,
        inflated: u64,
    ) -> RunOutput {
        if op_flags & !op.known_flags() != 0 {
            return RunOutput::failure(Status::BadArgument, "unknown flag bits");
        }
        let mut policy = self.policy_for(op, document.len(), inflated);
        self.size_for_picture_copy(&mut policy, op, op_flags, document);
        let sandbox = Sandbox::new(policy, [self.module.pin()]);
        let options = RunOptions::new().cancel_token(cancel.clone());
        let Some(prefix) = request::prefix(op, op_flags, password) else {
            return RunOutput::failure(Status::BadArgument, "the password is too long");
        };
        let out = match sandbox.run_request(&self.module, &prefix, document, &options) {
            Ok(out) => out,
            Err(e) => return RunOutput::failure(classify(&e), e.to_string()),
        };
        let stats = Some(out.stats);
        let unlock = match out.status {
            guest_status::OK_DECRYPTED => Unlock::Password,
            guest_status::OK_UNLOCKED => Unlock::NoPassword,
            _ => Unlock::Not,
        };
        if !guest_status::is_success(out.status) {
            let message = match decode(out.kind, &out.payload) {
                Err(DecodeError::Guest(m)) => m,
                _ => format!("guest status {}", out.status),
            };
            return RunOutput {
                status: classify_guest(out.status),
                unlock,
                message,
                data: None,
                stats,
            };
        }
        let expected = match op {
            Op::Scan => PayloadKind::XRayReport,
            Op::Sanitize => PayloadKind::SanitizedDocument,
            Op::Preview => PayloadKind::Preview,
            Op::Render => PayloadKind::Page,
            Op::Image => PayloadKind::Image,
        };
        if out.kind != expected {
            return RunOutput {
                status: Status::Fault,
                unlock,
                message: format!("the guest answered with {:?} instead of {expected:?}", out.kind),
                data: None,
                stats,
            };
        }
        match decode(out.kind, &out.payload) {
            Ok(data) => RunOutput {
                status: Status::Ok,
                unlock,
                message: String::new(),
                data: Some(data),
                stats,
            },
            Err(e) => RunOutput {
                status: Status::Fault,
                unlock,
                message: e.to_string(),
                data: None,
                stats,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Handle tables
// ---------------------------------------------------------------------------

/// Handles are never reused within a process.
#[derive(Debug)]
struct Table<T> {
    next: AtomicU64,
    items: Mutex<HashMap<Handle, Arc<T>>>,
}

impl<T> Table<T> {
    fn new() -> Self {
        Self { next: AtomicU64::new(1), items: Mutex::new(HashMap::new()) }
    }

    fn insert(&self, value: T) -> Handle {
        let handle = self.next.fetch_add(1, Ordering::Relaxed);
        self.items.lock().unwrap_or_else(PoisonError::into_inner).insert(handle, Arc::new(value));
        handle
    }

    fn get(&self, handle: Handle) -> Option<Arc<T>> {
        self.items.lock().unwrap_or_else(PoisonError::into_inner).get(&handle).cloned()
    }

    fn remove(&self, handle: Handle) -> bool {
        self.items.lock().unwrap_or_else(PoisonError::into_inner).remove(&handle).is_some()
    }

    fn len(&self) -> usize {
        self.items.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}

/// A document being loaded, then sealed.
#[derive(Debug)]
enum DocState {
    Loading(Zeroizing<Vec<u8>>),
    Sealed(Arc<Zeroizing<Vec<u8>>>),
}

#[derive(Debug)]
struct Document {
    limit: usize,
    state: Mutex<DocState>,
    /// What the reader typed for an encrypted document; wiped when the document goes.
    password: Mutex<Zeroizing<Vec<u8>>>,
    /// Bytes the last scan found the document to expand to (`0` before any scan).
    inflated: AtomicU64,
}

/// What a finished run keeps for the app to query.
#[derive(Debug)]
enum Body {
    None,
    Report(Box<XRayReport>),
    Preview(Box<PreviewIndex>),
    Sanitized {
        info: Vec<u8>,
        bytes: Zeroizing<Vec<u8>>,
    },
    /// The drawing of a page, as JSON.
    Page(Vec<u8>),
    /// A picture: facts as JSON, and the pixels (premultiplied RGBA, wiped on release).
    Image {
        info: Vec<u8>,
        pixels: Zeroizing<Vec<u8>>,
    },
}

#[derive(Debug)]
struct Outcome {
    op: Op,
    status: Status,
    unlock: Unlock,
    message: String,
    stats: Option<ExecutionStats>,
    body: Body,
}

/// The engine as the apps see it: handles in, JSON and bytes out.
#[derive(Debug)]
pub struct Host {
    engine: Engine,
    docs: Table<Document>,
    jobs: Table<CancelToken>,
    outcomes: Table<Outcome>,
}

/// Size of the chunks used when reading a file descriptor.
const READ_CHUNK: usize = 1 << 20;

/// Fuel a preview needs per byte the document expands to (measured ≈ 700 on spreadsheets; with
/// a margin).
const FUEL_PER_INFLATED_PREVIEW: u64 = 1_500;
/// Fuel a sterile copy needs per expanded byte (measured ≈ 4 700; with a margin).
const FUEL_PER_INFLATED_SANITIZE: u64 = 6_000;
/// Memory a preview needs per expanded byte.
const MEMORY_PER_INFLATED_PREVIEW: usize = 2;
/// Memory a sterile copy needs per expanded byte (the parts are held next to the rebuilt copy).
const MEMORY_PER_INFLATED_SANITIZE: usize = 4;
/// The scan's figure is used up to this much (the engine's own ceilings apply on top).
const MAX_INFLATED_HINT: u64 = 1 << 30;
/// Fuel an X-ray scan needs per byte of excess decompressed content above 20:1 compression ratio
/// (measured ≈ 1 060 - 1 400 on XML; with safety margin).
const FUEL_PER_INFLATED_SCAN: u64 = 1_500;
/// Ratio up to which the standard per_input_byte budget (32 000) provides sufficient fuel.
const DEFAULT_MAX_RATIO: u64 = 20;
/// Absolute maximum fuel ceiling for any single run (8 trillion fuel ≈ 1 GiB inflated at 6 000 fuel/B).
const MAX_FUEL_CEILING: u64 = 8_000_000_000_000;

/// Sniffs the declared uncompressed size of a container from its central directory without
/// decompressing any data. Returns `None` if the input is not a recognized container.
pub(crate) fn sniff_inflated(data: &[u8]) -> Option<u64> {
    if !data.starts_with(b"PK\x03\x04") && !data.starts_with(b"PK\x05\x06") {
        return None;
    }
    let zip = arca_container::zip::ZipArchive::parse(data, arca_container::zip::ZipLimits::default()).ok()?;
    let mut total: u64 = 0;
    for entry in zip.entries() {
        if !entry.is_dir() {
            total = total.saturating_add(entry.uncompressed_size());
        }
    }
    // Bound the hint by 100:1 ratio and 1 GiB maximum, matching InflateBudget limits.
    let max_allowed = (u64::try_from(data.len()).unwrap_or(u64::MAX))
        .saturating_mul(100)
        .min(MAX_INFLATED_HINT);
    Some(total.min(max_allowed))
}
/// Fuel a picture may use before the size of its file adds to it.
const IMAGE_FUEL_BASE: u64 = 20_000_000_000;
/// Fuel a sterile copy of a picture may use (at most 4 megapixels).
const COPY_FUEL: u64 = 100_000_000_000;
/// Fuel a full-size sterile copy may use (at most 16 megapixels).
const FULL_COPY_FUEL: u64 = 250_000_000_000;

impl Host {
    /// A host around `engine`.
    #[must_use]
    pub fn new(engine: Engine) -> Self {
        Self { engine, docs: Table::new(), jobs: Table::new(), outcomes: Table::new() }
    }

    /// The engine.
    #[must_use]
    pub const fn engine(&self) -> &Engine {
        &self.engine
    }

    // ----- documents ---------------------------------------------------------

    /// Starts an empty document that accepts at most `max_bytes` (and never more than the
    /// engine's own limit).
    pub fn doc_new(&self, max_bytes: u64) -> Handle {
        let own = u64::try_from(self.engine.config.max_input_bytes).unwrap_or(u64::MAX);
        let limit = usize::try_from(max_bytes.min(own)).unwrap_or(usize::MAX);
        self.docs.insert(Document {
            limit,
            state: Mutex::new(DocState::Loading(Zeroizing::new(Vec::new()))),
            password: Mutex::new(Zeroizing::new(Vec::new())),
            inflated: AtomicU64::new(0),
        })
    }

    /// Remembers the password for the next runs over the document (UTF-8; empty forgets it).
    ///
    /// # Errors
    /// [`Status::BadHandle`], [`Status::BadArgument`] for a password longer than the protocol
    /// allows.
    pub fn doc_set_password(&self, doc: Handle, password: &[u8]) -> Result<(), Status> {
        if password.len() > request::MAX_PASSWORD {
            return Err(Status::BadArgument);
        }
        let d = self.docs.get(doc).ok_or(Status::BadHandle)?;
        let mut slot = d.password.lock().unwrap_or_else(PoisonError::into_inner);
        *slot = Zeroizing::new(password.to_vec());
        Ok(())
    }

    /// Appends `chunk`.
    ///
    /// # Errors
    /// [`Status::TooLarge`] past the limit, [`Status::BadHandle`], or [`Status::BadArgument`]
    /// for a document that is already sealed.
    pub fn doc_append(&self, doc: Handle, chunk: &[u8]) -> Result<(), Status> {
        let d = self.docs.get(doc).ok_or(Status::BadHandle)?;
        let mut state = d.state.lock().unwrap_or_else(PoisonError::into_inner);
        let DocState::Loading(buf) = &mut *state else { return Err(Status::BadArgument) };
        if buf.len().saturating_add(chunk.len()) > d.limit {
            return Err(Status::TooLarge);
        }
        buf.try_reserve(chunk.len()).map_err(|_| Status::TooLarge)?;
        buf.extend_from_slice(chunk);
        Ok(())
    }

    /// Reads `reader` to its end into the document and returns the number of bytes read.
    ///
    /// # Errors
    /// [`Status::Io`] if reading fails, [`Status::TooLarge`] past the limit.
    pub fn doc_read_from(&self, doc: Handle, reader: &mut dyn Read) -> Result<u64, Status> {
        let mut chunk = vec![0u8; READ_CHUNK];
        let mut total = 0u64;
        loop {
            let n = match reader.read(&mut chunk) {
                Ok(0) => return Ok(total),
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(Status::Io),
            };
            let part = chunk.get(..n).ok_or(Status::Io)?;
            self.doc_append(doc, part)?;
            total = total.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        }
    }

    /// Number of bytes loaded so far.
    ///
    /// # Errors
    /// [`Status::BadHandle`].
    pub fn doc_len(&self, doc: Handle) -> Result<u64, Status> {
        let d = self.docs.get(doc).ok_or(Status::BadHandle)?;
        let state = d.state.lock().unwrap_or_else(PoisonError::into_inner);
        let n = match &*state {
            DocState::Loading(b) => b.len(),
            DocState::Sealed(b) => b.len(),
        };
        Ok(u64::try_from(n).unwrap_or(u64::MAX))
    }

    /// Forgets the document; a running job keeps its own reference until it ends. The bytes
    /// are wiped when the last reference goes.
    pub fn doc_free(&self, doc: Handle) -> bool {
        self.docs.remove(doc)
    }

    fn seal(&self, doc: Handle) -> Result<Arc<Zeroizing<Vec<u8>>>, Status> {
        let d = self.docs.get(doc).ok_or(Status::BadHandle)?;
        let mut state = d.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let DocState::Loading(buf) = &mut *state {
            let bytes = core::mem::take(buf);
            *state = DocState::Sealed(Arc::new(bytes));
        }
        match &*state {
            DocState::Sealed(b) => Ok(Arc::clone(b)),
            DocState::Loading(_) => Err(Status::Fault),
        }
    }

    // ----- jobs ----------------------------------------------------------------

    /// A new cancellation token.
    pub fn job_new(&self) -> Handle {
        self.jobs.insert(CancelToken::new())
    }

    /// Asks the job's run to stop at its next slice boundary (milliseconds).
    pub fn job_cancel(&self, job: Handle) -> bool {
        self.jobs.get(job).is_some_and(|t| {
            t.cancel();
            true
        })
    }

    /// Releases the job.
    pub fn job_free(&self, job: Handle) -> bool {
        self.jobs.remove(job)
    }

    // ----- running -------------------------------------------------------------

    /// Runs `op` over a document and stores the outcome. **Blocking**: the app calls this
    /// from a worker thread. The returned handle names the outcome even when it failed;
    /// its [`Host::outcome_status`] tells which.
    ///
    /// # Errors
    /// [`Status::BadHandle`] / [`Status::BadArgument`] when the arguments are invalid (no
    /// outcome is stored then).
    pub fn run(&self, job: Handle, doc: Handle, op: u16, op_flags: u32) -> Result<Handle, Status> {
        let token = self.jobs.get(job).ok_or(Status::BadHandle)?;
        let op = Op::from_code(op).ok_or(Status::BadArgument)?;
        if op_flags & !op.known_flags() != 0 {
            return Err(Status::BadArgument);
        }
        let bytes = self.seal(doc)?;
        let password = {
            let d = self.docs.get(doc).ok_or(Status::BadHandle)?;
            let p = d.password.lock().unwrap_or_else(PoisonError::into_inner);
            Zeroizing::new(p.to_vec())
        };
        let hint = {
            let stored = self.docs.get(doc).map_or(0, |d| d.inflated.load(Ordering::Relaxed));
            if stored > 0 {
                stored
            } else {
                sniff_inflated(&bytes).unwrap_or(0)
            }
        };
        let out = self.engine.run_scaled(op, op_flags, &password, &bytes, &token, hint);
        let body = match out.data {
            Some(Output::Report(r)) => {
                if let Some(d) = self.docs.get(doc) {
                    d.inflated.store(r.stats.bytes_inflated, Ordering::Relaxed);
                }
                Body::Report(r)
            }
            Some(Output::Preview(p)) => Body::Preview(Box::new(PreviewIndex::new(*p))),
            Some(Output::Sanitized(s)) => {
                let mut s: SanitizedDocument = *s;
                let info = render::sanitized_json(&s);
                // Move the sterile copy into a buffer that is wiped when it is released.
                let bytes = Zeroizing::new(core::mem::take(&mut s.bytes));
                Body::Sanitized { info, bytes }
            }
            Some(Output::Page(p)) => Body::Page(render::page_json(&p)),
            Some(Output::Image(i)) => {
                let info = render::image_json(&i);
                Body::Image { info, pixels: Zeroizing::new(render::premultiplied(i.rgba)) }
            }
            None => Body::None,
        };
        Ok(self.outcomes.insert(Outcome {
            op,
            status: out.status,
            unlock: out.unlock,
            message: out.message,
            stats: out.stats,
            body,
        }))
    }

    fn outcome(&self, outcome: Handle) -> Result<Arc<Outcome>, Status> {
        self.outcomes.get(outcome).ok_or(Status::BadHandle)
    }

    /// The status of a finished run.
    ///
    /// # Errors
    /// [`Status::BadHandle`].
    pub fn outcome_status(&self, outcome: Handle) -> Result<Status, Status> {
        Ok(self.outcome(outcome)?.status)
    }

    /// Diagnostics of a finished run as JSON: status, technical message and resource use.
    ///
    /// # Errors
    /// [`Status::BadHandle`].
    pub fn outcome_info_json(&self, outcome: Handle) -> Result<Vec<u8>, Status> {
        let o = self.outcome(outcome)?;
        let mut w = JsonWriter::new();
        w.begin_object();
        w.field_u64("status", u64::try_from(o.status.code()).unwrap_or(0));
        w.field_name("statusName", &o.status);
        w.field_u64("op", u64::from(o.op.code()));
        w.field_u64("unlock", u64::from(o.unlock as u8));
        w.field_str("message", &o.message);
        if let Some(s) = &o.stats {
            w.key("stats");
            w.begin_object();
            w.field_u64("fuel", s.fuel_consumed);
            w.field_u64("fuelLimit", s.fuel_limit);
            w.field_u64("peakMemory", u64::try_from(s.peak_memory_bytes).unwrap_or(u64::MAX));
            w.field_u64("slices", s.slices);
            w.field_u64("wallMs", u64::try_from(s.wall_time.as_millis()).unwrap_or(u64::MAX));
            w.end_object();
        }
        w.end_object();
        Ok(w.finish())
    }

    /// The drawing of a page (from a [`op::RENDER`] run) as JSON; see [`render::page_json`].
    ///
    /// # Errors
    /// [`Status::BadHandle`], or [`Status::BadArgument`] when the outcome holds no page.
    pub fn page_json(&self, outcome: Handle) -> Result<Vec<u8>, Status> {
        match &self.outcome(outcome)?.body {
            Body::Page(json) => Ok(json.clone()),
            _ => Err(Status::BadArgument),
        }
    }

    /// Facts about a picture (from an [`op::IMAGE`] run) as JSON; see [`render::image_json`].
    ///
    /// # Errors
    /// [`Status::BadHandle`], or [`Status::BadArgument`] when the outcome holds no picture.
    pub fn image_info_json(&self, outcome: Handle) -> Result<Vec<u8>, Status> {
        match &self.outcome(outcome)?.body {
            Body::Image { info, .. } => Ok(info.clone()),
            _ => Err(Status::BadArgument),
        }
    }

    /// The pixels of a picture: `width × height` pixels of four bytes (red, green, blue, alpha;
    /// colours multiplied by the alpha, as Android keeps them).
    ///
    /// # Errors
    /// As [`Host::image_info_json`].
    pub fn image_pixels(&self, outcome: Handle) -> Result<Vec<u8>, Status> {
        match &self.outcome(outcome)?.body {
            Body::Image { pixels, .. } => Ok(pixels.to_vec()),
            _ => Err(Status::BadArgument),
        }
    }

    /// The X-ray report as JSON.
    ///
    /// # Errors
    /// [`Status::BadHandle`], or [`Status::BadArgument`] when the outcome holds no report.
    pub fn report_json(&self, outcome: Handle) -> Result<Vec<u8>, Status> {
        match &self.outcome(outcome)?.body {
            Body::Report(r) => Ok(render::report_json(r)),
            _ => Err(Status::BadArgument),
        }
    }

    /// Sections, notices and totals of a preview as JSON.
    ///
    /// # Errors
    /// As [`Host::report_json`].
    pub fn preview_meta_json(&self, outcome: Handle) -> Result<Vec<u8>, Status> {
        match &self.outcome(outcome)?.body {
            Body::Preview(p) => Ok(p.meta_json()),
            _ => Err(Status::BadArgument),
        }
    }

    /// A page of preview items as a JSON array.
    ///
    /// # Errors
    /// As [`Host::report_json`]; [`Status::BadArgument`] also for an unknown section.
    pub fn preview_items_json(
        &self,
        outcome: Handle,
        section: u32,
        from: u64,
        count: u32,
    ) -> Result<Vec<u8>, Status> {
        match &self.outcome(outcome)?.body {
            Body::Preview(p) => {
                let section = usize::try_from(section).map_err(|_| Status::BadArgument)?;
                // A page is bounded whatever the app asks for.
                let count = usize::try_from(count).unwrap_or(usize::MAX).min(512);
                p.items_json(section, from, count).ok_or(Status::BadArgument)
            }
            _ => Err(Status::BadArgument),
        }
    }

    /// Description of a sterile copy as JSON.
    ///
    /// # Errors
    /// As [`Host::report_json`].
    pub fn sanitized_info_json(&self, outcome: Handle) -> Result<Vec<u8>, Status> {
        match &self.outcome(outcome)?.body {
            Body::Sanitized { info, .. } => Ok(info.clone()),
            _ => Err(Status::BadArgument),
        }
    }

    /// Size of the sterile copy.
    ///
    /// # Errors
    /// As [`Host::report_json`].
    pub fn sanitized_len(&self, outcome: Handle) -> Result<u64, Status> {
        match &self.outcome(outcome)?.body {
            Body::Sanitized { bytes, .. } => Ok(u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
            _ => Err(Status::BadArgument),
        }
    }

    /// Writes the sterile copy to `out` (a file chosen by the user) and returns its size.
    ///
    /// # Errors
    /// As [`Host::report_json`]; [`Status::Io`] if writing fails.
    pub fn sanitized_write_to(&self, outcome: Handle, out: &mut dyn Write) -> Result<u64, Status> {
        match &self.outcome(outcome)?.body {
            Body::Sanitized { bytes, .. } => {
                out.write_all(bytes).map_err(|_| Status::Io)?;
                out.flush().map_err(|_| Status::Io)?;
                Ok(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            }
            _ => Err(Status::BadArgument),
        }
    }

    /// Releases a stored outcome (a sterile copy is wiped).
    pub fn outcome_free(&self, outcome: Handle) -> bool {
        self.outcomes.remove(outcome)
    }

    /// Number of live documents, jobs and outcomes (for leak tests).
    #[must_use]
    pub fn live_objects(&self) -> (usize, usize, usize) {
        (self.docs.len(), self.jobs.len(), self.outcomes.len())
    }

    /// Facts about this build as JSON.
    #[must_use]
    pub fn info_json(&self) -> Vec<u8> {
        let mut w = JsonWriter::new();
        w.begin_object();
        w.field_str("engine", env!("CARGO_PKG_VERSION"));
        w.field_u64("abi", u64::try_from(arca_abi::ABI_VERSION).unwrap_or(0));
        w.field_str("guestSha256", &self.engine.guest_sha256());
        w.field_str("arch", std::env::consts::ARCH);
        w.field_str("os", std::env::consts::OS);
        w.field_str("appName", &arca_attribution::app_name());
        w.field_u64(
            "maxInputBytes",
            u64::try_from(self.engine.config.max_input_bytes).unwrap_or(0),
        );
        w.field_u64(
            "maxMemoryBytes",
            u64::try_from(self.engine.config.max_memory_bytes).unwrap_or(0),
        );
        w.end_object();
        w.finish()
    }
}

/// SHA-256 of a guest module, lowercase hex (the value the sandbox pins and the apps show).
#[must_use]
pub fn guest_digest_of(bytes: &[u8]) -> String {
    arca_digest::sha256(bytes).to_string()
}

/// Request flag constants re-exported for the JNI layer.
pub mod op {
    pub use arca_abi::request::flags::{
        ACCEPT_REVISIONS, REMOVE_COMMENTS, REMOVE_CUSTOM_XML, REVEAL_HIDDEN, STRIP_METADATA,
    };
    /// Operation codes.
    pub const SCAN: u16 = 1;
    /// Sterilise.
    pub const SANITIZE: u16 = 2;
    /// Preview.
    pub const PREVIEW: u16 = 3;
    /// The drawing of one page; the page number (from 0) goes in the flags.
    pub const RENDER: u16 = 4;
    /// One picture of the document; its id goes in the flags.
    pub const IMAGE: u16 = 5;
}
