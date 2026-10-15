//! Signature verification for updates (SPEC §12a, rule U1).
//!
//! Every file an update installs (the platform archive/package and the
//! release's installer script) must carry a minisign signature that verifies
//! against the public key compiled into this binary. Uses the pure-Rust
//! `minisign-verify` crate (0.3.0): <https://docs.rs/minisign-verify/0.3.0/minisign_verify/>
//! — `PublicKey::decode`, `Signature::decode`, `PublicKey::verify`,
//! `PublicKey::verify_stream` / `StreamVerifier`.
//!
//! Only pre-hashed signatures (the `minisign -S` default) are accepted:
//! `allow_legacy` is never set, and the streaming verifier refuses legacy
//! signatures anyway.

use minisign_verify::{Error as MinisignError, PublicKey, Signature, StreamVerifier};

/// The release signing public key, minisign `.pub` file format. Replaced by
/// the real key before the first release; `embedded_key_is_valid` fails the
/// build while it is a placeholder.
pub const EMBEDDED_PUBLIC_KEY: &str = include_str!("../../../resources/update-signing.pub");

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// The key text does not parse (a placeholder, or a damaged file).
    #[error("the update signing key built into this app is invalid ({0})")]
    BadKey(String),
    /// The `.minisig` text does not parse, or uses an unsupported algorithm.
    #[error("the signature file is malformed ({0})")]
    Malformed(String),
    /// Well-formed, but not a signature of these bytes by this key.
    #[error("the signature does not match the downloaded file")]
    Mismatch,
    /// Made by a different key than the one built into this app.
    #[error("the file was signed with a different key than this app trusts")]
    WrongKey,
    /// A valid signature, but of a different file or release than asked for.
    #[error("the signature is for another file or release (trusted comment: '{0}')")]
    NotBound(String),
}

impl From<MinisignError> for VerifyError {
    fn from(e: MinisignError) -> Self {
        match e {
            MinisignError::InvalidSignature => Self::Mismatch,
            MinisignError::UnexpectedKeyId => Self::WrongKey,
            other => Self::Malformed(other.to_string()),
        }
    }
}

/// A trusted public key.
pub struct Verifier {
    key: PublicKey,
}

impl Verifier {
    /// The key compiled into this binary.
    pub fn embedded() -> Result<Self, VerifyError> {
        Self::from_key_text(EMBEDDED_PUBLIC_KEY)
    }

    /// Parse a minisign `.pub` file's text (comment line + base64 line).
    pub fn from_key_text(text: &str) -> Result<Self, VerifyError> {
        PublicKey::decode(text)
            .map(|key| Self { key })
            .map_err(|e| VerifyError::BadKey(e.to_string()))
    }

    /// Parse the text of a `.minisig` file.
    pub fn parse_signature(text: &str) -> Result<Signature, VerifyError> {
        Signature::decode(text).map_err(|e| VerifyError::Malformed(e.to_string()))
    }

    /// Verify `data` against `signature`, then require the signed trusted
    /// comment to name `file` and `tag`.
    pub fn verify(
        &self,
        data: &[u8],
        signature: &Signature,
        file: &str,
        tag: &str,
    ) -> Result<(), VerifyError> {
        self.key.verify(data, signature, false)?;
        check_binding(signature.trusted_comment(), file, tag)
    }

    /// Start a streaming verification (large archives). Feed it with
    /// [`StreamVerifier::update`], then call [`finish_stream`].
    pub fn stream<'a>(
        &'a self,
        signature: &'a Signature,
    ) -> Result<StreamVerifier<'a>, VerifyError> {
        Ok(self.key.verify_stream(signature)?)
    }
}

/// Finish a streaming verification and check the file/tag binding.
pub fn finish_stream(
    mut stream: StreamVerifier<'_>,
    signature: &Signature,
    file: &str,
    tag: &str,
) -> Result<(), VerifyError> {
    stream.finalize()?;
    check_binding(signature.trusted_comment(), file, tag)
}

/// The trusted comment is `timestamp:<unix>\tfile:<name>\ttag:<tag>`
/// (tab-separated, as the release workflow writes it). It is authenticated by
/// the global signature, which `verify` / `finalize` have already checked, so
/// reading it here is safe. Binding the name and tag stops a validly signed
/// asset of another release (or another file) being served instead.
fn check_binding(trusted_comment: &str, file: &str, tag: &str) -> Result<(), VerifyError> {
    let has = |key: &str, value: &str| {
        trusted_comment
            .split('\t')
            .any(|field| field.strip_prefix(key).and_then(|f| f.strip_prefix(':')) == Some(value))
    };
    if has("file", file) && has("tag", tag) {
        Ok(())
    } else {
        Err(VerifyError::NotBound(trusted_comment.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PUB: &str = include_str!("../../../tests/fixtures/update/test.pub");
    const OTHER_PUB: &str = include_str!("../../../tests/fixtures/update/other.pub");
    const PAYLOAD: &[u8] = include_bytes!("../../../tests/fixtures/update/payload.bin");
    const SIG_GOOD: &str = include_str!("../../../tests/fixtures/update/payload.bin.minisig");
    const SIG_OTHER_KEY: &str =
        include_str!("../../../tests/fixtures/update/payload.bin.otherkey.minisig");
    const SIG_WRONG_FILE: &str =
        include_str!("../../../tests/fixtures/update/payload.bin.wrongfile.minisig");
    const SIG_WRONG_TAG: &str =
        include_str!("../../../tests/fixtures/update/payload.bin.wrongtag.minisig");
    const SIG_LEGACY: &str =
        include_str!("../../../tests/fixtures/update/payload.bin.legacy.minisig");

    const FILE: &str = "payload.bin";
    const TAG: &str = "v9.9.9";

    fn verifier() -> Verifier {
        Verifier::from_key_text(TEST_PUB).expect("fixture key parses")
    }

    /// The key shipped in the binary must be a real, parseable minisign key.
    /// This is the gate that stops a build with the placeholder key.
    #[test]
    fn embedded_key_is_valid() {
        let v = Verifier::embedded();
        assert!(
            v.is_ok(),
            "resources/update-signing.pub is not a valid minisign public key: {:?}",
            v.err()
        );
        assert!(
            !EMBEDDED_PUBLIC_KEY
                .to_ascii_lowercase()
                .contains("placeholder"),
            "resources/update-signing.pub still holds the placeholder key"
        );
    }

    /// Proves the gate above bites: placeholder-shaped text must not parse.
    #[test]
    fn placeholder_key_is_rejected() {
        for text in [
            "untrusted comment: minisign public key PLACEHOLDER\nREPLACE_WITH_REAL_KEY\n",
            "untrusted comment: x\nnot base64 at all !!!\n",
            "",
            "untrusted comment: only one line\n",
        ] {
            assert!(
                Verifier::from_key_text(text).is_err(),
                "placeholder accepted: {text:?}"
            );
        }
    }

    #[test]
    fn good_signature_verifies() {
        let sig = Verifier::parse_signature(SIG_GOOD).unwrap();
        verifier().verify(PAYLOAD, &sig, FILE, TAG).unwrap();
    }

    #[test]
    fn good_signature_verifies_streaming() {
        let sig = Verifier::parse_signature(SIG_GOOD).unwrap();
        let v = verifier();
        let mut s = v.stream(&sig).unwrap();
        for chunk in PAYLOAD.chunks(7) {
            s.update(chunk);
        }
        finish_stream(s, &sig, FILE, TAG).unwrap();
    }

    #[test]
    fn tampered_content_is_rejected() {
        let sig = Verifier::parse_signature(SIG_GOOD).unwrap();
        let mut bad = PAYLOAD.to_vec();
        bad[0] ^= 0x01;
        assert!(matches!(
            verifier().verify(&bad, &sig, FILE, TAG),
            Err(VerifyError::Mismatch)
        ));
        // And through the streaming path.
        let v = verifier();
        let mut s = v.stream(&sig).unwrap();
        s.update(&bad);
        assert!(matches!(
            finish_stream(s, &sig, FILE, TAG),
            Err(VerifyError::Mismatch)
        ));
    }

    #[test]
    fn signature_by_another_key_is_rejected() {
        let sig = Verifier::parse_signature(SIG_OTHER_KEY).unwrap();
        assert!(matches!(
            verifier().verify(PAYLOAD, &sig, FILE, TAG),
            Err(VerifyError::WrongKey)
        ));
        // The same bytes DO verify under the key that made that signature,
        // so the rejection above is about the key and nothing else.
        let other = Verifier::from_key_text(OTHER_PUB).unwrap();
        other.verify(PAYLOAD, &sig, FILE, TAG).unwrap();
    }

    #[test]
    fn signature_for_another_file_or_tag_is_rejected() {
        let v = verifier();
        for text in [SIG_WRONG_FILE, SIG_WRONG_TAG] {
            let sig = Verifier::parse_signature(text).unwrap();
            assert!(matches!(
                v.verify(PAYLOAD, &sig, FILE, TAG),
                Err(VerifyError::NotBound(_))
            ));
        }
        // A good signature asked about the wrong name/tag is also unbound.
        let sig = Verifier::parse_signature(SIG_GOOD).unwrap();
        assert!(matches!(
            v.verify(PAYLOAD, &sig, "other.bin", TAG),
            Err(VerifyError::NotBound(_))
        ));
        assert!(matches!(
            v.verify(PAYLOAD, &sig, FILE, "v9.9.8"),
            Err(VerifyError::NotBound(_))
        ));
    }

    #[test]
    fn legacy_signature_is_refused() {
        let sig = Verifier::parse_signature(SIG_LEGACY).unwrap();
        let v = verifier();
        assert!(v.verify(PAYLOAD, &sig, FILE, TAG).is_err());
        assert!(v.stream(&sig).is_err());
    }

    #[test]
    fn malformed_signature_text_is_rejected() {
        for text in ["", "garbage", "untrusted comment: x\nAAAA\n"] {
            assert!(matches!(
                Verifier::parse_signature(text),
                Err(VerifyError::Malformed(_))
            ));
        }
    }
}
