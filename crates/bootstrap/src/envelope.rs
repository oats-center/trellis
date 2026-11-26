//! The passphrase envelope of the files that move Trellis configuration
//! between installations: a service bundle (`trellis svc <id> export`) and an
//! installation export (`trellis-server export`).
//!
//! The file is a short text header followed by the content in chunks:
//!
//! ```text
//! trellis-service-bundle v1
//! argon2id m=65536 t=3 p=1 salt=<16 bytes, base64url>
//! nonce=<16 bytes, base64url>
//! mac=<HMAC-SHA256 of the lines above, base64url>
//! <ChaCha20-Poly1305 chunks of 64 KiB of content each>
//! ```
//!
//! This is age's passphrase construction, with Argon2id for scrypt: the
//! passphrase and salt give a key through Argon2id, the header is
//! authenticated with an HMAC key derived from it, and the content is
//! encrypted in chunks, each nonce a counter with a final-chunk flag, under a
//! key derived from it and the nonce. A wrong passphrase fails the header's
//! MAC; any change to the content, or cutting it short, fails a chunk's tag.
//! The first line names the kind of file, and the key derivation's labels
//! use the same name, so one kind never opens as another.

use std::io::{BufRead, Read, Write};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use miette::IntoDiagnostic;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use ring::hmac;
use ring::rand::{SecureRandom as _, SystemRandom};

/// A kind of file: the name its header starts with, and what to call it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Envelope(&'static str, &'static str);

/// A service bundle, as `trellis svc <id> export` writes it.
pub const SERVICE_BUNDLE: Envelope = Envelope("trellis-service-bundle", "Trellis service bundle");
/// An installation export, as `trellis-server export` writes it.
pub const INSTALLATION: Envelope = Envelope("trellis-installation", "Trellis installation export");

/// Plaintext bytes per chunk.
pub const CHUNK: usize = 64 * 1024;
/// Bytes a chunk's tag adds.
pub const TAG: usize = 16;

/// Argon2id cost this build writes: 64 MiB, three passes, one lane.
const MEMORY_KIB: u32 = 64 * 1024;
const PASSES: u32 = 3;
const LANES: u32 = 1;
/// Costs above these are refused, so a forged header cannot exhaust memory.
const MAX_MEMORY_KIB: u32 = 2 * 1024 * 1024;
const MAX_PASSES: u32 = 16;
const MAX_LANES: u32 = 16;
/// The shortest passphrase accepted for a new file.
pub const MIN_PASSPHRASE: usize = 12;

/// The keys one passphrase and header give.
struct Keys {
    header: hmac::Key,
    payload: LessSafeKey,
}

fn derive(
    envelope: Envelope,
    passphrase: &str,
    salt: &[u8],
    (memory, passes, lanes): (u32, u32, u32),
    nonce: &[u8],
) -> miette::Result<Keys> {
    let params = argon2::Params::new(memory, passes, lanes, Some(32))
        .map_err(|error| miette::miette!("the bundle's key derivation: {error}"))?;
    let mut key = [0; 32];
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|error| miette::miette!("the bundle's key derivation: {error}"))?;
    let root = hmac::Key::new(hmac::HMAC_SHA256, &key);
    let header = hmac::sign(&root, format!("{} header", envelope.0).as_bytes());
    let mut payload_label = format!("{} payload", envelope.0).into_bytes();
    payload_label.extend_from_slice(nonce);
    let payload = hmac::sign(&root, &payload_label);
    Ok(Keys {
        header: hmac::Key::new(hmac::HMAC_SHA256, header.as_ref()),
        payload: LessSafeKey::new(
            UnboundKey::new(&CHACHA20_POLY1305, payload.as_ref())
                .map_err(|_| miette::miette!("the bundle's content key"))?,
        ),
    })
}

fn chunk_nonce(counter: u64, last: bool) -> Nonce {
    let mut nonce = [0; 12];
    nonce[3..11].copy_from_slice(&counter.to_be_bytes());
    nonce[11] = u8::from(last);
    Nonce::assume_unique_for_key(nonce)
}

/// Encrypts content into a file as it is written.
pub struct Encryptor<W: Write> {
    out: W,
    key: LessSafeKey,
    counter: u64,
    pending: Vec<u8>,
}

impl<W: Write> Encryptor<W> {
    /// Write `envelope`'s header for `passphrase` to `out`.
    ///
    /// # Errors
    /// Fails when the system has no random bytes or `out` refuses the header.
    pub fn new(mut out: W, envelope: Envelope, passphrase: &str) -> miette::Result<Self> {
        let random = SystemRandom::new();
        let (mut salt, mut nonce) = ([0; 16], [0; 16]);
        random
            .fill(&mut salt)
            .and_then(|()| random.fill(&mut nonce))
            .map_err(|_| miette::miette!("no random bytes for the bundle"))?;
        let keys = derive(
            envelope,
            passphrase,
            &salt,
            (MEMORY_KIB, PASSES, LANES),
            &nonce,
        )?;
        let header = format!(
            "{} v1\nargon2id m={MEMORY_KIB} t={PASSES} p={LANES} salt={}\nnonce={}\n",
            envelope.0,
            URL_SAFE_NO_PAD.encode(salt),
            URL_SAFE_NO_PAD.encode(nonce)
        );
        let mac = hmac::sign(&keys.header, header.as_bytes());
        writeln!(out, "{header}mac={}", URL_SAFE_NO_PAD.encode(mac.as_ref())).into_diagnostic()?;
        Ok(Self {
            out,
            key: keys.payload,
            counter: 0,
            pending: Vec::with_capacity(2 * CHUNK),
        })
    }

    fn seal(&mut self, length: usize, last: bool) -> miette::Result<()> {
        let mut chunk: Vec<u8> = self.pending.drain(..length).collect();
        self.key
            .seal_in_place_append_tag(chunk_nonce(self.counter, last), Aad::empty(), &mut chunk)
            .map_err(|_| miette::miette!("cannot encrypt the bundle"))?;
        self.counter += 1;
        self.out.write_all(&chunk).into_diagnostic()
    }

    /// Encrypt `data` after what was written before.
    ///
    /// # Errors
    /// Fails when the output refuses a chunk.
    pub fn write(&mut self, data: &[u8]) -> miette::Result<()> {
        self.pending.extend_from_slice(data);
        // The final chunk is sealed by `finish`, so a full one waits until
        // more follows it.
        while self.pending.len() > CHUNK {
            self.seal(CHUNK, false)?;
        }
        Ok(())
    }

    /// Seal the last chunk and return the output.
    ///
    /// # Errors
    /// Fails when the output refuses the chunk or a flush.
    pub fn finish(mut self) -> miette::Result<W> {
        let length = self.pending.len();
        self.seal(length, true)?;
        self.out.flush().into_diagnostic()?;
        Ok(self.out)
    }
}

/// Decrypts a file's content as it is read.
pub struct Decryptor<R: BufRead> {
    input: R,
    key: LessSafeKey,
    counter: u64,
    buffer: Vec<u8>,
    position: usize,
    finished: bool,
}

fn header_line(
    input: &mut impl BufRead,
    envelope: Envelope,
    prefix: &str,
) -> miette::Result<String> {
    let mut line = String::new();
    input
        .by_ref()
        .take(512)
        .read_line(&mut line)
        .into_diagnostic()?;
    line.strip_suffix('\n')
        .and_then(|line| line.strip_prefix(prefix))
        .map(str::to_owned)
        .ok_or_else(|| miette::miette!("this is not a {}", envelope.1))
}

impl<R: BufRead> Decryptor<R> {
    /// Read `envelope`'s header from `input` and check it against
    /// `passphrase`.
    ///
    /// # Errors
    /// Fails for another kind of file, a header with costs above the limits,
    /// or a wrong passphrase.
    pub fn open(mut input: R, envelope: Envelope, passphrase: &str) -> miette::Result<Self> {
        let first = header_line(&mut input, envelope, "")?;
        miette::ensure!(
            first == format!("{} v1", envelope.0),
            "this is not a {} this Trellis reads (its header says {first:?})",
            envelope.1
        );
        let costs = header_line(&mut input, envelope, "argon2id ")?;
        let nonce_text = header_line(&mut input, envelope, "nonce=")?;
        let mac = header_line(&mut input, envelope, "mac=")?;
        let cost = |name: &str, max: u32| -> miette::Result<u32> {
            costs
                .split(' ')
                .find_map(|part| part.strip_prefix(&format!("{name}=")))
                .and_then(|value| value.parse().ok())
                .filter(|value| *value <= max)
                .ok_or_else(|| miette::miette!("the bundle's header has no usable {name}"))
        };
        let costs_used = (
            cost("m", MAX_MEMORY_KIB)?,
            cost("t", MAX_PASSES)?,
            cost("p", MAX_LANES)?,
        );
        let salt = costs
            .split(' ')
            .find_map(|part| part.strip_prefix("salt="))
            .and_then(|salt| URL_SAFE_NO_PAD.decode(salt).ok())
            .ok_or_else(|| miette::miette!("the bundle's header has no salt"))?;
        let nonce = URL_SAFE_NO_PAD
            .decode(&nonce_text)
            .map_err(|_| miette::miette!("the bundle's header has no nonce"))?;
        let mac = URL_SAFE_NO_PAD
            .decode(&mac)
            .map_err(|_| miette::miette!("the bundle's header has no MAC"))?;
        let keys = derive(envelope, passphrase, &salt, costs_used, &nonce)?;
        let header = format!("{} v1\nargon2id {costs}\nnonce={nonce_text}\n", envelope.0);
        hmac::verify(&keys.header, header.as_bytes(), &mac).map_err(|_| {
            miette::miette!(
                help = "Check the passphrase the file was exported with.",
                "the passphrase is wrong, or the {}'s header is damaged",
                envelope.1
            )
        })?;
        Ok(Self {
            input,
            key: keys.payload,
            counter: 0,
            buffer: Vec::new(),
            position: 0,
            finished: false,
        })
    }
}

impl<R: BufRead> Read for Decryptor<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.position == self.buffer.len() {
            if self.finished {
                return Ok(0);
            }
            let mut chunk = Vec::with_capacity(CHUNK + TAG);
            (&mut self.input)
                .take((CHUNK + TAG) as u64)
                .read_to_end(&mut chunk)?;
            // A chunk shorter than a full one, or one with nothing after it,
            // is the last.
            let last = chunk.len() < CHUNK + TAG || self.input.fill_buf()?.is_empty();
            let damaged =
                || std::io::Error::other("the bundle is damaged: a chunk does not decrypt");
            let plain = self
                .key
                .open_in_place(chunk_nonce(self.counter, last), Aad::empty(), &mut chunk)
                .map_err(|_| damaged())?
                .len();
            chunk.truncate(plain);
            self.counter += 1;
            self.buffer = chunk;
            self.position = 0;
            self.finished = last;
        }
        let count = out.len().min(self.buffer.len() - self.position);
        out[..count].copy_from_slice(&self.buffer[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealed(content: &[u8], passphrase: &str) -> Vec<u8> {
        let mut encryptor = Encryptor::new(Vec::new(), SERVICE_BUNDLE, passphrase).unwrap();
        encryptor.write(content).unwrap();
        encryptor.finish().unwrap()
    }

    fn opened(bundle: &[u8], passphrase: &str) -> miette::Result<Vec<u8>> {
        let mut decryptor =
            Decryptor::open(std::io::BufReader::new(bundle), SERVICE_BUNDLE, passphrase)?;
        let mut content = Vec::new();
        decryptor.read_to_end(&mut content).into_diagnostic()?;
        Ok(content)
    }

    /// Content of every length class comes back as written: empty, inside
    /// one chunk, exactly one chunk, and across several.
    #[test]
    fn a_bundle_opens_with_its_passphrase_to_the_content_written() {
        for length in [0, 5, CHUNK, 3 * CHUNK + 17] {
            let content: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
            let bundle = sealed(&content, "correct horse battery");
            assert_eq!(opened(&bundle, "correct horse battery").unwrap(), content);
        }
    }

    /// A changed byte, a dropped final chunk, or swapped chunks fail the
    /// content, even under the right passphrase.
    #[test]
    fn changed_or_reordered_content_does_not_decrypt() {
        let content: Vec<u8> = (0..2 * CHUNK + 9).map(|index| (index % 7) as u8).collect();
        let bundle = sealed(&content, "correct horse battery");
        let header = bundle
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'\n')
            .nth(3)
            .unwrap()
            .0
            + 1;
        let mut changed = bundle.clone();
        changed[header + 100] ^= 1;
        assert!(opened(&changed, "correct horse battery").is_err());
        let cut = &bundle[..header + CHUNK + TAG];
        assert!(opened(cut, "correct horse battery").is_err());
        let mut swapped = bundle[..header].to_vec();
        swapped.extend_from_slice(&bundle[header + CHUNK + TAG..header + 2 * (CHUNK + TAG)]);
        swapped.extend_from_slice(&bundle[header..header + CHUNK + TAG]);
        swapped.extend_from_slice(&bundle[header + 2 * (CHUNK + TAG)..]);
        assert!(opened(&swapped, "correct horse battery").is_err());
    }
}
