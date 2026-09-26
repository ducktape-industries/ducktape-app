//! Getting a device's key onto an account without a passkey. Identity's
//! `AddKey` is signed by the key joining and consented to by a key already
//! on the account, of any scheme; so any of the account's keys can let a
//! new one in.
//!
//! - **Another device.** The new device shows a short code; the person
//!   types it on a device already on the account, compares a fingerprint,
//!   and approves. The two never meet: they pass the request and the
//!   consent through the auth host's relay (`/r/<id>`, one-shot, 300 s),
//!   in two slots named from the code. What rides there is public (a public
//!   key, a signature over it): a forged request is caught by the
//!   fingerprint, a forged consent by the node.
//! - **A recovery key.** An extra key on the account whose only copy is its
//!   24 words on paper. Made from a device already on the account (this
//!   device consents to it); typed on a new device, it consents to that
//!   device's key. A phrase a password-locked key was minted with (before
//!   keys moved into the OS) is such a key already.

use commonware_codec::DecodeExt as _;
use commonware_cryptography::{Signer as _, ed25519};
use identity::{Admission, CONSENT_NAMESPACE, Consent, Op, Query, Reply};
use sha2::{Digest as _, Sha256};

use super::noded::Frame;
use super::passkey::{
    ask, auth_page, expires_at, generation, get, person, submit, submit_seated, url_encode,
};
use super::{RpcClient, hex_decode, hex_encode, next_seq, seated_key, seated_sign};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;

/// How long a new device waits for the other to approve: the relay's own
/// hold.
pub(crate) const WAIT: std::time::Duration = std::time::Duration::from_secs(300);
const POLL: std::time::Duration = std::time::Duration::from_millis(1500);

/// Crockford's base32: no I, L, O, U to misread.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A fresh code to type on the other device: 8 characters, 40 bits,
/// "KQ4M-9XPT".
pub(crate) fn new_code() -> String {
    let bits: u64 = rand::random::<u64>() >> 24;
    let chars: String = (0..8)
        .map(|nth| ALPHABET[((bits >> (35 - nth * 5)) & 31) as usize] as char)
        .collect();
    format!("{}-{}", &chars[..4], &chars[4..])
}

/// A typed code as the slots name it: case, spaces and dashes dropped, and
/// the letters people confuse with digits read as those digits.
fn normalized(code: &str) -> Option<String> {
    let folded: String = code
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .collect();
    (folded.len() == 8 && folded.bytes().all(|b| ALPHABET.contains(&b))).then_some(folded)
}

/// A key's fingerprint, to compare by eye on both screens: "A1F3 09C2".
pub(crate) fn fingerprint(key: &[u8]) -> String {
    let digest = Sha256::digest(key);
    let hex: String = digest[..4]
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect();
    format!("{} {}", &hex[..4], &hex[4..])
}

/// The relay slot for one side of a code: `request` (new → old) or
/// `consent` (old → new).
fn slot(page: &str, code: &str, side: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(page)
        .map_err(|error| format!("The auth host address is unusable: {error}"))?;
    let mut hash = Sha256::new();
    hash.update(b"ducktape:link:v1\0");
    hash.update(side.as_bytes());
    hash.update(b"\0");
    hash.update(code.as_bytes());
    url.set_path(&format!("/r/{}", B64.encode(hash.finalize())));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

async fn post(url: &str, json: String) -> Result<(), String> {
    let reply = reqwest::Client::new()
        .post(url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("result={}", url_encode(&json)))
        .send()
        .await
        .map_err(|_| {
            "Can't reach the auth host. Check the connection and try again.".to_string()
        })?;
    match reply.status().is_success() {
        true => Ok(()),
        false => Err(format!(
            "The auth host refused the message ({}).",
            reply.status()
        )),
    }
}

/// One GET of a slot: its message, or `None` while nothing has arrived.
async fn take(url: &str) -> Result<Option<String>, String> {
    let reply = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .map_err(|_| "Can't reach the auth host.".to_string())?;
    match reply.status() {
        reqwest::StatusCode::OK => reply
            .text()
            .await
            .map(Some)
            .map_err(|_| "Lost the message on the way.".to_string()),
        reqwest::StatusCode::NO_CONTENT => Ok(None),
        status => Err(format!("The auth host refused ({status}).")),
    }
}

/// A new device asking to join: which network, and its key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) network: String,
    pub(crate) key: Vec<u8>,
}

// ---------- the new device ----------

/// Puts this device's (seated) key up under `code`, then waits for a
/// device on the account to consent, and joins with that consent.
pub(crate) async fn join_from_device(
    client: &RpcClient,
    network: &str,
    code: &str,
) -> Result<(), String> {
    let page = auth_page();
    let code = normalized(code).ok_or("That code is malformed.")?;
    let device = seated_key().await.map_err(|refusal| refusal.message)?;
    let request = serde_json::json!({ "v": 1, "network": network, "key": hex_encode(&device) });
    post(&slot(&page, &code, "request")?, request.to_string()).await?;
    let answer = slot(&page, &code, "consent")?;
    let deadline = tokio::time::Instant::now() + WAIT;
    let json = loop {
        if let Some(json) = take(&answer).await? {
            break json;
        }
        if tokio::time::Instant::now() > deadline {
            return Err("No device approved in time. Start again for a new code.".into());
        }
        tokio::time::sleep(POLL).await;
    };
    let consent = parse_consent(&json)?;
    let add = Op::AddKey {
        scheme: abi::Scheme::Ed25519,
        label: Some("Desktop".into()),
        consent,
    };
    submit_seated(client, network, &add).await.map(drop)
}

fn parse_consent(json: &str) -> Result<Consent, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|_| "The other device's answer is unreadable.")?;
    let hex = |field: &str| {
        value[field]
            .as_str()
            .and_then(|text| hex_decode(text).ok())
            .ok_or_else(|| format!("The other device's answer has no {field}."))
    };
    Ok(Consent {
        key: hex("key")?,
        account: value["account"]
            .as_u64()
            .ok_or("The other device's answer names no account.")?,
        expires_at: value["expires_at"]
            .as_u64()
            .ok_or("The other device's answer has no deadline.")?,
        proof: hex("proof")?,
    })
}

// ---------- a device already on the account ----------

/// The request a new device put up under `code`. Taking it empties the
/// slot: one code, one look.
pub(crate) async fn find_request(code: &str) -> Result<Request, String> {
    let code = normalized(code).ok_or("A code is 8 letters and digits, like KQ4M-9XPT.")?;
    let json = take(&slot(&auth_page(), &code, "request")?)
        .await?
        .ok_or("No device is waiting on that code. Check it, or start again on the new device.")?;
    let value: serde_json::Value =
        serde_json::from_str(&json).map_err(|_| "The new device's request is unreadable.")?;
    let key = value["key"]
        .as_str()
        .and_then(|text| hex_decode(text).ok())
        .filter(|key| key.len() == 32)
        .ok_or("The new device's request has no key.")?;
    Ok(Request {
        network: value["network"].as_str().unwrap_or_default().to_owned(),
        key,
    })
}

/// This device (seated, on `account`) consents to `request`'s key joining,
/// and hands the consent to the new device under `code`.
pub(crate) async fn approve(
    client: &RpcClient,
    network: &str,
    account: u64,
    code: &str,
    request: &Request,
) -> Result<(), String> {
    if request.network != network {
        return Err(format!(
            "That device is on {}, not {network}. Approve it from a device on {}.",
            request.network, request.network
        ));
    }
    let code = normalized(code).ok_or("That code is malformed.")?;
    let admission = Admission {
        network: network.as_bytes().to_vec(),
        scheme: abi::Scheme::Ed25519,
        key: request.key.clone(),
        generation: generation(client, network, &request.key).await?,
        account,
        expires_at: expires_at(),
    };
    let (key, proof) = seated_sign(CONSENT_NAMESPACE, &admission.preimage())
        .await
        .map_err(|refusal| refusal.message)?;
    let consent = serde_json::json!({
        "v": 1,
        "account": account,
        "key": hex_encode(&key),
        "expires_at": admission.expires_at,
        "proof": hex_encode(&proof),
    });
    post(&slot(&auth_page(), &code, "consent")?, consent.to_string()).await
}

// ---------- recovery keys ----------

/// 24 fresh words: a recovery key not yet on any account.
pub(crate) fn new_recovery_phrase() -> String {
    keystore::userkey::mnemonic_of_seed(&rand::random::<[u8; 32]>())
}

fn key_of_phrase(phrase: &str) -> Result<ed25519::PrivateKey, String> {
    let seed = zeroize::Zeroizing::new(
        keystore::userkey::seed_of_mnemonic(phrase)
            .map_err(|_| "Those words aren't a recovery key — check each one.".to_string())?,
    );
    ed25519::PrivateKey::decode(seed.as_slice()).map_err(|_| "Those words make no key.".to_string())
}

async fn account_of(
    client: &RpcClient,
    network: &str,
    key: Vec<u8>,
) -> Result<Option<u64>, String> {
    match ask(client, network, Query::OfKey { key }).await? {
        Reply::Number(number) => Ok(number),
        _ => Err("identity answered something other than an account number".into()),
    }
}

/// Puts the key `phrase` makes on the account this device's (seated) key
/// holds: this device consents, the recovery key signs its own `AddKey`.
pub(crate) async fn add_recovery_key(
    client: &RpcClient,
    network: &str,
    phrase: &str,
) -> Result<(), String> {
    let recovery = key_of_phrase(phrase)?;
    let public = recovery.public_key().as_ref().to_vec();
    let device = seated_key().await.map_err(|refusal| refusal.message)?;
    let account = account_of(client, network, device)
        .await?
        .ok_or("This device's key holds no account yet.")?;
    person(&get(client, network, account).await?)?;
    let admission = Admission {
        network: network.as_bytes().to_vec(),
        scheme: abi::Scheme::Ed25519,
        key: public.clone(),
        generation: generation(client, network, &public).await?,
        account,
        expires_at: expires_at(),
    };
    let (key, proof) = seated_sign(CONSENT_NAMESPACE, &admission.preimage())
        .await
        .map_err(|refusal| refusal.message)?;
    let add = Op::AddKey {
        scheme: abi::Scheme::Ed25519,
        label: Some("Recovery key".into()),
        consent: Consent {
            key,
            account,
            expires_at: admission.expires_at,
            proof,
        },
    };
    let seq = next_seq(client, &public)
        .await
        .map_err(|refusal| refusal.message)?;
    let frame = Frame::sign(
        &recovery,
        network.as_bytes(),
        seq,
        identity::MODULE,
        abi::encode(&add),
    );
    submit(client, frame.encode()).await.map(drop)
}

/// This device's (seated) key joins the account the key `phrase` makes is
/// on; that key consents.
pub(crate) async fn join_with_recovery_key(
    client: &RpcClient,
    network: &str,
    phrase: &str,
) -> Result<(), String> {
    let recovery = key_of_phrase(phrase)?;
    let account = account_of(client, network, recovery.public_key().as_ref().to_vec())
        .await?
        .ok_or_else(|| format!("That recovery key isn't on an account on {network}."))?;
    person(&get(client, network, account).await?)?;
    let device = seated_key().await.map_err(|refusal| refusal.message)?;
    let admission = Admission {
        network: network.as_bytes().to_vec(),
        scheme: abi::Scheme::Ed25519,
        key: device.clone(),
        generation: generation(client, network, &device).await?,
        account,
        expires_at: expires_at(),
    };
    let proof = recovery.sign(CONSENT_NAMESPACE, &admission.preimage());
    let add = Op::AddKey {
        scheme: abi::Scheme::Ed25519,
        label: Some("Desktop".into()),
        consent: Consent {
            key: recovery.public_key().as_ref().to_vec(),
            account,
            expires_at: admission.expires_at,
            proof: proof.as_ref().to_vec(),
        },
    };
    submit_seated(client, network, &add).await.map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_reads_back_however_it_is_typed() {
        for _ in 0..50 {
            let code = new_code();
            assert_eq!(code.len(), 9);
            let folded = normalized(&code).expect("its own code parses");
            assert_eq!(
                normalized(&code.to_lowercase().replace('-', " ")),
                Some(folded)
            );
        }
        assert_eq!(
            normalized("kq4m-9xpo"),
            Some("KQ4M9XP0".into()),
            "O reads as 0"
        );
        assert_eq!(normalized("KQ4M-9XP"), None);
        assert_eq!(normalized("KQ4M-9XPU"), None, "U is not in the alphabet");
    }

    #[test]
    fn the_two_sides_of_a_code_are_different_slots() {
        let page = "https://auth.ducktape.industries/";
        let request = slot(page, "KQ4M9XPT", "request").unwrap();
        let consent = slot(page, "KQ4M9XPT", "consent").unwrap();
        assert_ne!(request, consent);
        // the relay takes 43-character ids only
        let id = request.rsplit('/').next().unwrap();
        assert_eq!(id.len(), 43);
        assert!(request.starts_with("https://auth.ducktape.industries/r/"));
    }

    #[test]
    fn a_consent_survives_the_trip() {
        let json = serde_json::json!({
            "v": 1, "account": 17, "key": hex_encode(&[1, 2]), "expires_at": 99, "proof": hex_encode(&[3]),
        })
        .to_string();
        let consent = parse_consent(&json).unwrap();
        assert_eq!((consent.account, consent.expires_at), (17, 99));
        assert_eq!((consent.key, consent.proof), (vec![1, 2], vec![3]));
        assert!(parse_consent("{}").is_err());
    }

    #[test]
    fn a_recovery_phrase_makes_one_key_and_fingerprints_are_short() {
        let phrase = new_recovery_phrase();
        assert_eq!(phrase.split_whitespace().count(), 24);
        let a = key_of_phrase(&phrase).unwrap().public_key();
        let b = key_of_phrase(&phrase).unwrap().public_key();
        assert_eq!(a, b);
        assert!(key_of_phrase("not words").is_err());
        assert_eq!(fingerprint(a.as_ref()).len(), 9);
    }
}
