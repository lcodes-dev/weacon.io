//! The core's end of a sign-in terminal's seal: Noise NK as the responder, whose static key is
//! the machine key. ring does AES-GCM, SHA-256 and randomness; x25519-dalek does X25519.

use snow::params::{CipherChoice, DHChoice, HashChoice, NoiseParams};
use snow::resolvers::{CryptoResolver, RingResolver};
use snow::types::{Cipher, Dh, Hash, Random};
use snow::{Builder, TransportState};
use weacon_observability::Fault;
use weacon_protocol::{NOISE_MAX, SEALED_MAX, TERMINAL_NOISE, code};
use x25519_dalek::{PublicKey, StaticSecret};

/// A handshake the core accepted: its channel, the first message's payload, and the reply to
/// send back.
pub(crate) struct Accepted {
    pub(crate) seal: Seal,
    pub(crate) first: Vec<u8>,
    pub(crate) reply: Vec<u8>,
}

/// One terminal's channel once the handshake is done. After any failure it refuses everything,
/// so the terminal closes.
pub(crate) struct Seal {
    transport: TransportState,
    broken: bool,
}

/// Answers the browser's first handshake message with the machine's private `key`, bound to
/// `prologue`.
///
/// # Errors
/// When the message wasn't sealed to this key, under this prologue, or isn't one at all.
pub(crate) fn accept(key: &[u8; 32], prologue: &[u8], handshake: &[u8]) -> Result<Accepted, Fault> {
    let responder = builder(prologue)
        .and_then(|builder| builder.local_private_key(key))
        .map_err(|error| handshake_failed().because(error))?;
    accept_with(responder, handshake, &[])
}

// The published vectors put a payload in the reply too.
fn accept_with(
    responder: Builder<'_>,
    handshake: &[u8],
    payload: &[u8],
) -> Result<Accepted, Fault> {
    let run = || {
        let mut state = responder.build_responder()?;
        let first = buffered(|buffer| state.read_message(handshake, buffer))?;
        let reply = buffered(|buffer| state.write_message(payload, buffer))?;
        Ok(Accepted {
            seal: Seal {
                transport: state.into_transport_mode()?,
                broken: false,
            },
            first,
            reply,
        })
    };
    run().map_err(|error: snow::Error| handshake_failed().because(error))
}

impl Seal {
    /// `plaintext` as the next message to send.
    ///
    /// # Errors
    /// When it is longer than [`SEALED_MAX`], or the channel failed before.
    pub(crate) fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, Fault> {
        if self.broken || plaintext.len() > SEALED_MAX {
            self.broken = true;
            return Err(seal_failed());
        }
        let sealed = buffered(|buffer| self.transport.write_message(plaintext, buffer));
        self.settle(sealed)
    }

    /// The plaintext of the next message received.
    ///
    /// # Errors
    /// When `message` isn't the next one sealed by the other end, or the channel failed before.
    pub(crate) fn open(&mut self, message: &[u8]) -> Result<Vec<u8>, Fault> {
        if self.broken {
            return Err(seal_failed());
        }
        let opened = buffered(|buffer| self.transport.read_message(message, buffer));
        self.settle(opened)
    }

    fn settle(&mut self, result: Result<Vec<u8>, snow::Error>) -> Result<Vec<u8>, Fault> {
        result.map_err(|error| {
            self.broken = true;
            seal_failed().because(error)
        })
    }
}

fn builder(prologue: &[u8]) -> Result<Builder<'_>, snow::Error> {
    let params: NoiseParams = TERMINAL_NOISE.parse()?;
    Builder::with_resolver(params, Box::new(Resolver)).prologue(prologue)
}

fn buffered(
    step: impl FnOnce(&mut [u8]) -> Result<usize, snow::Error>,
) -> Result<Vec<u8>, snow::Error> {
    let mut buffer = vec![0; NOISE_MAX];
    let len = step(&mut buffer)?;
    buffer.truncate(len);
    Ok(buffer)
}

fn handshake_failed() -> Fault {
    Fault::new(
        code!("terminal.handshake_failed"),
        "The terminal's handshake failed",
    )
}

pub(super) fn seal_failed() -> Fault {
    Fault::new(code!("terminal.seal_failed"), "The terminal's seal failed")
}

struct Resolver;

impl CryptoResolver for Resolver {
    fn resolve_rng(&self) -> Option<Box<dyn Random>> {
        RingResolver.resolve_rng()
    }

    fn resolve_dh(&self, choice: &DHChoice) -> Option<Box<dyn Dh>> {
        if !matches!(choice, DHChoice::Curve25519) {
            return None;
        }
        Some(Box::new(X25519::default()))
    }

    fn resolve_hash(&self, choice: &HashChoice) -> Option<Box<dyn Hash>> {
        RingResolver.resolve_hash(choice)
    }

    fn resolve_cipher(&self, choice: &CipherChoice) -> Option<Box<dyn Cipher>> {
        RingResolver.resolve_cipher(choice)
    }
}

struct X25519 {
    secret: StaticSecret,
    public: PublicKey,
}

impl Default for X25519 {
    fn default() -> Self {
        let secret = StaticSecret::from([0; 32]);
        let public = PublicKey::from(&secret);
        Self { secret, public }
    }
}

impl Dh for X25519 {
    fn name(&self) -> &'static str {
        "25519"
    }

    fn pub_len(&self) -> usize {
        32
    }

    fn priv_len(&self) -> usize {
        32
    }

    // snow always passes `priv_len` bytes.
    fn set(&mut self, privkey: &[u8]) {
        if let Some(Ok(bytes)) = privkey.get(..32).map(<[u8; 32]>::try_from) {
            self.secret = StaticSecret::from(bytes);
            self.public = PublicKey::from(&self.secret);
        }
    }

    fn generate(&mut self, rng: &mut dyn Random) -> Result<(), snow::Error> {
        let mut bytes = [0; 32];
        rng.try_fill_bytes(&mut bytes)?;
        self.set(&bytes);
        bytes.fill(0);
        Ok(())
    }

    fn pubkey(&self) -> &[u8] {
        self.public.as_bytes()
    }

    fn privkey(&self) -> &[u8] {
        self.secret.as_bytes()
    }

    // snow hands over buffers that may be longer than the key. A low-order point, which would
    // give everyone the same secret, is refused, as Web Crypto refuses it in the browser.
    fn dh(&self, pubkey: &[u8], out: &mut [u8]) -> Result<(), snow::Error> {
        let Some(Ok(public)) = pubkey.get(..32).map(<[u8; 32]>::try_from) else {
            return Err(snow::Error::Dh);
        };
        let shared = self.secret.diffie_hellman(&PublicKey::from(public));
        let out = out.get_mut(..32).ok_or(snow::Error::Dh)?;
        if !shared.was_contributory() {
            return Err(snow::Error::Dh);
        }
        out.copy_from_slice(shared.as_bytes());
        Ok(())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::fmt::Write;
    use std::fs;
    use std::path::Path;

    use serde::{Deserialize, Serialize};
    use snow::HandshakeState;
    use weacon_protocol::{
        AgentKind, Bytes, MachineId, SignInState, TerminalAction, TerminalCommand, TerminalId,
        TerminalInput, TerminalOutput, WorkspaceId,
    };

    use super::*;

    /// A test vector in the published Noise format: the messages alternate, initiator first.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Vector {
        protocol_name: String,
        init_prologue: String,
        init_ephemeral: String,
        init_remote_static: String,
        resp_prologue: String,
        resp_static: String,
        resp_ephemeral: String,
        handshake_hash: String,
        messages: Vec<Message>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Message {
        payload: String,
        ciphertext: String,
    }

    /// The one published vector for this protocol, and one of Weacon's own messages made here.
    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Vectors {
        published: Vector,
        weacon: Vector,
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut hex, byte| {
            write!(hex, "{byte:02x}").unwrap();
            hex
        })
    }

    fn unhex(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(hex.get(at..at + 2).unwrap(), 16).unwrap())
            .collect()
    }

    fn machine_key() -> [u8; 32] {
        unhex(MACHINE).try_into().unwrap()
    }

    fn public(private: &[u8; 32]) -> Vec<u8> {
        PublicKey::from(&StaticSecret::from(*private))
            .as_bytes()
            .to_vec()
    }

    /// The browser's end of a handshake with the machine whose key is `remote`.
    pub(in crate::terminal) fn initiator(
        prologue: &[u8],
        remote: &[u8],
        ephemeral: Option<&[u8]>,
    ) -> HandshakeState {
        let builder = builder(prologue)
            .unwrap()
            .remote_public_key(remote)
            .unwrap();
        let builder = match ephemeral {
            Some(key) => builder.fixed_ephemeral_key_for_testing_only(key),
            None => builder,
        };
        builder.build_initiator().unwrap()
    }

    pub(in crate::terminal) fn write(state: &mut HandshakeState, payload: &[u8]) -> Vec<u8> {
        buffered(|buffer| state.write_message(payload, buffer)).unwrap()
    }

    /// The browser's end after the handshake, and the core's.
    /// The browser's end once the core's `reply` is in.
    pub(in crate::terminal) fn finish(mut state: HandshakeState, reply: &[u8]) -> TransportState {
        state.read_message(reply, &mut [0; 64]).unwrap();
        state.into_transport_mode().unwrap()
    }

    pub(in crate::terminal) fn sealed(browser: &mut TransportState, plaintext: &[u8]) -> Vec<u8> {
        buffered(|buffer| browser.write_message(plaintext, buffer)).unwrap()
    }

    pub(in crate::terminal) fn opened(browser: &mut TransportState, message: &[u8]) -> Vec<u8> {
        buffered(|buffer| browser.read_message(message, buffer)).unwrap()
    }

    fn connected(prologue: &[u8], remote: &[u8], first: &[u8]) -> (TransportState, Seal) {
        let mut browser = initiator(prologue, remote, None);
        let handshake = write(&mut browser, first);
        let accepted = accept(&machine_key(), prologue, &handshake).unwrap();
        (finish(browser, &accepted.reply), accepted.seal)
    }

    // Plays `vector`'s payloads through both ends with its keys and checks each arrives; the
    // vector as these ends made it.
    fn replay(vector: &Vector) -> Vector {
        let init_ephemeral = unhex(&vector.init_ephemeral);
        let resp_static = unhex(&vector.resp_static);
        let resp_ephemeral = unhex(&vector.resp_ephemeral);
        let resp_prologue = unhex(&vector.resp_prologue);
        let payloads: Vec<Vec<u8>> = vector
            .messages
            .iter()
            .map(|message| unhex(&message.payload))
            .collect();
        let mut initiator = initiator(
            &unhex(&vector.init_prologue),
            &unhex(&vector.init_remote_static),
            Some(&init_ephemeral),
        );
        let first = write(&mut initiator, &payloads[0]);
        let responder = builder(&resp_prologue)
            .unwrap()
            .local_private_key(&resp_static)
            .unwrap()
            .fixed_ephemeral_key_for_testing_only(&resp_ephemeral);
        let mut accepted = accept_with(responder, &first, &payloads[1]).unwrap();
        assert_eq!(accepted.first, payloads[0]);
        let mut payload = vec![0; NOISE_MAX];
        let len = initiator
            .read_message(&accepted.reply, &mut payload)
            .unwrap();
        assert_eq!(payload[..len], payloads[1]);
        let handshake_hash = hex(initiator.get_handshake_hash());
        let mut browser = initiator.into_transport_mode().unwrap();
        let mut made = vec![first, accepted.reply];
        made.extend(exchange(&mut browser, &mut accepted.seal, &payloads));
        let messages = made.iter().zip(&vector.messages);
        Vector {
            handshake_hash,
            messages: messages
                .map(|(ciphertext, message)| Message {
                    payload: message.payload.clone(),
                    ciphertext: hex(ciphertext),
                })
                .collect(),
            ..vector.clone()
        }
    }

    // Sends the payloads after the handshake's two, alternating from the browser; what was sent.
    fn exchange(
        browser: &mut TransportState,
        seal: &mut Seal,
        payloads: &[Vec<u8>],
    ) -> Vec<Vec<u8>> {
        let sent = payloads.iter().enumerate().skip(2).map(|(index, payload)| {
            if index % 2 == 0 {
                let sealed = buffered(|buffer| browser.write_message(payload, buffer)).unwrap();
                assert_eq!(seal.open(&sealed).unwrap(), *payload);
                sealed
            } else {
                let sealed = seal.seal(payload).unwrap();
                let opened = buffered(|buffer| browser.read_message(&sealed, buffer)).unwrap();
                assert_eq!(opened, *payload);
                sealed
            }
        });
        sent.collect()
    }

    fn open_command(agent: AgentKind) -> TerminalCommand {
        TerminalCommand {
            terminal: TerminalId::try_from("t-1".to_owned()).unwrap(),
            machine: MachineId::try_from("m-1".to_owned()).unwrap(),
            workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
            action: TerminalAction::Open {
                agent,
                handshake: Bytes::new(Vec::new()).unwrap(),
            },
            by: None,
        }
    }

    fn prologue() -> Vec<u8> {
        open_command(AgentKind::Claude).prologue().unwrap()
    }

    // The private half of the key whose fingerprint the pairing tests check.
    const MACHINE: &str = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";

    // Weacon's messages, before they are sealed.
    fn weacon_draft() -> Vector {
        let prologue = hex(&prologue());
        let data = |text: &[u8]| Bytes::new(text.to_vec()).unwrap();
        // As a terminal runs: the size in the handshake and again in the browser's first frame,
        // what the login prints, what the person types, and the end.
        let payloads = [
            TerminalInput::Resize { cols: 80, rows: 24 }.to_json(),
            Ok(Vec::new()),
            TerminalInput::Resize { cols: 80, rows: 24 }.to_json(),
            TerminalOutput::Output {
                data: data(b"Paste the code: "),
            }
            .to_json(),
            TerminalInput::Input { data: data(b"y\r") }.to_json(),
            TerminalOutput::Ended {
                sign_in: SignInState::SignedIn,
            }
            .to_json(),
        ];
        Vector {
            protocol_name: TERMINAL_NOISE.to_owned(),
            init_prologue: prologue.clone(),
            init_ephemeral: "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb"
                .to_owned(),
            init_remote_static: hex(&public(&machine_key())),
            resp_prologue: prologue,
            resp_static: MACHINE.to_owned(),
            resp_ephemeral: "c8f2a1e04b3d5967788f0e1d2c3b4a59687766554433221100ffeeddccbbaa99"
                .to_owned(),
            handshake_hash: String::new(),
            messages: payloads
                .into_iter()
                .map(|payload| Message {
                    payload: hex(&payload.unwrap()),
                    ciphertext: String::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn both_ends_match_the_published_vector_and_the_committed_one_of_weacons_messages() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/protocol/noise-vectors.json");
        let committed = fs::read(&path).unwrap();
        let committed: Vectors = serde_json::from_slice(&committed).unwrap();
        assert_eq!(committed.published.protocol_name, TERMINAL_NOISE);
        assert_eq!(replay(&committed.published), committed.published);

        let current = Vectors {
            published: committed.published.clone(),
            weacon: replay(&weacon_draft()),
        };
        if std::env::var_os("UPDATE_VECTORS").is_some() {
            let json = serde_json::to_string_pretty(&current).unwrap() + "\n";
            fs::write(&path, json).unwrap();
            return;
        }
        assert_eq!(
            current, committed,
            "noise-vectors.json is out of date: run UPDATE_VECTORS=1 cargo test --locked -p weacon-core seal"
        );
    }

    #[test]
    fn a_handshake_for_another_key_or_prologue_or_with_a_low_order_point_is_refused() {
        let key = machine_key();
        let resize = br#"{"kind":"resize","cols":80,"rows":24}"#;
        let codex = open_command(AgentKind::Codex).prologue().unwrap();
        let other_key = public(&[9; 32]);
        for (bound, remote) in [(prologue(), other_key), (codex, public(&key))] {
            let mut browser = initiator(&prologue(), &remote, None);
            let handshake = write(&mut browser, resize);
            let refused = accept(&key, &bound, &handshake).err().unwrap();
            assert_eq!(refused.code().as_str(), "terminal.handshake_failed");
        }
        let mut low_order = vec![0; 32];
        low_order.extend_from_slice(&[1; 16]);
        accept(&key, &prologue(), &low_order).err().unwrap();
        accept(&key, &prologue(), b"").err().unwrap();
        assert!(X25519::default().dh(&[0; 32], &mut [0; 32]).is_err());
    }

    #[test]
    fn a_tampered_replayed_or_reordered_frame_breaks_the_channel_for_good() {
        let (mut browser, mut seal) = connected(&prologue(), &public(&machine_key()), b"{}");
        let mut frame =
            |text: &[u8]| buffered(|buffer| browser.write_message(text, buffer)).unwrap();
        let (one, two) = (frame(b"one"), frame(b"two"));
        assert_eq!(seal.open(&one).unwrap(), b"one");
        seal.open(&one).err().unwrap();
        seal.open(&two).err().unwrap();
        seal.seal(b"after").unwrap_err();

        let (mut browser, mut seal) = connected(&prologue(), &public(&machine_key()), b"{}");
        let mut tampered = buffered(|buffer| browser.write_message(b"one", buffer)).unwrap();
        tampered[0] ^= 1;
        seal.open(&tampered).err().unwrap();

        let (mut browser, mut seal) = connected(&prologue(), &public(&machine_key()), b"{}");
        buffered(|buffer| browser.write_message(b"skipped", buffer)).unwrap();
        let second = buffered(|buffer| browser.write_message(b"two", buffer)).unwrap();
        seal.open(&second).err().unwrap();
    }

    #[test]
    fn a_plaintext_too_long_for_one_frame_is_refused_and_breaks_the_channel() {
        let (mut browser, mut seal) = connected(&prologue(), &public(&machine_key()), b"{}");
        let full = seal.seal(&vec![7; SEALED_MAX]).unwrap();
        assert_eq!(full.len(), NOISE_MAX);
        let opened = buffered(|buffer| browser.read_message(&full, buffer)).unwrap();
        assert_eq!(opened.len(), SEALED_MAX);
        seal.seal(&vec![7; SEALED_MAX + 1]).err().unwrap();
        seal.seal(b"short").err().unwrap();
    }

    #[test]
    fn keys_snow_makes_are_x25519_pairs() {
        let pair = builder(&[]).unwrap().generate_keypair().unwrap();
        let private: [u8; 32] = pair.private.try_into().unwrap();
        assert_eq!(pair.public, public(&private));
        assert_ne!(private, [0; 32]);
        assert_eq!(X25519::default().name(), "25519");
    }

    #[test]
    fn each_handshake_makes_fresh_keys() {
        let remote = public(&machine_key());
        let (mut one, _) = connected(&prologue(), &remote, b"{}");
        let (mut two, _) = connected(&prologue(), &remote, b"{}");
        let first = buffered(|buffer| one.write_message(b"same", buffer)).unwrap();
        let second = buffered(|buffer| two.write_message(b"same", buffer)).unwrap();
        assert_ne!(first, second);
    }
}
