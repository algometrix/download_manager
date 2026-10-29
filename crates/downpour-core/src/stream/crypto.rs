//! HLS `METHOD=AES-128` decryption: AES-128-CBC with PKCS#7 padding over
//! each whole segment.

use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockDecryptMut, KeyIvInit};

pub const KEY_BYTES: usize = 16;

type Aes128CbcDecryptor = cbc::Decryptor<aes::Aes128>;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("the segment does not decrypt with its key (wrong key or damaged data)")]
pub struct DecryptError;

pub fn decrypt_segment(
    key: &[u8; KEY_BYTES],
    iv: &[u8; 16],
    mut ciphertext: Vec<u8>,
) -> Result<Vec<u8>, DecryptError> {
    let length = Aes128CbcDecryptor::new(key.into(), iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut ciphertext)
        .map_err(|_| DecryptError)?
        .len();
    ciphertext.truncate(length);
    Ok(ciphertext)
}

#[cfg(test)]
mod tests {
    use aes::cipher::BlockEncryptMut;

    use super::*;

    type Aes128CbcEncryptor = cbc::Encryptor<aes::Aes128>;

    #[test]
    fn round_trips_with_padding() {
        let key = [7u8; 16];
        let iv = 42u128.to_be_bytes();
        let plain = b"transport stream bytes that are not block aligned".to_vec();
        let encrypted = Aes128CbcEncryptor::new(&key.into(), &iv.into())
            .encrypt_padded_vec_mut::<Pkcs7>(&plain);
        assert_eq!(decrypt_segment(&key, &iv, encrypted).unwrap(), plain);
    }

    #[test]
    fn wrong_key_is_reported() {
        let iv = [0u8; 16];
        let encrypted = Aes128CbcEncryptor::new(&[1u8; 16].into(), &iv.into())
            .encrypt_padded_vec_mut::<Pkcs7>(b"secret");
        assert_eq!(
            decrypt_segment(&[2u8; 16], &iv, encrypted),
            Err(DecryptError)
        );
    }
}
