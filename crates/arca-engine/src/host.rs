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
    /// Technical message (English), empty on success.
    pub message: String,
    /// The decoded result.
    pub data: Option<Output>,
    /// Resource accounting, when the guest ran.
    pub stats: Option<ExecutionStats>,
}

impl RunOutput {
    fn failure(status: Status, message: impl Into<String>) -> Self {
        Self { status, message: message.into(), data: None, stats: None }
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

    /// Sandbox policy for one operation on a document of `len` bytes.
    fn policy_for(&self, op: Op, len: usize) -> SandboxPolicy {
        let c = &self.config;
        let mut policy = SandboxPolicy {
            max_input_bytes: c.max_input_bytes.saturating_add(request::HEADER_LEN),
            max_memory_bytes: c.max_memory_bytes,
            max_wall_time: Some(match op {
                Op::Scan | Op::Sanitize => c.scan_timeout,
                Op::Preview => c.preview_timeout,
            }),
            // A sterile copy is about as large as the original.
            max_output_bytes: (64usize << 20).saturating_add(len),
            ..SandboxPolicy::default()
        };
        if matches!(op, Op::Preview | Op::Sanitize) {
            // Extraction and rebuilding keep the decompressed parts next to the result.
            policy.memory_base_bytes = 192 << 20;
            policy.memory_per_input_byte = 8;
        }
        policy
    }

    /// Runs `op` over `document` in the sandbox and decodes the result.
    #[must_use]
    pub fn run(&self, op: Op, op_flags: u32, document: &[u8], cancel: &CancelToken) -> RunOutput {
        if op_flags & !op.known_flags() != 0 {
            return RunOutput::failure(Status::BadArgument, "unknown flag bits");
        }
        let sandbox = Sandbox::new(self.policy_for(op, document.len()), [self.module.pin()]);
        let options = RunOptions::new().cancel_token(cancel.clone());
        let header = request::header(op, op_flags);
        let out = match sandbox.run_request(&self.module, &header, document, &options) {
            Ok(out) => out,
            Err(e) => return RunOutput::failure(classify(&e), e.to_string()),
        };
        let stats = Some(out.stats);
        if out.status != guest_status::OK {
            let message = match decode(out.kind, &out.payload) {
                Err(DecodeError::Guest(m)) => m,
                _ => format!("guest status {}", out.status),
            };
            return RunOutput { status: classify_guest(out.status), message, data: None, stats };
        }
        let expected = match op {
            Op::Scan => PayloadKind::XRayReport,
            Op::Sanitize => PayloadKind::SanitizedDocument,
            Op::Preview => PayloadKind::Preview,
        };
        if out.kind != expected {
            return RunOutput {
                status: Status::Fault,
                message: format!("the guest answered with {:?} instead of {expected:?}", out.kind),
                data: None,
                stats,
            };
        }
        match decode(out.kind, &out.payload) {
            Ok(data) => {
                RunOutput { status: Status::Ok, message: String::new(), data: Some(data), stats }
            }
            Err(e) => {
                RunOutput { status: Status::Fault, message: e.to_string(), data: None, stats }
            }
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
}

/// What a finished run keeps for the app to query.
#[derive(Debug)]
enum Body {
    None,
    Report(Box<XRayReport>),
    Preview(Box<PreviewIndex>),
    Sanitized { info: Vec<u8>, bytes: Zeroizing<Vec<u8>> },
}

#[derive(Debug)]
struct Outcome {
    op: Op,
    status: Status,
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
        })
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
        let out = self.engine.run(op, op_flags, &bytes, &token);
        let body = match out.data {
            Some(Output::Report(r)) => Body::Report(r),
            Some(Output::Preview(p)) => Body::Preview(Box::new(PreviewIndex::new(*p))),
            Some(Output::Sanitized(s)) => {
                let mut s: SanitizedDocument = *s;
                let info = render::sanitized_json(&s);
                // Move the sterile copy into a buffer that is wiped when it is released.
                let bytes = Zeroizing::new(core::mem::take(&mut s.bytes));
                Body::Sanitized { info, bytes }
            }
            None => Body::None,
        };
        Ok(self.outcomes.insert(Outcome {
            op,
            status: out.status,
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
}
