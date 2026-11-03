//! NetherNet server identity. Bedrock 26.60 ignores an SDP answer that lacks a signed `a=identity` line.
//! Format from go-nethernet `identity.go` (MIT): a self-signed ES384 JWT carrying our public key as a
//! JWK `cpk` claim, plus a detached ES384 JWS over our DTLS fingerprints. Domain "self" matches BDS.
use base64ct::{Base64, Base64UrlUnpadded, Encoding};
use p384::ecdsa::{Signature, SigningKey, signature::Signer};
use p384::pkcs8::EncodePublicKey;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

fn b64url(b: &[u8]) -> String {
    Base64UrlUnpadded::encode_string(b)
}

/// Returns (`header.payload`, `signature`) for a compact ES384 JWS.
fn jws(key: &SigningKey, header: &serde_json::Value, payload: &str) -> (String, String) {
    let input = format!("{}.{}", b64url(header.to_string().as_bytes()), b64url(payload.as_bytes()));
    let sig: Signature = key.sign(input.as_bytes());
    (input, b64url(&sig.to_bytes()))
}

pub fn new_key() -> SigningKey {
    loop {
        let mut b = [0u8; 48];
        getrandom::fill(&mut b).expect("OS random number generator");
        if let Ok(k) = SigningKey::from_slice(&b) {
            return k;
        }
    }
}

/// The value for `a=identity:` in an answer whose fingerprint line is `a=fingerprint:<algorithm> <digest>`.
pub fn attribute(key: &SigningKey, algorithm: &str, digest: &str) -> String {
    let vk = key.verifying_key();
    let der = vk.to_public_key_der().expect("P-384 public key encodes as DER");
    let point = vk.to_sec1_point(false);
    let p = point.as_bytes(); // 0x04 || x (48 bytes) || y (48 bytes)
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let claims = json!({
        "exp": now + 60,
        "iat": now,
        "cpk": {"kty": "EC", "crv": "P-384", "x": b64url(&p[1..49]), "y": b64url(&p[49..97])},
    });
    let header = json!({"alg": "ES384", "x5u": Base64::encode_string(der.as_bytes())});
    let (input, sig) = jws(key, &header, &claims.to_string());
    let token = format!("{input}.{sig}");

    // Must match the client's canonical payload byte for byte: {"fingerprint":[{"algorithm":..,"digest":..}]}
    let payload = json!({"fingerprint": [{"algorithm": algorithm, "digest": digest}]}).to_string();
    let (input, sig) = jws(key, &json!({"alg": "ES384"}), &payload);
    let fingerprints = format!("{}..{sig}", input.split('.').next().unwrap());

    let assertion = json!({"fingerprints": fingerprints, "token": token}).to_string();
    let identity = json!({"assertion": assertion, "idp": {"domain": "self", "protocol": "default"}});
    Base64::encode_string(identity.to_string().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use p384::ecdsa::signature::Verifier;

    fn verify(key: &SigningKey, signing_input: &str, sig_b64: &str) {
        let sig = Signature::from_slice(&Base64UrlUnpadded::decode_vec(sig_b64).unwrap()).unwrap();
        key.verifying_key().verify(signing_input.as_bytes(), &sig).expect("valid ES384 signature");
    }

    #[test]
    fn identity_signatures_verify() {
        let key = new_key();
        let attr = attribute(&key, "sha-256", "AB:CD");
        let outer: serde_json::Value = serde_json::from_slice(&Base64::decode_vec(&attr).unwrap()).unwrap();
        assert_eq!(outer["idp"], json!({"domain": "self", "protocol": "default"}));
        let a: serde_json::Value = serde_json::from_str(outer["assertion"].as_str().unwrap()).unwrap();

        let token: Vec<_> = a["token"].as_str().unwrap().split('.').collect();
        verify(&key, &format!("{}.{}", token[0], token[1]), token[2]);
        let claims: serde_json::Value = serde_json::from_slice(&Base64UrlUnpadded::decode_vec(token[1]).unwrap()).unwrap();
        assert_eq!(claims["cpk"]["crv"], "P-384");

        let fp: Vec<_> = a["fingerprints"].as_str().unwrap().split('.').collect();
        assert_eq!(fp[1], "", "detached payload");
        let payload = r#"{"fingerprint":[{"algorithm":"sha-256","digest":"AB:CD"}]}"#;
        verify(&key, &format!("{}.{}", fp[0], b64url(payload.as_bytes())), fp[2]);
    }
}
