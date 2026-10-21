//! PDF files: see [`crate::pdf::decrypt`].

use super::{DecryptError, Decrypted};

pub(crate) fn decrypt(data: &[u8], password: &[u8]) -> Result<Decrypted, DecryptError> {
    crate::pdf::decrypt(data, password)
}
