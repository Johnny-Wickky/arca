// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

//! The JNI entry points: the one file of the workspace that contains `unsafe` code
//! (apart from the five export attributes of the Wasm guest).
//!
//! Every function here does the same three things and nothing else:
//!
//! 1. unwrap the Java arguments (primitives pass through; `byte[]` is copied into a Rust
//!    buffer through `GetByteArrayRegion`, after its length has been checked);
//! 2. call the safe function of [`crate::api`] inside `catch_unwind`, so that a panic can
//!    never unwind into the JVM;
//! 3. wrap the result (`NewByteArray` + `SetByteArrayRegion`, or a primitive).
//!
//! The JNI function table is addressed by index. The indices are those of
//! `struct JNINativeInterface_` in `jni.h`; `jni_table_indices_match_jni_h` (a test that
//! runs when a JDK is installed) recomputes them from the header, and the JVM tests run
//! with `-Xcheck:jni`.
//!
//! File descriptors: `docReadFd` and `sanitizedWriteFd` take **ownership** of the
//! descriptor and close it. The Java side must pass `ParcelFileDescriptor.detachFd()`.

#![allow(unsafe_code, reason = "this file is the JNI boundary")]
#![allow(non_snake_case, reason = "JNI export names are fixed by the JNI specification")]
#![allow(
    clippy::missing_safety_doc,
    reason = "the exports are safe functions: the JVM guarantees their arguments"
)]

use std::ffi::c_void;
use std::fs::File;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::null_mut;

use zeroize::Zeroize;

use crate::api;

type JInt = i32;
type JLong = i64;
type JSize = i32;
/// `jobject`, `jclass`, `jarray`, `jbyteArray` (all opaque pointers).
type JObject = *mut c_void;
/// `const struct JNINativeInterface_` seen as an array of 235 function pointers.
type Table = [*const c_void; 235];
/// `JNIEnv *`.
type Env = *mut *const Table;

// Indices into the function table (checked against jni.h by a test).
const EXCEPTION_CLEAR: usize = 17;
const GET_ARRAY_LENGTH: usize = 171;
const NEW_BYTE_ARRAY: usize = 176;
const GET_BYTE_ARRAY_REGION: usize = 200;
const SET_BYTE_ARRAY_REGION: usize = 208;
const EXCEPTION_CHECK: usize = 228;

type ExceptionClearFn = unsafe extern "system" fn(Env);
type GetArrayLengthFn = unsafe extern "system" fn(Env, JObject) -> JSize;
type NewByteArrayFn = unsafe extern "system" fn(Env, JSize) -> JObject;
type GetByteArrayRegionFn = unsafe extern "system" fn(Env, JObject, JSize, JSize, *mut i8);
type SetByteArrayRegionFn = unsafe extern "system" fn(Env, JObject, JSize, JSize, *const i8);
type ExceptionCheckFn = unsafe extern "system" fn(Env) -> u8;

/// Looks up `table[index]` as a function of type `$ty`.
macro_rules! jni_fn {
    ($env:expr, $index:expr, $ty:ty) => {{
        let table: &Table = $env.table()?;
        let raw: *const c_void = *table.get($index)?;
        if raw.is_null() {
            return None;
        }
        // SAFETY: the JVM fills the table with functions of the documented signatures.
        unsafe { core::mem::transmute::<*const c_void, $ty>(raw) }
    }};
}

/// The `JNIEnv *` of the current native call.
#[derive(Clone, Copy)]
struct JniEnv(Env);

impl JniEnv {
    fn new(env: Env) -> Option<Self> {
        if env.is_null() { None } else { Some(Self(env)) }
    }

    fn table(&self) -> Option<&Table> {
        // SAFETY: `self.0` is non-null and, by the JNI contract, points to a valid
        // `JNIEnv` (a pointer to the function table) for the duration of the call.
        unsafe { (*self.0).as_ref() }
    }

    fn exception_pending(&self) -> bool {
        let check = (|| -> Option<bool> {
            let f = jni_fn!(self, EXCEPTION_CHECK, ExceptionCheckFn);
            // SAFETY: valid env, no arguments.
            Some(unsafe { f(self.0) } != 0)
        })();
        check.unwrap_or(true)
    }

    fn clear_exception(&self) {
        let _ = (|| -> Option<()> {
            let f = jni_fn!(self, EXCEPTION_CLEAR, ExceptionClearFn);
            // SAFETY: valid env, no arguments.
            unsafe { f(self.0) };
            Some(())
        })();
    }

    fn array_len(&self, array: JObject) -> Option<usize> {
        if array.is_null() {
            return None;
        }
        let f = jni_fn!(self, GET_ARRAY_LENGTH, GetArrayLengthFn);
        // SAFETY: `array` is a non-null `byte[]` reference passed in by the JVM.
        let n = unsafe { f(self.0, array) };
        usize::try_from(n).ok()
    }

    /// Copies `array[off..off + len]` into a Rust buffer. `None` if the range does not fit
    /// the array, memory is short, or the JVM raised an exception.
    fn read_array(&self, array: JObject, off: usize, len: usize) -> Option<Vec<u8>> {
        let total = self.array_len(array)?;
        if off.checked_add(len)? > total {
            return None;
        }
        let mut buf = Vec::new();
        buf.try_reserve_exact(len).ok()?;
        buf.resize(len, 0u8);
        if len > 0 {
            let f = jni_fn!(self, GET_BYTE_ARRAY_REGION, GetByteArrayRegionFn);
            // SAFETY: `buf` holds `len` writable bytes; the range was checked against the
            // array length, so the JVM neither overruns `buf` nor throws.
            unsafe {
                f(
                    self.0,
                    array,
                    JSize::try_from(off).ok()?,
                    JSize::try_from(len).ok()?,
                    buf.as_mut_ptr().cast::<i8>(),
                );
            }
            if self.exception_pending() {
                self.clear_exception();
                buf.zeroize();
                return None;
            }
        }
        Some(buf)
    }

    /// A new Java `byte[]` holding `data`; null on failure (an `OutOfMemoryError` stays
    /// pending for the Java caller).
    fn new_array(&self, data: &[u8]) -> Option<JObject> {
        let len = JSize::try_from(data.len()).ok()?;
        let make = jni_fn!(self, NEW_BYTE_ARRAY, NewByteArrayFn);
        // SAFETY: valid env; `len` is non-negative.
        let array = unsafe { make(self.0, len) };
        if array.is_null() {
            return None;
        }
        if len > 0 {
            let set = jni_fn!(self, SET_BYTE_ARRAY_REGION, SetByteArrayRegionFn);
            // SAFETY: `array` has exactly `len` elements and `data` holds `len` bytes.
            unsafe { set(self.0, array, 0, len, data.as_ptr().cast::<i8>()) };
            if self.exception_pending() {
                return None;
            }
        }
        Some(array)
    }
}

/// Runs `f`; a panic becomes `default` instead of unwinding into the JVM.
fn guard<T>(default: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(default)
}

fn bytes_result(env: Env, f: impl FnOnce() -> Option<Vec<u8>>) -> JObject {
    let Some(data) = catch_unwind(AssertUnwindSafe(f)).ok().flatten() else {
        return null_mut();
    };
    JniEnv::new(env).and_then(|e| e.new_array(&data)).unwrap_or(null_mut())
}

/// The bytes of an optional `byte[]` argument (`null` is an empty record).
fn optional_bytes(env: Env, array: JObject) -> Vec<u8> {
    JniEnv::new(env)
        .and_then(|e| {
            let n = e.array_len(array)?;
            e.read_array(array, 0, n.min(1 << 16))
        })
        .unwrap_or_default()
}

const BAD_ARGUMENT: JInt = 12;
const FAULT: JInt = 9;

#[cfg(unix)]
fn file_from_fd(fd: JInt) -> Option<File> {
    use std::os::fd::FromRawFd;
    if fd < 0 {
        return None;
    }
    // SAFETY: the Java side hands over a descriptor it detached from its
    // `ParcelFileDescriptor`; this function is the new and only owner.
    Some(unsafe { File::from_raw_fd(fd) })
}

#[cfg(not(unix))]
fn file_from_fd(_fd: JInt) -> Option<File> {
    None
}

// ---------------------------------------------------------------------------
// Exports: org.arca.viewer.Native
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_init(
    _env: Env,
    _class: JObject,
    max_input_mib: JInt,
    max_memory_mib: JInt,
    scan_secs: JInt,
    preview_secs: JInt,
) -> JInt {
    guard(FAULT, || api::init(max_input_mib, max_memory_mib, scan_secs, preview_secs))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_info(env: Env, _class: JObject) -> JObject {
    bytes_result(env, api::info)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_selfTest(env: Env, _class: JObject) -> JObject {
    bytes_result(env, api::self_test)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_docNew(
    _env: Env,
    _class: JObject,
    max_bytes: JLong,
) -> JLong {
    guard(-i64::from(FAULT), || api::doc_new(max_bytes))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_docAppend(
    env: Env,
    _class: JObject,
    doc: JLong,
    buf: JObject,
    off: JInt,
    len: JInt,
) -> JInt {
    guard(FAULT, || {
        let (Ok(off), Ok(len)) = (usize::try_from(off), usize::try_from(len)) else {
            return BAD_ARGUMENT;
        };
        let Some(mut data) = JniEnv::new(env).and_then(|e| e.read_array(buf, off, len)) else {
            return BAD_ARGUMENT;
        };
        let status = api::doc_append(doc, &data);
        data.zeroize();
        status
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_docSetPassword(
    env: Env,
    _class: JObject,
    doc: JLong,
    buf: JObject,
    off: JInt,
    len: JInt,
) -> JInt {
    guard(FAULT, || {
        let (Ok(off), Ok(len)) = (usize::try_from(off), usize::try_from(len)) else {
            return BAD_ARGUMENT;
        };
        let Some(mut data) = JniEnv::new(env).and_then(|e| e.read_array(buf, off, len)) else {
            return BAD_ARGUMENT;
        };
        let status = api::doc_set_password(doc, &data);
        data.zeroize();
        status
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_docReadFd(
    _env: Env,
    _class: JObject,
    doc: JLong,
    fd: JInt,
) -> JLong {
    guard(-i64::from(FAULT), || match file_from_fd(fd) {
        Some(mut file) => api::doc_read(doc, &mut file),
        None => -i64::from(BAD_ARGUMENT),
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_docLen(
    _env: Env,
    _class: JObject,
    doc: JLong,
) -> JLong {
    guard(-i64::from(FAULT), || api::doc_len(doc))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_docFree(_env: Env, _class: JObject, doc: JLong) {
    guard((), || api::doc_free(doc));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_jobNew(_env: Env, _class: JObject) -> JLong {
    guard(-i64::from(FAULT), api::job_new)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_jobCancel(
    _env: Env,
    _class: JObject,
    job: JLong,
) {
    guard((), || api::job_cancel(job));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_jobFree(_env: Env, _class: JObject, job: JLong) {
    guard((), || api::job_free(job));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_run(
    _env: Env,
    _class: JObject,
    job: JLong,
    doc: JLong,
    op: JInt,
    flags: JInt,
) -> JLong {
    guard(-i64::from(FAULT), || api::run(job, doc, op, flags))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_outcomeStatus(
    _env: Env,
    _class: JObject,
    outcome: JLong,
) -> JInt {
    guard(-FAULT, || api::outcome_status(outcome))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_outcomeInfo(
    env: Env,
    _class: JObject,
    outcome: JLong,
) -> JObject {
    bytes_result(env, || api::outcome_info(outcome))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_report(
    env: Env,
    _class: JObject,
    outcome: JLong,
) -> JObject {
    bytes_result(env, || api::report(outcome))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_previewMeta(
    env: Env,
    _class: JObject,
    outcome: JLong,
) -> JObject {
    bytes_result(env, || api::preview_meta(outcome))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_previewItems(
    env: Env,
    _class: JObject,
    outcome: JLong,
    section: JInt,
    from: JLong,
    count: JInt,
) -> JObject {
    bytes_result(env, || api::preview_items(outcome, section, from, count))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_sanitizedInfo(
    env: Env,
    _class: JObject,
    outcome: JLong,
) -> JObject {
    bytes_result(env, || api::sanitized_info(outcome))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_sanitizedWriteFd(
    _env: Env,
    _class: JObject,
    outcome: JLong,
    fd: JInt,
) -> JLong {
    guard(-i64::from(FAULT), || match file_from_fd(fd) {
        Some(mut file) => api::sanitized_write(outcome, &mut file),
        None => -i64::from(BAD_ARGUMENT),
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_outcomeFree(
    _env: Env,
    _class: JObject,
    outcome: JLong,
) {
    guard((), || api::outcome_free(outcome));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_about(
    env: Env,
    _class: JObject,
    lang: JInt,
) -> JObject {
    bytes_result(env, || Some(api::about(lang)))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_passport(
    env: Env,
    _class: JObject,
    lang: JInt,
    record: JObject,
    today: JInt,
) -> JObject {
    bytes_result(env, || Some(api::passport(lang, &optional_bytes(env, record), today)))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_passportUpdate(
    env: Env,
    _class: JObject,
    record: JObject,
    today: JInt,
    trackers: JInt,
    active: JInt,
) -> JObject {
    bytes_result(env, || {
        Some(api::passport_update(&optional_bytes(env, record), today, trackers, active))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_licenseText(
    env: Env,
    _class: JObject,
    which: JInt,
) -> JObject {
    bytes_result(env, || api::license_text(which))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_arca_viewer_Native_releaseInfo(
    env: Env,
    _class: JObject,
) -> JObject {
    bytes_result(env, || Some(api::release_info()))
}

#[cfg(test)]
mod tests {
    /// Recomputes the table indices used above from the JDK's `jni.h`, when one is installed.
    #[test]
    fn jni_table_indices_match_jni_h() {
        let candidates = [
            std::env::var("JAVA_HOME").ok().map(|h| format!("{h}/include/jni.h")),
            Some("/usr/lib/jvm/default-java/include/jni.h".to_owned()),
            Some("/usr/lib/jvm/java-21-openjdk-amd64/include/jni.h".to_owned()),
            Some("/usr/lib/jvm/java-17-openjdk-amd64/include/jni.h".to_owned()),
        ];
        let Some(header) =
            candidates.into_iter().flatten().find_map(|p| std::fs::read_to_string(p).ok())
        else {
            assert!(
                std::env::var_os("ARCA_REQUIRE_JDK").is_none(),
                "ARCA_REQUIRE_JDK is set but no jni.h was found"
            );
            return;
        };
        let start = header.find("struct JNINativeInterface_ {").unwrap_or(0);
        let end = header.find("struct JNIEnv_ {").unwrap_or(header.len());
        let body = header.get(start..end).unwrap_or_default();
        // Member names in declaration order: the four reserved slots, then every
        // `(JNICALL *Name)`.
        let mut names: Vec<String> = Vec::new();
        let mut rest = body;
        while let Some(i) = rest.find(['(', 'v']) {
            let tail = rest.get(i..).unwrap_or_default();
            if let Some(after) = tail.strip_prefix("void *reserved") {
                names.push(format!("reserved{}", after.chars().next().unwrap_or('?')));
                rest = after;
            } else if let Some(after) = tail.strip_prefix("(JNICALL *") {
                let name: String = after.chars().take_while(|c| c.is_alphanumeric()).collect();
                names.push(name);
                rest = after;
            } else {
                rest = tail.get(1..).unwrap_or_default();
            }
        }
        let index = |n: &str| names.iter().position(|x| x == n);
        assert_eq!(names.len(), 235);
        assert_eq!(index("ExceptionClear"), Some(super::EXCEPTION_CLEAR));
        assert_eq!(index("GetArrayLength"), Some(super::GET_ARRAY_LENGTH));
        assert_eq!(index("NewByteArray"), Some(super::NEW_BYTE_ARRAY));
        assert_eq!(index("GetByteArrayRegion"), Some(super::GET_BYTE_ARRAY_REGION));
        assert_eq!(index("SetByteArrayRegion"), Some(super::SET_BYTE_ARRAY_REGION));
        assert_eq!(index("ExceptionCheck"), Some(super::EXCEPTION_CHECK));
    }
}
