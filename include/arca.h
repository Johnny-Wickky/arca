/* SPDX-License-Identifier: AGPL-3.0-only */
/* Arca — paranoid, memory-safe document viewer core. */
/* Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md. */

/**
 * @file arca.h
 * @brief C ABI for the Arca zero-trust document security engine.
 *
 * This header defines the native C interface to the Arca Rust core.
 * It is consumed directly by Swift, Objective-C, and generic C environments on iOS,
 * macOS, and other POSIX platforms.
 *
 * Design principles:
 * 1. Handles over pointers: Documents, jobs, and outcomes are opaque positive
 *    64-bit integers (`int64_t`). Invalid or expired handles fail deterministically
 *    with `ARCA_STATUS_BAD_HANDLE` (-10), never causing memory corruption.
 * 2. Strict memory zeroization: Buffers returned in `ArcaBuffer` are allocated
 *    on the Rust heap and securely wiped (`zeroize`) when freed via `arca_buffer_free`.
 * 3. Thread-safe cancellation: `arca_job_cancel` can be called from any thread
 *    (e.g., UI main queue) while `arca_run` executes on a background worker thread.
 */

#ifndef ARCA_H
#define ARCA_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* -------------------------------------------------------------------------- */
/* Status Codes                                                               */
/* -------------------------------------------------------------------------- */

/**
 * @brief Outcome status classes returned by Arca operations.
 *
 * When an Arca function returns an integer status code:
 * - `0` (`ARCA_STATUS_OK`) indicates success.
 * - Greater than `0` indicates a specific operational result or failure.
 *
 * For functions returning an `int64_t` handle or byte count, a negative value
 * `-status` represents an error (e.g., `-ARCA_STATUS_BAD_HANDLE` for handle errors).
 */
typedef enum ArcaStatus {
    /** @brief Operation completed successfully. */
    ARCA_STATUS_OK = 0,
    /** @brief The operation was cancelled by the user via job token. */
    ARCA_STATUS_CANCELLED = 1,
    /** @brief Wall-clock execution deadline passed. */
    ARCA_STATUS_TIMEOUT = 2,
    /** @brief Document exceeds configured input limits. */
    ARCA_STATUS_TOO_LARGE = 3,
    /** @brief Document was rejected as hostile, corrupted, or violating limits. */
    ARCA_STATUS_REJECTED = 4,
    /** @brief Format or requested feature is unsupported. */
    ARCA_STATUS_UNSUPPORTED = 5,
    /** @brief Document is encrypted and requires a password. */
    ARCA_STATUS_ENCRYPTED = 6,
    /** @brief Operation failed after document acceptance (e.g. sanitization check failed). */
    ARCA_STATUS_FAILED = 7,
    /** @brief Guest exceeded CPU fuel, memory ceiling, or stack budget. */
    ARCA_STATUS_RESOURCE_LIMIT = 8,
    /** @brief Internal engine or sandbox fault. */
    ARCA_STATUS_FAULT = 9,
    /** @brief Handle does not refer to a live object. */
    ARCA_STATUS_BAD_HANDLE = 10,
    /** @brief I/O error occurred while reading or writing. */
    ARCA_STATUS_IO = 11,
    /** @brief Function argument is invalid or out of range. */
    ARCA_STATUS_BAD_ARGUMENT = 12,
    /** @brief Engine has not been initialized yet. */
    ARCA_STATUS_NOT_READY = 13,
    /** @brief Document is encrypted and the provided password failed. */
    ARCA_STATUS_WRONG_PASSWORD = 14
} ArcaStatus;

/* -------------------------------------------------------------------------- */
/* Operation Codes                                                            */
/* -------------------------------------------------------------------------- */

/**
 * @brief Operation codes for `arca_run`.
 */
typedef enum ArcaOp {
    /** @brief Threat X-ray scan; output is a JSON X-Ray report. */
    ARCA_OP_SCAN = 1,
    /** @brief Produce a sanitized, sterile document copy. */
    ARCA_OP_SANITIZE = 2,
    /** @brief Extract structured read-only preview (paragraphs, tables, slides). */
    ARCA_OP_PREVIEW = 3,
    /** @brief Render vector primitives of a specific page. */
    ARCA_OP_RENDER = 4,
    /** @brief Rasterize an embedded picture into premultiplied RGBA. */
    ARCA_OP_IMAGE = 5
} ArcaOp;

/* -------------------------------------------------------------------------- */
/* Operation Flags                                                            */
/* -------------------------------------------------------------------------- */

/** @brief Sanitize flag: accept all tracked revisions. */
#define ARCA_FLAG_ACCEPT_REVISIONS 1

/** @brief Sanitize flag: remove comments and author metadata. */
#define ARCA_FLAG_REMOVE_COMMENTS 2

/** @brief Sanitize flag: strip author, company, dates, paths, printer metadata. */
#define ARCA_FLAG_STRIP_METADATA 4

/** @brief Sanitize flag: remove custom XML data parts. */
#define ARCA_FLAG_REMOVE_CUSTOM_XML 8

/** @brief Sanitize flag: apply all standard sanitization rules. */
#define ARCA_FLAG_SANITIZE_ALL (ARCA_FLAG_ACCEPT_REVISIONS | ARCA_FLAG_REMOVE_COMMENTS | ARCA_FLAG_STRIP_METADATA | ARCA_FLAG_REMOVE_CUSTOM_XML)

/** @brief Sanitize flag: retain up to 16 megapixels for pictures instead of 4 megapixels. */
#define ARCA_FLAG_FULL_SIZE 16

/** @brief Preview flag: extract hidden text/sheets/slides marked as hidden. */
#define ARCA_FLAG_REVEAL_HIDDEN 1

/** @brief Preview flag: all preview flags combined. */
#define ARCA_FLAG_PREVIEW_ALL ARCA_FLAG_REVEAL_HIDDEN

/** @brief Render flag: page number bitmask (0-indexed). */
#define ARCA_FLAG_PAGE_MASK 0x00FFFFFF

/* -------------------------------------------------------------------------- */
/* Data Structures                                                            */
/* -------------------------------------------------------------------------- */

/**
 * @brief Contiguous byte buffer allocated by Arca.
 *
 * When `data` is non-NULL, the caller owns the memory and must free it
 * by calling `arca_buffer_free`.
 */
typedef struct ArcaBuffer {
    /** @brief Pointer to byte array, or NULL if empty. */
    uint8_t *data;
    /** @brief Number of valid bytes in `data`. */
    size_t len;
} ArcaBuffer;

/* -------------------------------------------------------------------------- */
/* Memory Management                                                          */
/* -------------------------------------------------------------------------- */

/**
 * @brief Frees an `ArcaBuffer` allocated by the Arca core.
 *
 * Before deallocation, memory is volatilely wiped (`zeroize`) to prevent
 * plaintext document fragments or metadata from persisting in memory.
 *
 * @param buf The buffer to free. Safe to call with a NULL or empty buffer.
 */
void arca_buffer_free(ArcaBuffer buf);

/* -------------------------------------------------------------------------- */
/* Lifecycle & Engine Configuration                                           */
/* -------------------------------------------------------------------------- */

/**
 * @brief Initializes the Arca engine with device-appropriate limits.
 *
 * Must be called once per process lifecycle before opening documents.
 * Later calls return the initial result's status without re-initializing.
 *
 * @param max_input_mib Maximum allowed input document size in MiB (0 for default 256 MiB).
 * @param max_memory_mib Guest linear memory ceiling in MiB (0 for default 768 MiB).
 * @param scan_secs Wall-clock timeout for X-ray scans in seconds (0 for default 120s).
 * @param preview_secs Wall-clock timeout for previews in seconds (0 for default 300s).
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_init(
    int32_t max_input_mib,
    int32_t max_memory_mib,
    int32_t scan_secs,
    int32_t preview_secs
);

/**
 * @brief Checks if a WebAssembly guest was embedded into the library at build time.
 * @return 1 if guest is present, 0 otherwise.
 */
int32_t arca_has_guest(void);

/**
 * @brief Checks if the engine has been successfully initialized.
 * @return 1 if initialized, 0 otherwise.
 */
int32_t arca_is_init(void);

/**
 * @brief Obtains JSON metadata about the engine configuration and embedded guest module.
 *
 * @param[out] out Receives the JSON buffer. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_info(ArcaBuffer *out);

/**
 * @brief Runs the built-in self-test suite and writes its diagnostic JSON report.
 *
 * @param[out] out Receives the JSON buffer. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_self_test(ArcaBuffer *out);

/* -------------------------------------------------------------------------- */
/* Document Ingestion & Management                                            */
/* -------------------------------------------------------------------------- */

/**
 * @brief Creates a new empty document slot accepting at most `max_bytes`.
 *
 * @param max_bytes Maximum bytes to accept (clamped by engine's input limit).
 * @return Positive document handle on success, or negative `-status` on error.
 */
int64_t arca_doc_new(int64_t max_bytes);

/**
 * @brief Appends a chunk of bytes to an unsealed document.
 *
 * @param doc Document handle returned by `arca_doc_new`.
 * @param data Pointer to input bytes.
 * @param len Number of bytes to append.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_doc_append(int64_t doc, const uint8_t *data, size_t len);

/**
 * @brief Ingests an entire file from disk into the document.
 *
 * @param doc Document handle.
 * @param path Null-terminated UTF-8 filesystem path.
 * @return Number of bytes read on success, or negative `-status` on error.
 */
int64_t arca_doc_read_file(int64_t doc, const char *path);

/**
 * @brief Ingests document bytes from a POSIX file descriptor until EOF.
 *
 * Takes ownership of `fd` and closes it upon completion.
 *
 * @param doc Document handle.
 * @param fd Open POSIX file descriptor (e.g. from security-scoped URL).
 * @return Number of bytes read on success, or negative `-status` on error.
 */
int64_t arca_doc_read_fd(int64_t doc, int32_t fd);

/**
 * @brief Sets or clears the password used for encrypted documents.
 *
 * @param doc Document handle.
 * @param password Pointer to UTF-8 password bytes, or NULL / len=0 to clear.
 * @param len Byte length of password.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_doc_set_password(int64_t doc, const uint8_t *password, size_t len);

/**
 * @brief Returns the total byte size of the document ingested so far.
 *
 * @param doc Document handle.
 * @return Number of bytes on success, or negative `-status` on error.
 */
int64_t arca_doc_len(int64_t doc);

/**
 * @brief Releases the document and securely zeroes its memory.
 *
 * If a background job is still running over this document, the bytes remain
 * until the job finishes, then are immediately wiped.
 *
 * @param doc Document handle.
 */
void arca_doc_free(int64_t doc);

/* -------------------------------------------------------------------------- */
/* Jobs & Cancellation                                                        */
/* -------------------------------------------------------------------------- */

/**
 * @brief Creates a cancellation job token.
 *
 * @return Positive job handle on success, or negative `-status` on error.
 */
int64_t arca_job_new(void);

/**
 * @brief Requests prompt cancellation of any operation executing with `job`.
 *
 * Thread-safe: can be invoked concurrently from any thread.
 *
 * @param job Job handle.
 */
void arca_job_cancel(int64_t job);

/**
 * @brief Releases a cancellation job token.
 *
 * @param job Job handle.
 */
void arca_job_free(int64_t job);

/* -------------------------------------------------------------------------- */
/* Synchronous Execution                                                      */
/* -------------------------------------------------------------------------- */

/**
 * @brief Executes an operation over a document in the isolated sandbox (blocking).
 *
 * Seal the document if not already sealed and executes `op` inside the single-use
 * WebAssembly sandbox. Should typically be called from a background worker thread.
 *
 * @param job Cancellation job handle.
 * @param doc Document handle.
 * @param op Operation code (from `ArcaOp`).
 * @param flags Operation-specific bitmask flags (e.g., `ARCA_FLAG_SANITIZE_ALL`).
 * @return Positive outcome handle on success, or negative `-status` on error.
 */
int64_t arca_run(int64_t job, int64_t doc, int32_t op, int32_t flags);

/* -------------------------------------------------------------------------- */
/* Outcome Inspection & Data Retrieval                                        */
/* -------------------------------------------------------------------------- */

/**
 * @brief Returns the execution status of a finished outcome.
 *
 * @param outcome Outcome handle.
 * @return Status code (0 for OK, 4 for Rejected, etc.), or negative for invalid handle.
 */
int32_t arca_outcome_status(int64_t outcome);

/**
 * @brief Obtains diagnostic and resource execution stats for the outcome as JSON.
 *
 * @param outcome Outcome handle.
 * @param[out] out Receives the JSON buffer. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_outcome_info(int64_t outcome, ArcaBuffer *out);

/**
 * @brief Obtains the threat X-ray report as JSON.
 *
 * @param outcome Outcome handle produced with `ARCA_OP_SCAN`.
 * @param[out] out Receives the JSON buffer. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_outcome_report(int64_t outcome, ArcaBuffer *out);

/**
 * @brief Obtains preview sections metadata, notices, and item counts as JSON.
 *
 * @param outcome Outcome handle produced with `ARCA_OP_PREVIEW`.
 * @param[out] out Receives the JSON buffer. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_outcome_preview_meta(int64_t outcome, ArcaBuffer *out);

/**
 * @brief Obtains a paginated chunk of preview items as a JSON array.
 *
 * Allows lazy virtualized rendering of large documents without memory spikes.
 *
 * @param outcome Outcome handle produced with `ARCA_OP_PREVIEW`.
 * @param section Section index (from metadata).
 * @param from Starting item index.
 * @param count Requested item count (clamped to max 512 items).
 * @param[out] out Receives the JSON buffer. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_outcome_preview_items(
    int64_t outcome,
    uint32_t section,
    uint64_t from,
    uint32_t count,
    ArcaBuffer *out
);

/**
 * @brief Obtains rendered page primitives (lines, text, boxes) as JSON.
 *
 * @param outcome Outcome handle produced with `ARCA_OP_RENDER`.
 * @param[out] out Receives the JSON buffer. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_outcome_page(int64_t outcome, ArcaBuffer *out);

/**
 * @brief Obtains picture metadata (dimensions, EXIF notices, format) as JSON.
 *
 * @param outcome Outcome handle produced with `ARCA_OP_IMAGE`.
 * @param[out] out Receives the JSON buffer. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_outcome_image_info(int64_t outcome, ArcaBuffer *out);

/**
 * @brief Obtains premultiplied RGBA raster pixels (4 bytes per pixel: R, G, B, A).
 *
 * Pixels are decoded purely in safe Rust inside the sandbox, avoiding platform decoder bugs.
 *
 * @param outcome Outcome handle produced with `ARCA_OP_IMAGE`.
 * @param[out] out Receives the raw pixel bytes. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_outcome_image_pixels(int64_t outcome, ArcaBuffer *out);

/**
 * @brief Obtains metadata and verification info of a sterile copy as JSON.
 *
 * @param outcome Outcome handle produced with `ARCA_OP_SANITIZE`.
 * @param[out] out Receives the JSON buffer. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_outcome_sanitized_info(int64_t outcome, ArcaBuffer *out);

/**
 * @brief Returns the size in bytes of the sterile sanitized document copy.
 *
 * @param outcome Outcome handle produced with `ARCA_OP_SANITIZE`.
 * @return Byte count on success, or negative `-status` on error.
 */
int64_t arca_outcome_sanitized_len(int64_t outcome);

/**
 * @brief Retrieves the entire sterile document copy directly as an in-memory byte buffer.
 *
 * Ideal for immediate in-memory sharing (e.g. via iOS UIActivityViewController)
 * without intermediate temporary file creation.
 *
 * @param outcome Outcome handle produced with `ARCA_OP_SANITIZE`.
 * @param[out] out Receives the sanitized document bytes. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_outcome_sanitized_bytes(int64_t outcome, ArcaBuffer *out);

/**
 * @brief Writes the sterile document copy directly to a file on disk.
 *
 * @param outcome Outcome handle produced with `ARCA_OP_SANITIZE`.
 * @param path Null-terminated UTF-8 destination file path.
 * @return Number of bytes written on success, or negative `-status` on error.
 */
int64_t arca_outcome_sanitized_write_file(int64_t outcome, const char *path);

/**
 * @brief Writes the sterile document copy to a POSIX file descriptor and closes `fd`.
 *
 * @param outcome Outcome handle produced with `ARCA_OP_SANITIZE`.
 * @param fd Destination POSIX file descriptor.
 * @return Number of bytes written on success, or negative `-status` on error.
 */
int64_t arca_outcome_sanitized_write_fd(int64_t outcome, int32_t fd);

/**
 * @brief Releases the outcome handle and frees associated buffers.
 *
 * @param outcome Outcome handle.
 */
void arca_outcome_free(int64_t outcome);

/* -------------------------------------------------------------------------- */
/* Attribution, Licensing & Patron Passport (AGPLv3 Mandatory Compliance)    */
/* -------------------------------------------------------------------------- */

/**
 * @brief Generates the "About & Support" screen JSON structure.
 *
 * Contains required upstream creator attribution and support links per
 * AGPLv3 section 7(b) and LICENSE_ADDITIONAL_TERMS.md.
 *
 * @param lang Language selector: `0` for Russian, `1` (or any other) for English.
 * @param[out] out Receives the JSON buffer. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_about(int32_t lang, ArcaBuffer *out);

/**
 * @brief Generates the Guardian's Passport JSON for a given 36-byte local stats record.
 *
 * @param lang Language selector (`0` = Ru, `1` = En).
 * @param record Pointer to 36-byte patron record buffer (or NULL / 0 length for fresh).
 * @param record_len Length of record buffer (expected 36 bytes).
 * @param today Unix day number (`unix_timestamp / 86400`).
 * @param[out] out Receives the JSON buffer. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_passport(
    int32_t lang,
    const uint8_t *record,
    size_t record_len,
    int32_t today,
    ArcaBuffer *out
);

/**
 * @brief Updates the 36-byte patron record after opening a document.
 *
 * Increments document counts, blocked trackers, and neutralized active elements.
 *
 * @param record Pointer to current 36-byte record (or NULL / 0 for new).
 * @param record_len Length of record buffer.
 * @param today Unix day number.
 * @param trackers Count of hidden trackers blocked in this document.
 * @param active Count of active content neutralized.
 * @param[out] out Receives the updated 36-byte record. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_passport_update(
    const uint8_t *record,
    size_t record_len,
    int32_t today,
    int32_t trackers,
    int32_t active,
    ArcaBuffer *out
);

/**
 * @brief Optional local counter: the blob after one scanned document.
 *
 * The counter is an opaque, versioned, CRC-protected blob of eight flat all-time totals that the
 * app keeps in its private storage only while the person has switched the counter on (it is off
 * by default). There is no date, time or per-day count anywhere in it, and nothing else about
 * documents is ever stored. Pass NULL / 0 for "no totals yet"; a damaged or unknown blob
 * (including one written by an older version that kept a daily history) is treated as empty.
 *
 * @param verdict Wire tag of the X-ray verdict: 0 Clean, 1 HiddenTrackers,
 *        2 ActiveContentNeutralized, 3 Rejected, 4 Unverified (anything else counts as 4).
 * @param trackers Remote-content findings of medium severity or higher (negative = 0).
 * @param active Active-content findings of medium severity or higher (negative = 0).
 * @param[out] out Receives the new blob. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_stats_record_scan(
    const uint8_t *blob,
    size_t blob_len,
    int32_t verdict,
    int32_t trackers,
    int32_t active,
    ArcaBuffer *out
);

/**
 * @brief Optional local counter: the blob after a sterile copy (`event` 1) or an opened picture (2).
 *
 * Any other event code returns the blob unchanged. See `arca_stats_record_scan`.
 */
int32_t arca_stats_record_event(
    const uint8_t *blob,
    size_t blob_len,
    int32_t event,
    ArcaBuffer *out
);

/**
 * @brief Optional local counter: the flat all-time totals as JSON.
 *
 * `{"version":2,"empty":false,"all":{...}}` where the totals hold opened, clean, trackers,
 * active, threats, rejected, unverified, sterile and pictures.
 *
 * @param[out] out Receives the JSON. Caller must free with `arca_buffer_free`.
 */
int32_t arca_stats_summary(
    const uint8_t *blob,
    size_t blob_len,
    ArcaBuffer *out
);

/**
 * @brief The Patron Passport JSON (as `arca_passport`) built from the counter blob.
 *
 * @param first_use_day The local calendar day the app was installed (days since 1970-01-01).
 * @param today Today's local calendar day.
 */
int32_t arca_stats_passport(
    int32_t lang,
    const uint8_t *blob,
    size_t blob_len,
    int32_t first_use_day,
    int32_t today,
    ArcaBuffer *out
);

/**
 * @brief Reminder rules: when to show the next support reminder.
 *
 * 7 to 9 local days after `now_ms` (epoch milliseconds), between 12:00 and 17:59 local time.
 * `offset_ms` is the UTC offset of the local time zone in milliseconds, `rnd` any random
 * number. The core reads no clock and draws no randomness.
 */
int64_t arca_reminder_plan(int64_t now_ms, int32_t offset_ms, int32_t rnd);

/**
 * @brief Reminder rules: moves a planned time so that it keeps its local wall-clock time when
 *        the UTC offset at that time (`offset_at_target_ms`) differs from the one used.
 */
int64_t arca_reminder_refine(int64_t target_ms, int32_t offset_used_ms, int32_t offset_at_target_ms);

/**
 * @brief Reminder rules: what to do when the reminder fires.
 *
 * JSON `{"action":"show|quietHours|tooSoon|noPermission|disabled","at":<epoch ms>}`: post
 * only on "show"; in every case except "disabled" arm the next trigger for `at`.
 * Reminders are shown between 11:00 and 20:30 local time, at least seven days apart.
 *
 * @param last_shown_ms When the previous reminder was shown (0 if never); the host keeps only
 *        the day, not a time of day.
 * @param enabled Non-zero if the person has reminders switched on.
 * @param can_post Non-zero if the system lets the app post notifications.
 */
int32_t arca_reminder_on_fire(
    int64_t now_ms,
    int32_t offset_ms,
    int64_t last_shown_ms,
    int32_t enabled,
    int32_t can_post,
    int32_t rnd,
    ArcaBuffer *out
);

/**
 * @brief Reminder rules: what to do with the stored time when the app starts.
 *
 * JSON `{"action":"off|keep|plan","at":<epoch ms>}`: "off" cancels, "keep" and "plan" arm the
 * trigger for `at` and store it. `next_ms` is the stored time (0 if none).
 */
int32_t arca_reminder_on_start(
    int64_t next_ms,
    int64_t now_ms,
    int32_t offset_ms,
    int32_t enabled,
    int32_t rnd,
    ArcaBuffer *out
);

/**
 * @brief Reminder rules: which reminder text to show.
 *
 * Returns the index (0 .. `arca_reminder_item_count() - 1`) for the stored position `seq`; the
 * texts go round in order and quote no counter. Store `index + 1` as the next `seq`.
 */
int32_t arca_reminder_pick(int32_t seq);

/** @brief Reminder rules: the number of reminder texts the app must have. */
int32_t arca_reminder_item_count(void);

/**
 * @brief The extra support addresses (crypto wallets, Boosty) as JSON.
 *
 * `{"items":[{"kind":"crypto"|"boosty","label":..,"network":..,"value":..,"memo":..}]}`. The list
 * is empty until the owner configures it (crates/arca-attribution/src/support.rs). Show the
 * full value first; copy a wallet address only on an explicit tap (mark the clipboard item
 * sensitive and clear it later); open a Boosty address only after an explicit tap.
 */
int32_t arca_extra_support(ArcaBuffer *out);

/**
 * @brief Retrieves verbatim license text strings.
 *
 * @param which License selector: `0` = GNU AGPLv3, `1` = Additional Terms (Russian), `2` = English.
 * @param[out] out Receives the text buffer. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_license_text(int32_t which, ArcaBuffer *out);

/**
 * @brief Retrieves release readiness status and blocker notices as JSON.
 *
 * @param[out] out Receives the JSON buffer. Caller must free with `arca_buffer_free`.
 * @return ARCA_STATUS_OK (0) on success, or an error status code.
 */
int32_t arca_release_info(ArcaBuffer *out);

#ifdef __cplusplus
}
#endif

#endif /* ARCA_H */
