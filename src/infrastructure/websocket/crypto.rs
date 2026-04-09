use aes::Aes256;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use cbc::cipher::block_padding::Pkcs7;

use crate::domain::error::KisError;

type Aes256CbcDec = cbc::Decryptor<Aes256>;

/// KIS WebSocket 체결통보 AES-256-CBC 복호화
///
/// key/iv는 구독 응답의 body.output.key/iv에서 수신한 문자열을
/// UTF-8 바이트로 직접 사용 (해싱 없음).
pub fn aes_cbc_base64_decrypt(key: &str, iv: &str, cipher_text: &str) -> Result<String, KisError> {
    let key_bytes = key.as_bytes();
    let iv_bytes = iv.as_bytes();

    if key_bytes.len() != 32 {
        return Err(KisError::Internal(format!(
            "AES key 길이 오류: {} (32바이트 필요)", key_bytes.len()
        )));
    }
    if iv_bytes.len() != 16 {
        return Err(KisError::Internal(format!(
            "AES IV 길이 오류: {} (16바이트 필요)", iv_bytes.len()
        )));
    }

    let encrypted = BASE64.decode(cipher_text)
        .map_err(|e| KisError::ParseError(format!("Base64 디코딩 실패: {e}")))?;

    let decryptor = Aes256CbcDec::new_from_slices(key_bytes, iv_bytes)
        .map_err(|e| KisError::Internal(format!("AES 초기화 실패: {e}")))?;

    let decrypted = decryptor
        .decrypt_padded_vec_mut::<Pkcs7>(&encrypted)
        .map_err(|e| KisError::Internal(format!("AES 복호화 실패: {e}")))?;

    String::from_utf8(decrypted)
        .map_err(|e| KisError::ParseError(format!("복호화 결과 UTF-8 변환 실패: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decrypt_round_trip() {
        // AES-256-CBC 암호화 후 복호화 검증
        use cbc::cipher::BlockEncryptMut;
        type Aes256CbcEnc = cbc::Encryptor<Aes256>;

        let key = "01234567890123456789012345678901"; // 32바이트
        let iv = "0123456789012345"; // 16바이트
        let plaintext = "hello^world^test";

        let encryptor = Aes256CbcEnc::new_from_slices(key.as_bytes(), iv.as_bytes()).unwrap();
        let encrypted = encryptor.encrypt_padded_vec_mut::<Pkcs7>(plaintext.as_bytes());
        let cipher_text = BASE64.encode(&encrypted);

        let result = aes_cbc_base64_decrypt(key, iv, &cipher_text).unwrap();
        assert_eq!(result, plaintext);
    }
}
