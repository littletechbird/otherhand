//! `fill-secret REF HANDLE`: resolve a Dark Room handle inside this process
//! and write the plaintext straight into the target field. The plaintext
//! never reaches stdout, stderr, the clipboard, the trace or an error.

pub(crate) mod fernet;
pub(crate) mod fill;
pub(crate) mod origin;
pub(crate) mod vault;
