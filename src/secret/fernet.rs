//! Fernet token decryption (AES-128-CBC with HMAC-SHA256), the format
//! Dark Room's Python vault writes. Verify first, then decrypt.

use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FernetError {
    Key,
    Token,
    Signature,
}

fn b64(s: &[u8]) -> Result<Vec<u8>, ()> {
    let trimmed: Vec<u8> = s.iter().copied().filter(|c| !c.is_ascii_whitespace()).collect();
    base64::engine::general_purpose::URL_SAFE
        .decode(&trimmed)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&trimmed))
        .map_err(|_| ())
}

pub(crate) struct FernetKey {
    signing: [u8; 16],
    encryption: [u8; 16],
}

impl FernetKey {
    pub(crate) fn from_encoded(encoded: &[u8]) -> Result<Self, FernetError> {
        let raw = b64(encoded).map_err(|_| FernetError::Key)?;
        if raw.len() != 32 {
            return Err(FernetError::Key);
        }
        let mut signing = [0u8; 16];
        let mut encryption = [0u8; 16];
        signing.copy_from_slice(&raw[..16]);
        encryption.copy_from_slice(&raw[16..]);
        Ok(Self { signing, encryption })
    }

    pub(crate) fn decrypt(&self, token: &[u8]) -> Result<Vec<u8>, FernetError> {
        let data = b64(token).map_err(|_| FernetError::Token)?;
        if data.len() < 1 + 8 + 16 + 16 + 32 || data[0] != 0x80 {
            return Err(FernetError::Token);
        }
        let (body, sig) = data.split_at(data.len() - 32);
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.signing).map_err(|_| FernetError::Key)?;
        mac.update(body);
        mac.verify_slice(sig).map_err(|_| FernetError::Signature)?;
        let iv = &body[9..25];
        let ciphertext = &body[25..];
        if ciphertext.is_empty() || ciphertext.len() % 16 != 0 {
            return Err(FernetError::Token);
        }
        let mut buf = ciphertext.to_vec();
        let plain = cbc::Decryptor::<aes::Aes128>::new(&self.encryption.into(), iv.into())
            .decrypt_padded_mut::<Pkcs7>(&mut buf)
            .map_err(|_| FernetError::Token)?
            .to_vec();
        buf.iter_mut().for_each(|b| *b = 0);
        Ok(plain)
    }
}

impl Drop for FernetKey {
    fn drop(&mut self) {
        self.signing.iter_mut().for_each(|b| *b = 0);
        self.encryption.iter_mut().for_each(|b| *b = 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generated with Python `cryptography.fernet` using a canary key and a
    // canary message; neither is a real secret.
    const KEY: &[u8] = b"CANARYKEY";

    #[test]
    fn rejects_bad_keys_and_tokens() {
        assert!(FernetKey::from_encoded(KEY).is_err());
        let key = FernetKey::from_encoded(b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").unwrap();
        assert_eq!(key.decrypt(b"not-a-token").unwrap_err(), FernetError::Token);
    }
}
