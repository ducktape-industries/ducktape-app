//! Passkeys: an ACCOUNT's key, held by the person's authenticator. This
//! device's ed25519 key still signs every write; a passkey only creates the
//! account with it, and admits a new device's key into it later.
//!
//! The app speaks no WebAuthn. The ceremony runs on the auth page
//! ([`AUTH_PAGE`], RP ID = its host) in the system browser: the request
//! rides the URL fragment, and the result comes back as a top-level form
//! POST (`result=<JSON>`) to a one-shot loopback [`Listener`]. The contract
//! is core's `ops/auth-page/README.md` (b690a31bf^); the verifier every
//! answer must satisfy is `keyscheme` (`Secp256r1` = the assertion envelope
//! `authenticatorData ‖ clientDataJSON ‖ sig64`, challenge
//! `SHA-256(ns ‖ preimage)`).
//!
//! The contract's `user.id` names the account NUMBER, which the identity
//! program assigns at `Create`. So a passkey account is created by this
//! device's key, and the passkey joins it next: touch 1 `create`s the
//! passkey, touch 2 signs its own `AddKey` frame (the device key consents).
//! A new device is two touches too: touch 1 asks the passkey which account
//! it holds, touch 2 consents to this device's key joining that account.
//!
//! A passkey on a phone: every touch also offers its request as a QR
//! ([`Phone`]) whose callback is the auth host's relay slot `/r/<id>`
//! (README §Relay: POST stores `result`, GET hands it out once, 204 until
//! then). Once the person picks the phone, touches stop opening this
//! device's browser and the app polls the slot.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use identity::{Admission, CONSENT_NAMESPACE, Consent, Control, Op, Query, Reply};
use keyscheme::KeyScheme;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};

use super::noded::{Body, FRAME_NAMESPACE, Frame, Layer};
use super::{RpcClient, next_seq, query_frame, seated_frame, seated_key, seated_sign};

/// The live page. Its host IS the RP ID every passkey is scoped to.
pub(crate) const AUTH_PAGE: &str = "https://auth.ducktape.industries/";

/// How long one touch may take before the app stops waiting.
pub(crate) const CEREMONY_TIMEOUT: Duration = Duration::from_secs(300);

/// How often a touch on the phone asks the auth host for its answer.
const RELAY_POLL: Duration = Duration::from_millis(1500);

/// How long a consent stays good, in the node's milliseconds.
const CONSENT_TTL_MS: u64 = 15 * 60 * 1000;

pub(super) fn auth_page() -> String {
    std::env::var("DUCKTAPE_AUTH_PAGE").unwrap_or_else(|_| AUTH_PAGE.to_owned())
}

// ---------- the flows ----------

/// A new account held by a passkey, with this device's (seated) key on it
/// too. Retry-safe: a device key that already holds an account keeps it,
/// and the passkey joins that one.
pub(crate) async fn create_account(
    client: &RpcClient,
    network: &str,
    name: &str,
    phone: &Phone,
) -> Result<(), String> {
    let device = seated_key().await.map_err(|refusal| refusal.message)?;
    let number = match ask(
        client,
        network,
        Query::OfKey {
            key: device.clone(),
        },
    )
    .await?
    {
        Reply::Number(Some(number)) => {
            person(&get(client, network, number).await?)?;
            number
        }
        _ => create_seated(client, network, name).await?,
    };
    let passkey = created(
        Request::Create {
            user: user_handle(network, number),
            name: format!("{name} · {network}"),
        },
        phone,
    )
    .await?;
    let admission = Admission {
        network: network.as_bytes().to_vec(),
        scheme: abi::Scheme::Secp256r1,
        key: passkey.clone(),
        generation: generation(client, network, &passkey).await?,
        account: number,
        expires_at: expires_at(),
    };
    let (device, proof) = seated_sign(CONSENT_NAMESPACE, &admission.preimage())
        .await
        .map_err(|refusal| refusal.message)?;
    let add = Op::AddKey {
        scheme: abi::Scheme::Secp256r1,
        label: Some("Passkey".into()),
        consent: Consent {
            key: device,
            account: number,
            expires_at: admission.expires_at,
            proof,
        },
    };
    let seq = next_seq(client, &passkey)
        .await
        .map_err(|refusal| refusal.message)?;
    let body = passkey_body(&passkey, network, seq, abi::encode(&add));
    let assertion = asserted(
        keyscheme::webauthn_challenge(FRAME_NAMESPACE, &body.preimage()),
        phone,
    )
    .await?;
    let frame = passkey_frame(body, &assertion)
        .ok_or("That was a different passkey than the one just made. Choose the new one.")?;
    submit(client, frame.encode()).await.map(drop)
}

/// A self-serve account on `network` whose one key is this device's
/// (seated) key: identity's `Create`, no passkey. Its number and name, as
/// the rail shows them. A key that already holds an account (a retry after
/// a lost answer) is not an error here: that account is the answer.
pub(crate) async fn create_plain_account(
    client: &RpcClient,
    network: &str,
    name: &str,
) -> Result<(u64, String), String> {
    let device = seated_key().await.map_err(|refusal| refusal.message)?;
    if let Some(account) = account_of_key(client, network, device).await? {
        return Ok(account);
    }
    let number = create_seated(client, network, name).await?;
    Ok((number, name.trim().to_owned()))
}

async fn create_seated(client: &RpcClient, network: &str, name: &str) -> Result<u64, String> {
    let create = Op::Create {
        name: name.to_owned(),
        scheme: abi::Scheme::Ed25519,
    };
    let output = submit_seated(client, network, &create).await?;
    abi::decode::<u64>(&output).map_err(|refusal| refusal.sentence)
}

/// This device's (seated) key joins the account a passkey holds.
pub(crate) async fn sign_in(
    client: &RpcClient,
    network: &str,
    phone: &Phone,
) -> Result<(), String> {
    let hint = asserted(rand::random(), phone).await?;
    let number = account_of_handle(network, hint.user_handle.as_deref())?;
    let account = get(client, network, number).await?;
    person(&account)?;
    let device = seated_key().await.map_err(|refusal| refusal.message)?;
    let admission = Admission {
        network: network.as_bytes().to_vec(),
        scheme: abi::Scheme::Ed25519,
        key: device.clone(),
        generation: generation(client, network, &device).await?,
        account: number,
        expires_at: expires_at(),
    };
    let preimage = admission.preimage();
    let consent = asserted(
        keyscheme::webauthn_challenge(CONSENT_NAMESPACE, &preimage),
        phone,
    )
    .await?;
    let proof = consent.proof();
    let key = consenting_key(&account, &preimage, &proof)
        .ok_or("That passkey isn't on this account. Use the same passkey both times.")?;
    let add = Op::AddKey {
        scheme: abi::Scheme::Ed25519,
        label: Some("Desktop".into()),
        consent: Consent {
            key,
            account: number,
            expires_at: admission.expires_at,
            proof,
        },
    };
    submit_seated(client, network, &add).await.map(drop)
}

/// The account `number` on `network`.
pub(super) async fn get(
    client: &RpcClient,
    network: &str,
    number: u64,
) -> Result<identity::Account, String> {
    match ask(client, network, Query::Get { number }).await? {
        Reply::Account(Some(account)) => Ok(account),
        _ => Err(format!("This names an account {network} does not have.")),
    }
}

/// A passkey flow is a person's: their own keys join their own account. An
/// agent's keys are its manager's to add, and a module's account holds
/// none, so a device key that holds either is told so instead of sending
/// an `AddKey` identity would refuse.
pub(super) fn person(account: &identity::Account) -> Result<(), String> {
    let name = &account.card.name;
    match &account.control {
        Control::Person { .. } => Ok(()),
        Control::Managed { manager, .. } => Err(format!(
            "This key belongs to {name} (account {}), an agent managed by account {manager}. \
             Passkeys are for a person's own account: sign in with a person's key.",
            account.number
        )),
        Control::Module { module } => Err(format!(
            "This key belongs to the account of the module {module}, which takes no passkey."
        )),
    }
}

/// Which of `account`'s passkeys signed `proof` over the consent `preimage`
/// — the page does not say which credential answered.
fn consenting_key(account: &identity::Account, preimage: &[u8], proof: &[u8]) -> Option<Vec<u8>> {
    account
        .keys()
        .iter()
        .filter(|held| held.scheme == abi::Scheme::Secp256r1)
        .find(|held| KeyScheme::Secp256r1.verify(&held.key, CONSENT_NAMESPACE, preimage, proof))
        .map(|held| held.key.clone())
}

/// A frame body whose signer is the passkey `pubkey`.
fn passkey_body(pubkey: &[u8], network: &str, seq: u64, payload: Vec<u8>) -> Body {
    Body {
        scheme: KeyScheme::Secp256r1,
        signer: pubkey.to_vec(),
        network: network.as_bytes().to_vec(),
        seq,
        target: identity::MODULE.to_owned(),
        payload,
    }
}

/// The frame `body` completed by `assertion`, or `None` when the assertion
/// is not the body's signer signing it (another passkey answered).
fn passkey_frame(body: Body, assertion: &Assertion) -> Option<Frame> {
    let proof = assertion.proof();
    KeyScheme::Secp256r1
        .verify(&body.signer, FRAME_NAMESPACE, &body.preimage(), &proof)
        .then_some(Frame { body, proof })
}

// ---------- the node ----------

pub(super) async fn ask(client: &RpcClient, network: &str, query: Query) -> Result<Reply, String> {
    let frame = query_frame(network, identity::MODULE, abi::encode(&query)).await;
    let reply = client
        .query(Layer::Preconfirmed, frame)
        .await
        .map_err(|error| node_error(error.to_string()))?;
    abi::decode(&reply).map_err(|refusal| refusal.sentence)
}

/// The account `key` belongs to on `network` — its number and name — or
/// `None` while the key holds no account there. The rail's foot reads it.
pub(crate) async fn account_of_key(
    client: &RpcClient,
    network: &str,
    key: Vec<u8>,
) -> Result<Option<(u64, String)>, String> {
    let number = match ask(client, network, Query::OfKey { key }).await? {
        Reply::Number(Some(number)) => number,
        _ => return Ok(None),
    };
    match ask(client, network, Query::Get { number }).await? {
        Reply::Account(Some(account)) => Ok(Some((number, account.card.name))),
        _ => Ok(None),
    }
}

pub(super) async fn generation(
    client: &RpcClient,
    network: &str,
    key: &[u8],
) -> Result<u64, String> {
    match ask(client, network, Query::Generation { key: key.to_vec() }).await? {
        Reply::Generation(generation) => Ok(generation),
        _ => Err("identity answered something other than a generation".into()),
    }
}

/// A consent's deadline, against block time (unix ms). The node's status
/// names only its genesis time, so this reads this device's clock; the TTL
/// absorbs ordinary skew.
pub(super) fn expires_at() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default();
    now + CONSENT_TTL_MS
}

pub(super) async fn submit_seated(
    client: &RpcClient,
    network: &str,
    op: &Op,
) -> Result<Vec<u8>, String> {
    let frame = seated_frame(client, network, identity::MODULE, abi::encode(op))
        .await
        .map_err(|refusal| refusal.message)?;
    submit(client, frame).await
}

pub(super) async fn submit(client: &RpcClient, frame: Vec<u8>) -> Result<Vec<u8>, String> {
    let receipt = client
        .submit(frame)
        .await
        .map_err(|error| node_error(error.to_string()))?;
    match receipt.outcome {
        abi::Outcome::Applied { output } => Ok(output),
        abi::Outcome::Rejected(refusal) => Err(rejected(refusal)),
    }
}

fn rejected(refusal: abi::Refusal) -> String {
    // identity's only already_exists is "this key already belongs to an account".
    if refusal.reason == abi::reason::ALREADY_EXISTS {
        return "This device's key already belongs to an account. Unlock it instead.".into();
    }
    node_error(refusal.sentence)
}

fn node_error(sentence: String) -> String {
    // ponytail: identity refuses an expired consent as a generic `unauthorized`,
    // so the sentence is the only tell until it gets its own token.
    if sentence.contains("expired") {
        return "That took too long and the consent expired. Try again.".into();
    }
    super::user_error(sentence)
}

// ---------- the account a passkey names ----------

/// `user.id`: `SHA-256("ducktape:passkey-account:v1\0" ‖ network)` ‖ the
/// account number, u64 LE.
fn user_handle(network: &str, number: u64) -> [u8; 40] {
    let mut handle = [0; 40];
    handle[..32].copy_from_slice(&chain_hash(network));
    handle[32..].copy_from_slice(&number.to_le_bytes());
    handle
}

fn chain_hash(network: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"ducktape:passkey-account:v1\0");
    hash.update(network.as_bytes());
    hash.finalize().into()
}

/// The account a passkey's (unsigned) `userHandle` names on `network`. A
/// hint only: the consent that follows must verify against a key the
/// account holds.
fn account_of_handle(network: &str, handle: Option<&[u8]>) -> Result<u64, String> {
    let handle = handle
        .and_then(|bytes| <&[u8; 40]>::try_from(bytes).ok())
        .ok_or("That passkey wasn't made by Ducktape. Choose a Ducktape passkey.")?;
    if handle[..32] != chain_hash(network) {
        return Err(format!(
            "That passkey belongs to another network. Choose one for {network}."
        ));
    }
    Ok(u64::from_le_bytes(
        handle[32..].try_into().expect("8 bytes"),
    ))
}

// ---------- the page ----------

enum Request {
    /// `navigator.credentials.create()`; the challenge is pass-through.
    Create { user: [u8; 40], name: String },
    /// `navigator.credentials.get()` with `allowCredentials: []`.
    Get { challenge: [u8; 32] },
}

fn request_url(page: &str, request: &Request, callback: &str) -> String {
    let fields = match request {
        Request::Create { user, name } => format!(
            "op=create&challenge={}&user={}&name={}",
            B64.encode(rand::random::<[u8; 32]>()),
            B64.encode(user),
            url_encode(name)
        ),
        Request::Get { challenge } => format!("op=get&challenge={}", B64.encode(challenge)),
    };
    format!("{page}#{fields}&cb={}", url_encode(callback))
}

pub(super) fn url_encode(value: &str) -> String {
    value
        .bytes()
        .map(
            |byte| match byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                true => (byte as char).to_string(),
                false => format!("%{byte:02X}"),
            },
        )
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// the 33-byte compressed SEC1 point
    Created(Vec<u8>),
    Asserted(Assertion),
}

#[derive(Debug, PartialEq, Eq)]
struct Assertion {
    authenticator_data: Vec<u8>,
    client_data_json: Vec<u8>,
    /// raw `R‖S`
    signature: Vec<u8>,
    user_handle: Option<Vec<u8>>,
}

impl Assertion {
    /// `keyscheme`'s `Secp256r1` proof bytes.
    fn proof(&self) -> Vec<u8> {
        keyscheme::webauthn_proof(
            &self.authenticator_data,
            &self.client_data_json,
            &self.signature,
        )
    }
}

fn parse_result(json: &str) -> Result<Outcome, String> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|_| "The browser sent back something unreadable. Try again.".to_string())?;
    if let Some(error) = value["error"].as_str() {
        return Err(ceremony_error(error));
    }
    let binary = |field: &str| {
        value[field]
            .as_str()
            .and_then(|text| B64.decode(text).ok())
            .ok_or_else(|| format!("The browser's answer is missing {field}. Try again."))
    };
    match value["op"].as_str() {
        Some("create") => {
            let key = binary("publicKey")?;
            match KeyScheme::Secp256r1.pubkey_wellformed(&key) {
                true => Ok(Outcome::Created(key)),
                false => Err("The browser returned a key Ducktape can't use.".into()),
            }
        }
        Some("get") => Ok(Outcome::Asserted(Assertion {
            authenticator_data: binary("authenticatorData")?,
            client_data_json: binary("clientDataJSON")?,
            signature: binary("signature")?,
            user_handle: binary("userHandle").ok(),
        })),
        _ => Err("The browser's answer names no step. Try again.".into()),
    }
}

/// A sentence for the page's `DOMException` name.
fn ceremony_error(name: &str) -> String {
    match name {
        "NotAllowedError" | "AbortError" => {
            "The passkey step was cancelled or timed out. Try again.".into()
        }
        "InvalidStateError" => {
            "That authenticator already holds a passkey for this account.".into()
        }
        "SecurityError" => "The browser refused a passkey on this page.".into(),
        "NotSupportedError" => "This browser or authenticator doesn't support passkeys.".into(),
        other => format!("The passkey step failed ({other}). Try again."),
    }
}

async fn created(request: Request, phone: &Phone) -> Result<Vec<u8>, String> {
    match ceremony(request, phone).await? {
        Outcome::Created(key) => Ok(key),
        Outcome::Asserted(_) => Err("The browser answered a different step. Try again.".into()),
    }
}

async fn asserted(challenge: [u8; 32], phone: &Phone) -> Result<Assertion, String> {
    match ceremony(Request::Get { challenge }, phone).await? {
        Outcome::Asserted(assertion) => Ok(assertion),
        Outcome::Created(_) => Err("The browser answered a different step. Try again.".into()),
    }
}

/// One touch: offer `request` as a QR for a phone, open it in this
/// device's browser unless the phone was picked, and take whichever answer
/// comes first.
async fn ceremony(request: Request, phone: &Phone) -> Result<Outcome, String> {
    let page = auth_page();
    let listener = Listener::bind()
        .await
        .map_err(|error| format!("Couldn't listen for the browser: {error}"))?;
    let relay = Relay::at(&page)?;
    phone.show(request_url(&page, &request, &relay.url));
    if !phone.chosen() {
        open_browser(&request_url(&page, &request, &listener.callback_url()))?;
    }
    answer(listener, relay, phone, CEREMONY_TIMEOUT).await
}

/// The first answer, from this device's browser or the phone's relay slot.
async fn answer(
    listener: Listener,
    relay: Relay,
    phone: &Phone,
    within: Duration,
) -> Result<Outcome, String> {
    let either = async {
        tokio::select! {
            outcome = listener.wait() => outcome,
            outcome = relay.wait(&phone.chosen) => outcome,
        }
    };
    tokio::time::timeout(within, either)
        .await
        .map_err(|_| "Nothing came back from the passkey. Try again.".to_string())?
}

/// The phone path of one flow's touches: each touch's QR URL goes out on
/// `shown`; the screen sets `chosen` once the person picks "Use a phone
/// instead". The flow holds the only sender, so the URLs end with it.
pub(crate) struct Phone {
    chosen: Arc<AtomicBool>,
    shown: futures::channel::mpsc::UnboundedSender<String>,
}

impl Phone {
    /// A phone path the screen picks through `chosen`, and the stream of QR
    /// URLs its touches show.
    pub(crate) fn new(
        chosen: Arc<AtomicBool>,
    ) -> (Phone, futures::channel::mpsc::UnboundedReceiver<String>) {
        let (shown, urls) = futures::channel::mpsc::unbounded();
        (Phone { chosen, shown }, urls)
    }

    fn chosen(&self) -> bool {
        self.chosen.load(Ordering::Relaxed)
    }

    fn show(&self, url: String) {
        let _ = self.shown.unbounded_send(url);
    }
}

// ---------- the relay ----------

/// One touch's slot on the auth host, `/r/<id>`: 32 random bytes the app
/// mints (an unguessable id is the slot's only lock), which the phone's
/// page POSTs to and the app polls. The body is public data every flow
/// verifies against the account's keys, so a forged post only fails there.
struct Relay {
    http: reqwest::Client,
    url: String,
    every: Duration,
}

impl Relay {
    /// A fresh slot on `page`'s origin — the page accepts only its own.
    fn at(page: &str) -> Result<Relay, String> {
        let mut url = reqwest::Url::parse(page)
            .map_err(|error| format!("The auth page address is unusable: {error}"))?;
        url.set_path(&format!("/r/{}", B64.encode(rand::random::<[u8; 32]>())));
        url.set_query(None);
        url.set_fragment(None);
        Ok(Relay {
            http: reqwest::Client::new(),
            url: url.into(),
            every: RELAY_POLL,
        })
    }

    /// Polls while `chosen` (the phone was picked) until the slot answers.
    /// An unreachable host is retried; the ceremony's timeout bounds it.
    async fn wait(self, chosen: &AtomicBool) -> Result<Outcome, String> {
        loop {
            if chosen.load(Ordering::Relaxed) {
                match self.http.get(&self.url).send().await {
                    Ok(reply) if reply.status() == reqwest::StatusCode::OK => {
                        let body = reply.text().await.map_err(|_| {
                            "Lost the phone's answer on the way. Try again.".to_string()
                        })?;
                        return parse_result(&body);
                    }
                    Ok(reply) if reply.status() == reqwest::StatusCode::NO_CONTENT => {}
                    Ok(reply) => {
                        return Err(format!(
                            "The auth host refused to relay the phone's answer ({}). Try again.",
                            reply.status()
                        ));
                    }
                    Err(error) => {
                        tracing::debug!(target: "ducktape::auth", event = "relay_unreachable", %error);
                    }
                }
            }
            tokio::time::sleep(self.every).await;
        }
    }
}

/// The system browser, or `DUCKTAPE_BROWSER <url>` when set (a test's
/// headless browser).
fn open_browser(url: &str) -> Result<(), String> {
    match std::env::var_os("DUCKTAPE_BROWSER") {
        Some(command) => std::process::Command::new(command)
            .arg(url)
            .spawn()
            .map(drop)
            .map_err(|error| format!("Couldn't open the browser: {error}")),
        None => match OPEN_URL.get() {
            Some(open) => {
                open(url.to_owned());
                Ok(())
            }
            None => {
                tracing::error!(target: "ducktape::auth", reason = "no_url_opener", "the browser page could not be opened");
                Ok(())
            }
        },
    }
}

/// What opens a page in the system browser: the shell's, handed over at
/// startup ([`on_open_url`]) so this layer never calls up into it.
static OPEN_URL: std::sync::OnceLock<fn(String)> = std::sync::OnceLock::new();

/// The shell says how a page reaches the system browser.
pub(crate) fn on_open_url(open: fn(String)) {
    let _ = OPEN_URL.set(open);
}

// ---------- the loopback listener ----------

/// One-shot, on an ephemeral 127.0.0.1 port. The port is no secret, so the
/// callback path carries 32 random bytes and only a POST to that path ends
/// the wait.
struct Listener {
    listener: TcpListener,
    path: String,
}

impl Listener {
    async fn bind() -> std::io::Result<Listener> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let path = format!("/cb/{}", B64.encode(rand::random::<[u8; 32]>()));
        Ok(Listener { listener, path })
    }

    fn callback_url(&self) -> String {
        let port = self
            .listener
            .local_addr()
            .map(|addr| addr.port())
            .unwrap_or_default();
        format!("http://127.0.0.1:{port}{}", self.path)
    }

    async fn wait(self) -> Result<Outcome, String> {
        loop {
            let (mut stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|error| format!("Lost the browser's connection: {error}"))?;
            let Ok((method, path, body)) = read_request(&mut stream).await else {
                continue;
            };
            let result = (path == self.path && method == "POST")
                .then(|| form_field(&body, "result"))
                .flatten();
            let Some(result) = result else {
                respond(&mut stream, "404 Not Found", "Not found.").await;
                continue;
            };
            let outcome = parse_result(&result);
            let said = match outcome {
                Ok(_) => "Done. You can return to Ducktape.",
                Err(_) => "That didn't finish. Ducktape has the details.",
            };
            respond(&mut stream, "200 OK", said).await;
            return outcome;
        }
    }
}

pub(super) async fn read_request(
    stream: &mut TcpStream,
) -> std::io::Result<(String, String, Vec<u8>)> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut length = 0usize;
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    // the page's answer is a few KiB; refuse to buffer anything absurd
    let mut body = vec![0; length.min(64 * 1024)];
    reader.read_exact(&mut body).await?;
    Ok((method, path, body))
}

async fn respond(stream: &mut TcpStream, status: &str, said: &str) {
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>Ducktape</title>\
         <body style=\"font:16px system-ui;display:grid;place-items:center;min-height:90vh\">\
         <p>{said}</p>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
}

/// One `application/x-www-form-urlencoded` field, decoded.
pub(super) fn form_field(body: &[u8], name: &str) -> Option<String> {
    body.split(|byte| *byte == b'&').find_map(|pair| {
        let at = pair.iter().position(|byte| *byte == b'=')?;
        (form_decode(&pair[..at]) == name.as_bytes())
            .then(|| String::from_utf8(form_decode(&pair[at + 1..])).ok())
            .flatten()
    })
}

fn form_decode(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        let escaped = (value[i] == b'%')
            .then(|| value.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        match (value[i], escaped) {
            (_, Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (b'+', None) => {
                out.push(b' ');
                i += 1;
            }
            (byte, None) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyscheme::testkit;

    const RP: &str = "auth.ducktape.industries";

    /// The page's `get` answer, as JSON, for an assertion made the way an
    /// authenticator makes it.
    fn page_answer(
        sk: &p256::ecdsa::SigningKey,
        ns: &[u8],
        preimage: &[u8],
        handle: &[u8],
    ) -> Assertion {
        let (authenticator_data, client_data_json, signature) =
            testkit::passkey_assertion_parts(sk, RP, ns, preimage);
        let json = serde_json::json!({
            "op": "get",
            "credentialId": B64.encode([9; 16]),
            "authenticatorData": B64.encode(authenticator_data),
            "clientDataJSON": B64.encode(client_data_json),
            "signature": B64.encode(signature),
            "userHandle": B64.encode(handle),
        });
        match parse_result(&json.to_string()).unwrap() {
            Outcome::Asserted(assertion) => assertion,
            Outcome::Created(_) => unreachable!(),
        }
    }

    #[test]
    fn a_passkey_signed_frame_verifies_as_the_node_verifies_it() {
        let sk = testkit::passkey(3);
        let pubkey = testkit::passkey_pubkey(&sk);
        let body = passkey_body(&pubkey, "testkit", 0, vec![1, 2, 3]);
        let assertion = page_answer(&sk, FRAME_NAMESPACE, &body.preimage(), &[]);
        let frame = passkey_frame(body, &assertion).expect("its own signer");
        // the node decodes the bytes and runs `scheme.verify(signer, NS, preimage, proof)`
        let decoded: Frame = abi::decode(&frame.encode()).unwrap();
        assert_eq!(decoded.body.scheme, KeyScheme::Secp256r1);
        assert!(decoded.body.scheme.verify(
            &decoded.body.signer,
            FRAME_NAMESPACE,
            &decoded.body.preimage(),
            &decoded.proof
        ));

        // another passkey answering is caught before the node sees it
        let other = page_answer(
            &testkit::passkey(4),
            FRAME_NAMESPACE,
            &decoded.body.preimage(),
            &[],
        );
        assert!(passkey_frame(decoded.body, &other).is_none());
    }

    #[test]
    fn a_passkey_consent_verifies_and_names_the_key_that_gave_it() {
        let sk = testkit::passkey(5);
        let admission = Admission {
            network: b"testkit".to_vec(),
            scheme: abi::Scheme::Ed25519,
            key: vec![7; 32],
            generation: 0,
            account: 12,
            expires_at: 99,
        };
        let preimage = admission.preimage();
        let assertion = page_answer(
            &sk,
            CONSENT_NAMESPACE,
            &preimage,
            &user_handle("testkit", 12),
        );
        assert_eq!(
            account_of_handle("testkit", assertion.user_handle.as_deref()),
            Ok(12)
        );
        let key = |sk| identity::Key {
            scheme: abi::Scheme::Secp256r1,
            key: testkit::passkey_pubkey(sk),
            label: None,
            added_at: 0,
        };
        let account = identity::Account {
            number: 12,
            card: identity::Card {
                name: "ada".into(),
                avatar: None,
                bio: None,
                updated_at: 0,
            },
            control: Control::Person {
                keys: vec![key(&testkit::passkey(6)), key(&sk)],
            },
        };
        let proof = assertion.proof();
        let signer = consenting_key(&account, &preimage, &proof).unwrap();
        assert_eq!(signer, testkit::passkey_pubkey(&sk));
        assert!(KeyScheme::Secp256r1.verify(&signer, CONSENT_NAMESPACE, &preimage, &proof));
        // bound to its admission: another account's preimage does not verify
        let elsewhere = Admission {
            account: 13,
            ..admission
        }
        .preimage();
        assert!(consenting_key(&account, &elsewhere, &proof).is_none());
    }

    #[test]
    fn a_device_consent_admits_a_passkey() {
        use commonware_cryptography::Signer as _;
        let device = commonware_cryptography::ed25519::PrivateKey::from_seed(1);
        let preimage = b"admission".to_vec();
        let proof = device.sign(CONSENT_NAMESPACE, &preimage);
        assert!(KeyScheme::Ed25519.verify(
            device.public_key().as_ref(),
            CONSENT_NAMESPACE,
            &preimage,
            proof.as_ref()
        ));
    }

    #[test]
    fn a_user_handle_names_its_network_and_account() {
        let handle = user_handle("testkit", 258);
        assert_eq!(account_of_handle("testkit", Some(&handle)), Ok(258));
        assert!(account_of_handle("other", Some(&handle)).is_err());
        assert!(account_of_handle("testkit", None).is_err());
        assert!(account_of_handle("testkit", Some(&[1; 39])).is_err());
    }

    #[test]
    fn the_request_rides_the_fragment() {
        let url = request_url(
            AUTH_PAGE,
            &Request::Get {
                challenge: [0xff; 32],
            },
            "http://127.0.0.1:1/cb/x",
        );
        assert_eq!(
            url,
            format!(
                "{AUTH_PAGE}#op=get&challenge={}&cb=http%3A%2F%2F127.0.0.1%3A1%2Fcb%2Fx",
                B64.encode([0xff; 32])
            )
        );
        let create = request_url(
            AUTH_PAGE,
            &Request::Create {
                user: [1; 40],
                name: "ada · testkit".into(),
            },
            "cb",
        );
        assert!(create.contains("&name=ada%20%C2%B7%20testkit&cb=cb"));
        assert!(create.contains(&format!("&user={}&", B64.encode([1; 40]))));
    }

    #[test]
    fn a_page_error_reads_as_a_sentence() {
        let error = parse_result(r#"{"op":"get","error":"NotAllowedError","message":"x"}"#);
        assert_eq!(error, Err(ceremony_error("NotAllowedError")));
        let short_key = format!(r#"{{"op":"create","publicKey":"{}"}}"#, B64.encode([2; 32]));
        assert!(parse_result(&short_key).is_err());
    }

    #[tokio::test]
    async fn the_listener_takes_one_post_on_its_path() {
        let listener = Listener::bind().await.unwrap();
        let callback = listener.callback_url();
        let waiting = tokio::spawn(listener.wait());
        let http = reqwest::Client::new();
        let wrong = callback.rsplit_once('/').unwrap().0.to_owned() + "/nope";
        assert_eq!(
            http.post(&wrong).body("").send().await.unwrap().status(),
            404
        );
        let key = testkit::passkey_pubkey(&testkit::passkey(2));
        let result = format!(r#"{{"op":"create","publicKey":"{}"}}"#, B64.encode(&key));
        let form = format!("result={}", url_encode(&result));
        let answer = http
            .post(&callback)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form)
            .send()
            .await
            .unwrap();
        assert_eq!(answer.status(), 200);
        assert_eq!(waiting.await.unwrap(), Ok(Outcome::Created(key)));
    }

    /// A fake auth host: each GET of `/r/<id>` takes the next of `answers`
    /// (then 204s); `polls` counts them. It answers one request at a time,
    /// in the order they connected, so [`barrier`] is served only after
    /// every poll already on the wire has been counted.
    async fn fake_relay(
        answers: Vec<(&'static str, String)>,
    ) -> (String, tokio::sync::watch::Receiver<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let page = format!("http://{}/.duck/auth", listener.local_addr().unwrap());
        let (count, polls) = tokio::sync::watch::channel(0);
        tokio::spawn(async move {
            let mut answers = answers.into_iter();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                // a poll cut off by a cancel sends nothing whole
                let Ok((method, path, _)) = read_request(&mut stream).await else {
                    continue;
                };
                assert_eq!(method, "GET");
                let (status, body) = match path.as_str() {
                    "/barrier" => ("204 No Content", String::new()),
                    _ => {
                        assert!(path.starts_with("/r/") && path.len() == 46, "{path}");
                        count.send_modify(|polls| *polls += 1);
                        answers.next().unwrap_or(("204 No Content", String::new()))
                    }
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        (page, polls)
    }

    /// Returns once the fake host has answered every request that reached
    /// it before this one.
    async fn barrier(page: &str) {
        let origin = page.trim_end_matches("/.duck/auth");
        reqwest::get(format!("{origin}/barrier")).await.unwrap();
    }

    /// A relay polling every 10 ms, and a phone already picked.
    fn phone_relay(page: &str) -> (Relay, Phone) {
        let relay = Relay {
            every: Duration::from_millis(10),
            ..Relay::at(page).unwrap()
        };
        (relay, Phone::new(Arc::new(AtomicBool::new(true))).0)
    }

    #[tokio::test]
    async fn a_relay_slot_is_minted_on_the_pages_origin() {
        let relay = Relay::at("https://auth.example/.duck/auth?x#y").unwrap();
        let id = relay.url.strip_prefix("https://auth.example/r/").unwrap();
        assert_eq!(B64.decode(id).unwrap().len(), 32);
        assert_ne!(
            Relay::at("https://a/").unwrap().url,
            Relay::at("https://a/").unwrap().url
        );
        assert!(Relay::at("not a url").is_err());
        // the QR carries the slot as the page's callback
        let url = request_url(
            "https://auth.example/",
            &Request::Get { challenge: [0; 32] },
            &relay.url,
        );
        assert!(url.ends_with(&format!("&cb=https%3A%2F%2Fauth.example%2Fr%2F{id}")));
    }

    #[tokio::test]
    async fn the_relay_polls_through_204s_to_the_phones_answer() {
        let key = testkit::passkey_pubkey(&testkit::passkey(2));
        let created = format!(r#"{{"op":"create","publicKey":"{}"}}"#, B64.encode(&key));
        let (page, polls) = fake_relay(vec![
            ("204 No Content", String::new()),
            ("204 No Content", String::new()),
            ("200 OK", created),
        ])
        .await;
        let (relay, phone) = phone_relay(&page);
        let listener = Listener::bind().await.unwrap();
        let outcome = answer(listener, relay, &phone, Duration::from_secs(10)).await;
        assert_eq!(outcome, Ok(Outcome::Created(key)));
        assert_eq!(*polls.borrow(), 3);
    }

    #[tokio::test]
    async fn the_relay_is_not_polled_until_the_phone_is_picked_and_then_times_out() {
        let (page, mut polls) = fake_relay(vec![]).await;
        let chosen = Arc::new(AtomicBool::new(false));
        let phone = Phone::new(chosen.clone()).0;
        // never picked: the ceremony times out, the slot never asked
        let (relay, _) = phone_relay(&page);
        let listener = Listener::bind().await.unwrap();
        let outcome = answer(listener, relay, &phone, Duration::from_millis(100)).await;
        assert_eq!(
            outcome,
            Err("Nothing came back from the passkey. Try again.".into())
        );
        barrier(&page).await;
        assert_eq!(*polls.borrow(), 0);
        // picked: the slot is asked
        chosen.store(true, Ordering::Relaxed);
        let (relay, _) = phone_relay(&page);
        let listener = Listener::bind().await.unwrap();
        tokio::select! {
            outcome = answer(listener, relay, &phone, Duration::from_secs(60)) => {
                panic!("answered with nothing to answer: {outcome:?}")
            }
            polled = polls.wait_for(|polls| *polls > 0) => {
                polled.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn cancelling_stops_the_polling() {
        let (page, mut polls) = fake_relay(vec![]).await;
        let (relay, phone) = phone_relay(&page);
        let listener = Listener::bind().await.unwrap();
        let waiting =
            tokio::spawn(
                async move { answer(listener, relay, &phone, Duration::from_secs(60)).await },
            );
        polls.wait_for(|polls| *polls > 0).await.unwrap();
        waiting.abort();
        assert!(waiting.await.unwrap_err().is_cancelled());
        barrier(&page).await;
        let stopped = *polls.borrow();
        // ten of the relay's periods: a poll here would be one after the
        // cancel (a pause can only hide one, never invent one)
        tokio::time::sleep(Duration::from_millis(100)).await;
        barrier(&page).await;
        assert_eq!(*polls.borrow(), stopped);
    }

    #[tokio::test]
    async fn a_relayed_answer_that_fails_verification_is_refused() {
        // a key no P-256 verifier accepts
        let bad_key = format!(r#"{{"op":"create","publicKey":"{}"}}"#, B64.encode([2; 32]));
        // an assertion by another passkey than the frame's signer
        let body = passkey_body(
            &testkit::passkey_pubkey(&testkit::passkey(3)),
            "testkit",
            0,
            vec![1],
        );
        let (authenticator_data, client_data_json, signature) = testkit::passkey_assertion_parts(
            &testkit::passkey(4),
            RP,
            FRAME_NAMESPACE,
            &body.preimage(),
        );
        let stranger = serde_json::json!({
            "op": "get",
            "authenticatorData": B64.encode(authenticator_data),
            "clientDataJSON": B64.encode(client_data_json),
            "signature": B64.encode(signature),
        })
        .to_string();
        let (page, _) = fake_relay(vec![("200 OK", bad_key), ("200 OK", stranger)]).await;

        let (relay, phone) = phone_relay(&page);
        let outcome = answer(
            Listener::bind().await.unwrap(),
            relay,
            &phone,
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(
            outcome,
            Err("The browser returned a key Ducktape can't use.".into())
        );

        let (relay, phone) = phone_relay(&page);
        let Ok(Outcome::Asserted(assertion)) = answer(
            Listener::bind().await.unwrap(),
            relay,
            &phone,
            Duration::from_secs(10),
        )
        .await
        else {
            panic!("an assertion");
        };
        assert!(passkey_frame(body, &assertion).is_none());
    }

    #[test]
    fn an_identity_refusal_reads_by_its_token() {
        let taken = abi::Refusal::new(abi::reason::ALREADY_EXISTS, "reworded upstream");
        assert!(rejected(taken).starts_with("This device's key already belongs"));
        let expired = abi::Refusal::new("unauthorized", "the consent has expired");
        assert!(rejected(expired).contains("consent expired"));
    }

    #[tokio::test]
    async fn a_relay_refusal_reads_as_a_sentence() {
        let (page, _) = fake_relay(vec![("404 Not Found", String::new())]).await;
        let (relay, phone) = phone_relay(&page);
        let outcome = answer(
            Listener::bind().await.unwrap(),
            relay,
            &phone,
            Duration::from_secs(10),
        )
        .await;
        assert!(outcome.unwrap_err().starts_with("The auth host refused"));
    }
}
