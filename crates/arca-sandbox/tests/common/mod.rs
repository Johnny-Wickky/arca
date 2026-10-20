//! A tiny WebAssembly binary encoder for adversarial guest modules.
//!
//! Hand-encoding keeps the test suite free of a WAT toolchain and makes
//! every byte of every hostile module explicit.

#![allow(dead_code, reason = "each test binary uses a different subset")]
#![allow(
    clippy::unwrap_used,
    reason = "test fixture builder; inputs are small compile-time constants"
)]

pub const I32: u8 = 0x7F;

/// The ABI version the host expects (test guests announce it).
pub const ABI: i32 = arca_abi::ABI_VERSION;

fn uleb(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = u8::try_from(v & 0x7F).unwrap();
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn sleb(mut v: i64, out: &mut Vec<u8>) {
    loop {
        let byte = u8::try_from(v & 0x7F).unwrap();
        v >>= 7;
        let done = (v == 0 && byte & 0x40 == 0) || (v == -1 && byte & 0x40 != 0);
        out.push(if done { byte } else { byte | 0x80 });
        if done {
            return;
        }
    }
}

fn name(s: &str, out: &mut Vec<u8>) {
    uleb(s.len() as u64, out);
    out.extend_from_slice(s.as_bytes());
}

/// Instruction helpers.
pub mod op {
    pub fn i32_const(v: i32) -> Vec<u8> {
        let mut o = vec![0x41];
        super::sleb(i64::from(v), &mut o);
        o
    }
    pub fn local_get(i: u32) -> Vec<u8> {
        let mut o = vec![0x20];
        super::uleb(u64::from(i), &mut o);
        o
    }
    pub fn call(f: u32) -> Vec<u8> {
        let mut o = vec![0x10];
        super::uleb(u64::from(f), &mut o);
        o
    }
    /// `i32.store offset=0 align=2`
    pub const I32_STORE: [u8; 3] = [0x36, 0x02, 0x00];
    /// `i32.load offset=0 align=2`
    pub const I32_LOAD: [u8; 3] = [0x28, 0x02, 0x00];
    pub const I32_ADD: u8 = 0x6A;
    pub const I32_SUB: u8 = 0x6B;
    pub const I32_GT_U: u8 = 0x4B;
    pub const DROP: u8 = 0x1A;
    pub const UNREACHABLE: u8 = 0x00;
    pub const MEMORY_GROW: [u8; 2] = [0x40, 0x00];
    pub const LOOP_EMPTY: [u8; 2] = [0x03, 0x40];
    pub const IF_EMPTY: [u8; 2] = [0x04, 0x40];
    pub const BR0: [u8; 2] = [0x0C, 0x00];
    pub const BR_IF0: [u8; 2] = [0x0D, 0x00];
    pub const RETURN: u8 = 0x0F;
    pub const END: u8 = 0x0B;
}

pub struct Func {
    pub ty: u32,
    pub body: Vec<u8>,
    pub export: Option<String>,
}

#[derive(Default)]
pub struct ModuleBuilder {
    types: Vec<(Vec<u8>, Vec<u8>)>,
    imports: Vec<(String, String, u32)>,
    funcs: Vec<Func>,
    memory: Option<(u32, Option<u32>)>,
    export_memory: bool,
    start: Option<u32>,
}

impl ModuleBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ty(&mut self, params: &[u8], results: &[u8]) -> u32 {
        self.types.push((params.to_vec(), results.to_vec()));
        u32::try_from(self.types.len() - 1).unwrap()
    }

    pub fn import_func(&mut self, module: &str, field: &str, ty: u32) {
        self.imports.push((module.to_owned(), field.to_owned(), ty));
    }

    /// Adds a function; returns its index (imports come first).
    pub fn func(&mut self, ty: u32, body: Vec<u8>, export: Option<&str>) -> u32 {
        self.funcs.push(Func { ty, body, export: export.map(str::to_owned) });
        u32::try_from(self.imports.len() + self.funcs.len() - 1).unwrap()
    }

    pub fn memory(&mut self, min_pages: u32, max_pages: Option<u32>, export: bool) {
        self.memory = Some((min_pages, max_pages));
        self.export_memory = export;
    }

    pub fn start(&mut self, f: u32) {
        self.start = Some(f);
    }

    fn section(id: u8, content: &[u8], out: &mut Vec<u8>) {
        out.push(id);
        uleb(content.len() as u64, out);
        out.extend_from_slice(content);
    }

    pub fn build(&self) -> Vec<u8> {
        let mut out = b"\0asm\x01\0\0\0".to_vec();
        // type
        let mut s = Vec::new();
        uleb(self.types.len() as u64, &mut s);
        for (p, r) in &self.types {
            s.push(0x60);
            uleb(p.len() as u64, &mut s);
            s.extend_from_slice(p);
            uleb(r.len() as u64, &mut s);
            s.extend_from_slice(r);
        }
        Self::section(1, &s, &mut out);
        // import
        if !self.imports.is_empty() {
            let mut s = Vec::new();
            uleb(self.imports.len() as u64, &mut s);
            for (m, f, ty) in &self.imports {
                name(m, &mut s);
                name(f, &mut s);
                s.push(0x00);
                uleb(u64::from(*ty), &mut s);
            }
            Self::section(2, &s, &mut out);
        }
        // function
        let mut s = Vec::new();
        uleb(self.funcs.len() as u64, &mut s);
        for f in &self.funcs {
            uleb(u64::from(f.ty), &mut s);
        }
        Self::section(3, &s, &mut out);
        // memory
        if let Some((min, max)) = self.memory {
            let mut s = vec![1];
            match max {
                Some(max) => {
                    s.push(0x01);
                    uleb(u64::from(min), &mut s);
                    uleb(u64::from(max), &mut s);
                }
                None => {
                    s.push(0x00);
                    uleb(u64::from(min), &mut s);
                }
            }
            Self::section(5, &s, &mut out);
        }
        // export
        let mut exports: Vec<(String, u8, u32)> = Vec::new();
        if self.export_memory {
            exports.push(("memory".to_owned(), 0x02, 0));
        }
        for (i, f) in self.funcs.iter().enumerate() {
            if let Some(n) = &f.export {
                exports.push((n.clone(), 0x00, u32::try_from(self.imports.len() + i).unwrap()));
            }
        }
        let mut s = Vec::new();
        uleb(exports.len() as u64, &mut s);
        for (n, kind, idx) in &exports {
            name(n, &mut s);
            s.push(*kind);
            uleb(u64::from(*idx), &mut s);
        }
        Self::section(7, &s, &mut out);
        // start
        if let Some(f) = self.start {
            let mut s = Vec::new();
            uleb(u64::from(f), &mut s);
            Self::section(8, &s, &mut out);
        }
        // code
        let mut s = Vec::new();
        uleb(self.funcs.len() as u64, &mut s);
        for f in &self.funcs {
            let mut body = vec![0x00]; // no locals
            body.extend_from_slice(&f.body);
            body.push(op::END);
            uleb(body.len() as u64, &mut s);
            s.extend_from_slice(&body);
        }
        Self::section(10, &s, &mut out);
        out
    }
}

/// Concatenates instruction fragments.
pub fn code(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// Which `arca_run` behaviour a test guest has.
pub enum RunBody {
    /// Writes an envelope header in front of the input and returns 0
    /// (echo: the payload is the input itself).
    Echo,
    /// Loops forever.
    InfiniteLoop,
    /// Runs a counted loop of the given number of iterations, then behaves
    /// like [`RunBody::Echo`].
    Burn(i32),
    /// Grows memory until refused.
    MemoryHog,
    /// Executes `unreachable`.
    Trap,
    /// Recurses without bound.
    Recurse,
    /// Returns the given status without touching memory.
    Status(i32),
}

/// Memory layout of the test guests: the input is reserved at `INPUT`,
/// the envelope header sits right before it so the result envelope is
/// `[INPUT-12 .. INPUT+len)`; the reserved length is stored at `LEN_SLOT`.
pub const INPUT: i32 = 1024;
pub const LEN_SLOT: i32 = 8;
/// Scratch cell of the counted-loop guest.
pub const COUNTER_SLOT: i32 = 16;

/// The echo `arca_run` body: writes the envelope header and returns 0.
fn echo_body() -> Vec<u8> {
    code(&[
        // magic "ARCA"
        &op::i32_const(INPUT - 12),
        &op::i32_const(i32::from_le_bytes(*b"ARCA")),
        &op::I32_STORE,
        // ABI version 2, kind 1 (X-ray report)
        &op::i32_const(INPUT - 8),
        &op::i32_const(0x0001_0002),
        &op::I32_STORE,
        // payload length = reserved length (parameter 0)
        &op::i32_const(INPUT - 4),
        &op::local_get(0),
        &op::I32_STORE,
        &op::i32_const(0),
    ])
}

/// Builds an ABI-conformant guest with a configurable `arca_run`.
pub fn guest(
    run: RunBody,
    abi_version: i32,
    output_ptr_override: Option<i32>,
    memory_pages: u32,
) -> Vec<u8> {
    let mut m = ModuleBuilder::new();
    let t_void_i32 = m.ty(&[], &[I32]);
    let t_i32_i32 = m.ty(&[I32], &[I32]);
    m.memory(memory_pages, None, true);

    m.func(t_void_i32, op::i32_const(abi_version), Some("arca_abi_version"));
    // arca_input_reserve(len): if len > 60000 { return 0 } ; mem[LEN_SLOT] = len ; return INPUT
    let reserve = code(&[
        &op::local_get(0),
        &op::i32_const(60_000),
        &[op::I32_GT_U],
        &op::IF_EMPTY,
        &op::i32_const(0),
        &[op::RETURN],
        &[op::END],
        &op::i32_const(LEN_SLOT),
        &op::local_get(0),
        &op::I32_STORE,
        &op::i32_const(INPUT),
    ]);
    m.func(t_i32_i32, reserve, Some("arca_input_reserve"));

    // Function indices: 0 abi_version, 1 input_reserve, 2 run, 3 output_ptr, 4 output_len.
    let run_idx = 2u32;
    let run_body = match run {
        RunBody::Echo => echo_body(),
        RunBody::Burn(iterations) => {
            // mem[COUNTER_SLOT] = iterations; do { mem[COUNTER_SLOT] -= 1 } while mem[COUNTER_SLOT] != 0
            let counter = |tail: &[u8]| code(&[&op::i32_const(COUNTER_SLOT), tail]);
            code(&[
                &counter(&op::i32_const(iterations.max(1))),
                &op::I32_STORE,
                &op::LOOP_EMPTY,
                &counter(&code(&[
                    &op::i32_const(COUNTER_SLOT),
                    &op::I32_LOAD,
                    &op::i32_const(1),
                    &[op::I32_SUB],
                ])),
                &op::I32_STORE,
                &counter(&op::I32_LOAD),
                &op::BR_IF0,
                &[op::END],
                &echo_body(),
            ])
        }
        RunBody::InfiniteLoop => code(&[&op::LOOP_EMPTY, &op::BR0, &[op::END], &op::i32_const(0)]),
        RunBody::MemoryHog => code(&[
            &op::LOOP_EMPTY,
            &op::i32_const(16),
            &op::MEMORY_GROW,
            &[op::DROP],
            &op::BR0,
            &[op::END],
            &op::i32_const(0),
        ]),
        RunBody::Trap => code(&[&[op::UNREACHABLE]]),
        RunBody::Recurse => {
            code(&[&op::local_get(0), &op::call(run_idx), &[op::DROP], &op::i32_const(0)])
        }
        RunBody::Status(s) => op::i32_const(s),
    };
    m.func(t_i32_i32, run_body, Some("arca_run"));
    let ptr_body = match output_ptr_override {
        Some(p) => op::i32_const(p),
        None => op::i32_const(INPUT - 12),
    };
    m.func(t_void_i32, ptr_body, Some("arca_output_ptr"));
    // len = mem[LEN_SLOT] + 12
    m.func(
        t_void_i32,
        code(&[&op::i32_const(LEN_SLOT), &op::I32_LOAD, &op::i32_const(12), &[op::I32_ADD]]),
        Some("arca_output_len"),
    );
    m.build()
}
