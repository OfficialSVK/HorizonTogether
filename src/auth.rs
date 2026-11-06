//! Checks who a Bedrock player is. A 26.x client logs in with a token from Minecraft's authorization service (an
//! OpenID token, RS256, whose keys the service publishes) naming the player's gamertag (`xname`), XUID (`xid`) and
//! the client's own public key (`cpk`); the login's client data must be signed with that key (ES384), which proves
//! the token wasn't copied from someone else's login. Issuer and keys: Minecraft's service discovery for
//! MinecraftPE 1.26.60 (the "auth" environment) and its `/.well-known/openid-configuration`.
use base64ct::{Base64, Base64UrlUnpadded, Encoding};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ISSUER: &str = "https://authorization.franchise.minecraft-services.net/";
const KEYS_URL: &str = "https://authorization.franchise.minecraft-services.net/.well-known/keys";

/// A verified player.
#[derive(Debug, PartialEq)]
pub struct Player {
    pub name: String,
    pub xuid: String,
}

/// Verifies a login: `token` (the auth JSON's Token) and `client_data` (the client data JWT after it).
pub fn verify(token: &str, client_data: &str) -> Result<Player, String> {
    check(token, client_data, now(), &|kid, msg, sig| {
        let (n, e) = key(kid)?;
        aws_lc_rs::signature::RsaPublicKeyComponents { n: &n[..], e: &e[..] }
            .verify(&aws_lc_rs::signature::RSA_PKCS1_2048_8192_SHA256, msg, sig)
            .map_err(|_| "bad token signature".to_string())
    })
}

/// Fetches the service's signing keys now, so the first login doesn't wait for them (and a network problem shows at
/// start).
pub fn prefetch() -> Result<usize, String> {
    let keys = fetch_keys()?;
    let n = keys.len();
    *KEYS.lock().unwrap() = (keys, Some(Instant::now()));
    Ok(n)
}

type Keys = HashMap<String, (Vec<u8>, Vec<u8>)>;
static KEYS: std::sync::LazyLock<Mutex<(Keys, Option<Instant>)>> = std::sync::LazyLock::new(|| Mutex::new((HashMap::new(), None)));

/// The RSA key (modulus, exponent) with this id, fetching the published keys again when it's unknown (they rotate);
/// at most once a minute. shortcut: the fetch blocks the caller (a login) for up to its timeout; rare, on rotation.
fn key(kid: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut keys = KEYS.lock().unwrap();
    if !keys.0.contains_key(kid) && keys.1.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
        let fetched = fetch_keys();
        keys.1 = Some(Instant::now());
        keys.0 = fetched?;
    }
    keys.0.get(kid).cloned().ok_or_else(|| format!("unknown signing key {kid}"))
}

fn fetch_keys() -> Result<Keys, String> {
    let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().new_agent();
    let body = (agent.get(KEYS_URL).call())
        .map_err(|e| format!("fetch {KEYS_URL}: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("read {KEYS_URL}: {e}"))?;
    let jwks: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("keys JSON: {e}"))?;
    let b64 = |v: &serde_json::Value| Base64UrlUnpadded::decode_vec(v.as_str()?.trim_end_matches('=')).ok();
    Ok((jwks["keys"].as_array().ok_or("no keys")?.iter())
        .filter(|k| k["kty"] == "RSA")
        .filter_map(|k| Some((k["kid"].as_str()?.to_string(), (b64(&k["n"])?, b64(&k["e"])?))))
        .collect())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A compact JWS's parts: header and claims (JSON), signing input, signature.
fn parts(jwt: &str) -> Result<(serde_json::Value, serde_json::Value, &str, Vec<u8>), String> {
    let mut it = jwt.splitn(3, '.');
    let (h, c, s) = (it.next().unwrap_or(""), it.next().ok_or("malformed JWT")?, it.next().ok_or("malformed JWT")?);
    let json = |p: &str| -> Result<serde_json::Value, String> {
        let b = Base64UrlUnpadded::decode_vec(p.trim_end_matches('=')).map_err(|e| format!("JWT base64: {e}"))?;
        serde_json::from_slice(&b).map_err(|e| format!("JWT JSON: {e}"))
    };
    let sig = Base64UrlUnpadded::decode_vec(s.trim_end_matches('=')).map_err(|e| format!("JWT signature: {e}"))?;
    Ok((json(h)?, json(c)?, &jwt[..h.len() + 1 + c.len()], sig))
}

type RsaCheck<'a> = &'a dyn Fn(&str, &[u8], &[u8]) -> Result<(), String>;

/// The checks themselves; `rsa` verifies an RS256 signature with the key of an id.
fn check(token: &str, client_data: &str, now: u64, rsa: RsaCheck) -> Result<Player, String> {
    let (header, claims, input, sig) = parts(token)?;
    if header["alg"] != "RS256" {
        return Err(format!("token algorithm {}", header["alg"]));
    }
    rsa(header["kid"].as_str().ok_or("token has no key id")?, input.as_bytes(), &sig)?;
    if claims["iss"].as_str().map(|i| i.trim_end_matches('/')) != Some(ISSUER.trim_end_matches('/')) {
        return Err(format!("token issuer {}", claims["iss"]));
    }
    // A minute's grace for clocks that disagree.
    if claims["exp"].as_u64().is_none_or(|exp| exp + 60 < now) {
        return Err("token expired".into());
    }
    if claims["nbf"].as_u64().is_some_and(|nbf| nbf > now + 60) {
        return Err("token not valid yet".into());
    }
    let name = claims["xname"].as_str().filter(|n| !n.is_empty()).ok_or("token has no gamertag")?;
    let xuid = claims["xid"].as_str().filter(|x| !x.is_empty()).ok_or("token has no XUID")?;

    // The client data is signed with the token's client key.
    use p384::ecdsa::{Signature, VerifyingKey, signature::Verifier};
    use p384::pkcs8::DecodePublicKey;
    let cpk = Base64::decode_vec(claims["cpk"].as_str().ok_or("token has no client key")?).map_err(|e| format!("client key: {e}"))?;
    let key = VerifyingKey::from_public_key_der(&cpk).map_err(|e| format!("client key: {e}"))?;
    let (data_header, _, data_input, data_sig) = parts(client_data)?;
    if data_header["alg"] != "ES384" {
        return Err(format!("client data algorithm {}", data_header["alg"]));
    }
    let data_sig = Signature::from_slice(&data_sig).map_err(|_| "client data signature".to_string())?;
    key.verify(data_input.as_bytes(), &data_sig).map_err(|_| "client data not signed by the token's key".to_string())?;
    Ok(Player { name: name.to_string(), xuid: xuid.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use p384::ecdsa::{SigningKey, signature::Signer};
    use p384::pkcs8::EncodePublicKey;
    use serde_json::json;

    fn b64(v: &serde_json::Value) -> String {
        Base64UrlUnpadded::encode_string(v.to_string().as_bytes())
    }

    /// An "RS256" token whose signature is `ok` or `bad` (the RSA check is stubbed), and client data signed by `key`.
    fn login(claims: serde_json::Value, key: &SigningKey) -> (String, String) {
        let token = format!("{}.{}.{}", b64(&json!({"alg": "RS256", "kid": "k1"})), b64(&claims), Base64UrlUnpadded::encode_string(b"ok"));
        let input = format!("{}.{}", b64(&json!({"alg": "ES384"})), b64(&json!({"SkinId": "x"})));
        let sig: p384::ecdsa::Signature = key.sign(input.as_bytes());
        (token, format!("{input}.{}", Base64UrlUnpadded::encode_string(&sig.to_bytes())))
    }

    #[test]
    fn checks_token_and_client_data() {
        let rsa: RsaCheck = &|kid, _, sig| if kid == "k1" && sig == b"ok" { Ok(()) } else { Err("bad token signature".into()) };
        let key = crate::identity::new_key();
        let cpk = Base64::encode_string(key.verifying_key().to_public_key_der().unwrap().as_bytes());
        let claims = |exp: u64, iss: &str| json!({"iss": iss, "exp": exp, "xname": "Steve", "xid": "2535", "cpk": cpk});
        let (token, data) = login(claims(1000, ISSUER), &key);
        assert_eq!(check(&token, &data, 900, rsa), Ok(Player { name: "Steve".into(), xuid: "2535".into() }));
        assert_eq!(check(&token, &data, 2000, rsa), Err("token expired".into()));
        let (token2, _) = login(claims(1000, "https://evil.example/"), &key);
        assert!(check(&token2, &data, 900, rsa).unwrap_err().starts_with("token issuer"));
        // Client data from another key: a copied token doesn't log in.
        let (_, other) = login(claims(1000, ISSUER), &crate::identity::new_key());
        assert_eq!(check(&token, &other, 900, rsa), Err("client data not signed by the token's key".into()));
        // A forged token signature.
        let forged = format!("{}.{}", token.rsplit_once('.').unwrap().0, Base64UrlUnpadded::encode_string(b"no"));
        assert_eq!(check(&forged, &data, 900, rsa), Err("bad token signature".into()));
    }

    /// Fetches the live keys (needs the internet).
    #[test]
    #[ignore]
    fn fetches_live_keys() {
        let n = prefetch().unwrap();
        assert!(n > 0);
        println!("{n} signing keys");
    }
}
