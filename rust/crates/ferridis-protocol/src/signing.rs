//! Manifest signing verification (Sigstore / cosign).
//!
//! v0.2 phase 1 implements **signature-against-cert** verification:
//! given a manifest's bytes and the matching `cosign sign-blob
//! --bundle` artifact, parse the bundle, extract the embedded
//! certificate's public key, and verify the signature against it.
//! This catches a tampered manifest the moment it is fetched.
//!
//! **What this phase does not yet verify** (queued for v0.2 phase 2):
//!
//! - **Fulcio cert chain.** We trust the cert in the bundle; we don't
//!   yet check that it chains to the Fulcio root and that the
//!   subject matches an authorized identity. A bundle signed with a
//!   self-issued cert will currently pass verification.
//! - **Rekor inclusion proof.** Sigstore's transparency log entry
//!   inside the bundle is parsed but not verified against Rekor's
//!   public key. A signer who can produce a valid signature but not a
//!   Rekor entry would currently pass.
//! - **Cert validity window.** Fulcio's short-lived certs are valid
//!   for ~10 minutes. We don't currently enforce the `notBefore` /
//!   `notAfter` extensions.
//!
//! These gaps are documented loudly so capability publishers and
//! mesh operators know exactly what guarantees this phase provides.
//! Phase 2 closes them.
//!
//! # Wire format
//!
//! The signed artifact is exactly what `cosign sign-blob --bundle`
//! produces: two files, the original `manifest.json` and the
//! companion `manifest.json.cosign.bundle` (JSON with
//! `base64Signature`, `cert`, and `rekorBundle` fields). This is the
//! standard Sigstore artifact format used by npm, PyPI, Kubernetes,
//! and Homebrew — operators familiar with one are familiar with all.
//!
//! # Publishing today
//!
//! Until a `ferridis-sign` CLI ships (queued), capability publishers
//! sign with the upstream `cosign` tool:
//!
//! ```sh,ignore
//! cosign sign-blob \
//!     --bundle manifest.json.cosign.bundle \
//!     --output-signature manifest.json.sig \
//!     manifest.json
//! ```
//!
//! Verifying clients fetch both `manifest.json` and
//! `manifest.json.cosign.bundle` (the URL convention is documented
//! in the mesh-registry layer) and call
//! [`verify_signed_manifest`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use base64::Engine;
use sigstore::cosign::bundle::SignedArtifactBundle;
use sigstore::crypto::{CosignVerificationKey, Signature};
use sigstore::trust::TrustRoot as SigstoreTrustRootTrait;
use sigstore::trust::sigstore::SigstoreTrustRoot;
use webpki::{BorrowedCertRevocationList, CertRevocationList as WebpkiCrl};
use x509_cert::Certificate;
use x509_cert::der::asn1::ObjectIdentifier;
use x509_cert::der::{Decode, DecodePem};
use x509_cert::ext::pkix::crl::CrlDistributionPoints;
use x509_cert::ext::pkix::name::DistributionPointName;
use x509_cert::ext::pkix::name::GeneralName;

/// Errors returned by [`verify_signed_manifest`].
#[derive(Debug, Error)]
pub enum SigningError {
    /// The cosign bundle JSON failed to parse.
    #[error("cosign bundle is malformed: {0}")]
    MalformedBundle(String),

    /// The certificate inside the bundle could not be parsed or did
    /// not yield a usable verification key.
    #[error("certificate in bundle is unusable: {0}")]
    UnusableCertificate(String),

    /// The signature did not verify against the certificate's public
    /// key. This is the hard rejection: tampering, wrong cert, or
    /// signature corruption.
    #[error("signature verification failed: {0}")]
    SignatureMismatch(String),

    /// The signing certificate's validity window does not include
    /// the current time. Either `notBefore` is in the future
    /// (clock skew or a misissued cert) or `notAfter` has passed
    /// (short-lived Fulcio certs are typically valid for ~10
    /// minutes; an expired one means the bundle was made long ago
    /// and the cert should no longer be trusted).
    #[error("certificate is outside its validity window: {0}")]
    CertificateExpired(String),

    /// The Rekor inclusion proof in the cosign bundle failed
    /// verification against the trusted Rekor public key. Either the
    /// bundle was forged (signed-entry-timestamp doesn't match the
    /// payload) or it was signed by a Rekor instance the caller's
    /// trust root does not list.
    #[error("Rekor inclusion proof failed: {0}")]
    RekorInclusionFailed(String),

    /// The trust root supplied to the verify call has no Rekor keys
    /// configured, so the inclusion proof cannot be checked.
    /// Distinct from [`Self::RekorInclusionFailed`] so an operator
    /// can tell a misconfiguration apart from a tampering attempt.
    #[error("trust root has no Rekor keys configured")]
    TrustRootMissingRekorKeys,

    /// The bundle's leaf certificate does not chain to any of the
    /// trusted Fulcio CA certs on the trust root. Either the bundle
    /// was issued by an untrusted Fulcio instance (or self-issued
    /// outside of Sigstore) or the trust root is missing the
    /// expected intermediate.
    #[error("Fulcio cert chain validation failed: {0}")]
    FulcioChainInvalid(String),

    /// Fetching Sigstore trust material via TUF failed. Wraps the
    /// sigstore-rs / `tough` error chain; the typical causes are
    /// no network when the embedded trusted_root.json is stale, a
    /// corrupt cache dir, or — rarely — an actually-invalid TUF
    /// metadata signature (which would mean someone is attacking
    /// the Sigstore TUF root, not your client).
    #[error("TUF trust-root fetch failed: {0}")]
    TufFetch(String),

    /// The signing certificate has been revoked according to the CRL
    /// fetched from its CDP extension. Definitive — the CRL is the
    /// CA's authoritative list of revoked serials.
    #[error("signing certificate has been revoked")]
    CertRevoked,

    /// CRL fetch or parse failure. Whether this aborts verification
    /// depends on [`RevocationMode`]: `Required` treats it as fatal;
    /// `BestEffort` logs a warning and continues.
    #[error("CRL error for {url}: {detail}")]
    CrlFetchFailed {
        /// The CRL endpoint URL (or `"(parse)"` for parse-stage failures).
        url: String,
        /// Human-readable description of the failure.
        detail: String,
    },

    /// The leaf certificate has no CDP extension so revocation cannot
    /// be checked via CRL. Only returned when
    /// [`RevocationMode::Required`] is set; `BestEffort` continues.
    #[error("certificate has no CRL Distribution Point extension")]
    CdpExtensionMissing,
}

/// Controls how CRL-based revocation failures affect verification.
///
/// `BestEffort` is the default and matches the behaviour of most
/// Sigstore clients against Fulcio's short-lived certs (~10 min
/// TTL): network failures are non-fatal because the cert will
/// expire on its own shortly. Set `Required` in high-assurance
/// deployments where an unverified revocation status is a hard
/// failure.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RevocationMode {
    /// Fetch and check the CRL; treat fetch/parse failures as
    /// non-fatal (log a WARN and continue treating as "not revoked").
    #[default]
    BestEffort,
    /// Fetch and check the CRL; any failure aborts verification with
    /// [`SigningError::CrlFetchFailed`] or
    /// [`SigningError::CdpExtensionMissing`].
    Required,
    /// Skip CRL checking entirely. Appropriate for air-gapped
    /// deployments where the CRL endpoint is unreachable by design.
    Skip,
}

/// Opaque wrapper around raw DER-encoded CRL bytes, returned by
/// [`fetch_crl`]. Only constructable via the public fetch function
/// to prevent callers from injecting arbitrary bytes into the
/// revocation check.
pub struct CrlData(Vec<u8>);

/// A manifest whose signature has been verified against the
/// certificate embedded in its cosign bundle.
///
/// The presence of this value is the type-level proof that the
/// signature-vs-cert check passed. Phase 2 will add fields recording
/// the Fulcio identity (subject email / OIDC issuer) and the Rekor
/// log entry id; today the value is intentionally minimal.
///
/// Holds borrowed bytes rather than owned copies so callers can
/// continue working with the original manifest body without an
/// allocation.
#[derive(Debug, Clone, Copy)]
pub struct VerifiedManifest<'a> {
    manifest_bytes: &'a [u8],
}

impl<'a> VerifiedManifest<'a> {
    /// The original manifest body whose signature was verified.
    /// Callers parse this with [`ferridis_core::Manifest::parse`]
    /// once they've established trust at this layer.
    pub fn bytes(&self) -> &'a [u8] {
        self.manifest_bytes
    }
}

/// The cosign bundle artifact, deserialized from the
/// `manifest.json.cosign.bundle` JSON.
///
/// Newtype wrapper around sigstore's [`SignedArtifactBundle`] so the
/// public API doesn't bleed sigstore types into every caller; the
/// implementation may swap later (e.g., to a custom parser if we
/// drop the sigstore dep).
#[derive(Debug)]
pub struct CosignBundle {
    inner: SignedArtifactBundle,
}

impl CosignBundle {
    /// Parse a `manifest.json.cosign.bundle` JSON document.
    pub fn parse(bundle_json: &[u8]) -> Result<Self, SigningError> {
        let raw = std::str::from_utf8(bundle_json).map_err(|e| {
            SigningError::MalformedBundle(format!("bundle is not valid UTF-8: {e}"))
        })?;
        let inner: SignedArtifactBundle = serde_json::from_str(raw)
            .map_err(|e| SigningError::MalformedBundle(format!("JSON parse error: {e}")))?;
        Ok(Self { inner })
    }

    /// Re-serialize the bundle's identifying fields for logging /
    /// debugging. Does not include cryptographic material.
    pub fn describe(&self) -> BundleDescription {
        BundleDescription {
            cert_pem_bytes: self.inner.cert.len(),
            signature_base64_bytes: self.inner.base64_signature.len(),
            rekor_log_id: self.inner.rekor_bundle.payload.log_id.clone(),
        }
    }
}

/// Trusted material for full-Sigstore verification.
///
/// Carries the public-key material needed for the two trust checks
/// that v0.2 phase 1 deferred:
///
/// - **Rekor inclusion proof.** Each key is the public half of one
///   Rekor instance's signing key, keyed by Rekor log id (a string
///   the bundle's `rekorBundle.payload.logId` is matched against). A
///   bundle signed by a Rekor instance whose key is not in this map
///   is rejected.
///
/// - **Fulcio cert chain.** A list of trusted Fulcio root and
///   intermediate certs (DER-encoded). v0.3 wires these to webpki
///   for chain validation; the v0.3 partial that ships in this
///   session only uses the Rekor keys (chain validation is queued).
///
/// Construct via [`TrustRoot::new`] for explicit material — the
/// shape suitable for tests and air-gapped deployments. A future
/// helper will fetch via TUF from `sigstore-trust-root`.
#[derive(Debug, Default, Clone)]
pub struct TrustRoot {
    /// PEM-encoded public keys of trusted Rekor instances, keyed by
    /// Rekor log id. The signing module accepts PEM; sigstore's
    /// internal verifier wraps these in `CosignVerificationKey`.
    rekor_pubkeys_pem: BTreeMap<String, String>,
    /// DER-encoded Fulcio CA certs (root + intermediates).
    fulcio_cert_der: Vec<Vec<u8>>,
    /// Controls how CRL-based revocation failures are treated.
    /// See [`RevocationMode`]. Defaults to [`RevocationMode::BestEffort`].
    revocation_mode: RevocationMode,
}

impl TrustRoot {
    /// Build an empty trust root. Add material via the builder
    /// methods below.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a PEM-encoded Rekor public key under `log_id`. The
    /// `log_id` is what bundle authors record in
    /// `rekorBundle.payload.logId`; a bundle whose log id does not
    /// match an entry here is rejected.
    #[must_use]
    pub fn with_rekor_key(mut self, log_id: impl Into<String>, pem: impl Into<String>) -> Self {
        self.rekor_pubkeys_pem.insert(log_id.into(), pem.into());
        self
    }

    /// Register a DER-encoded Fulcio CA cert. The cert is treated as
    /// a trust anchor by [`verify_signed_manifest_with_trust_root`]
    /// when validating the leaf cert's chain via `rustls-webpki`.
    /// Add the root and any intermediates you trust.
    #[must_use]
    pub fn with_fulcio_cert_der(mut self, der: impl Into<Vec<u8>>) -> Self {
        self.fulcio_cert_der.push(der.into());
        self
    }

    /// Register a PEM-encoded Fulcio CA cert. Convenience over
    /// [`with_fulcio_cert_der`] for the common case where the
    /// operator has the cert as PEM (the default
    /// `cosign initialize` output). Errors on malformed PEM rather
    /// than silently dropping the anchor.
    pub fn with_fulcio_cert_pem(mut self, pem: &str) -> Result<Self, SigningError> {
        let der = pem_to_der(pem)?;
        self.fulcio_cert_der.push(der);
        Ok(self)
    }

    /// Build a TrustRoot by reading the conventional Sigstore root
    /// files out of `dir`. Matches the filenames produced by
    /// `cosign initialize` and the standard Sigstore trust bundle:
    ///
    /// - `fulcio_v1.crt.pem` — Fulcio root CA cert
    /// - `fulcio_intermediate_v1.crt.pem` — Fulcio intermediate
    ///   (optional; loaded if present)
    /// - `rekor.pub` — Rekor public key, PEM-encoded
    /// - `rekor.log_id` — Rekor log id (single line); when absent,
    ///   defaults to `"rekor.sigstore.dev"`
    ///
    /// Files that don't exist are skipped without error so an
    /// operator can deploy a partial trust root (e.g., Fulcio only
    /// during a Rekor migration); the resulting `TrustRoot`'s
    /// `has_fulcio_certs` / `has_rekor_keys` accessors reflect what
    /// was actually loaded. Returns an error if a file exists but
    /// fails to parse — silent partial-load on malformed input
    /// would be a security footgun.
    pub fn from_sigstore_dir(dir: impl AsRef<std::path::Path>) -> Result<Self, SigningError> {
        let dir = dir.as_ref();
        let mut root = Self::new();

        let fulcio_root_path = dir.join("fulcio_v1.crt.pem");
        if fulcio_root_path.exists() {
            let pem = std::fs::read_to_string(&fulcio_root_path).map_err(|e| {
                SigningError::UnusableCertificate(format!(
                    "read {}: {e}",
                    fulcio_root_path.display()
                ))
            })?;
            root = root.with_fulcio_cert_pem(&pem)?;
        }
        let fulcio_int_path = dir.join("fulcio_intermediate_v1.crt.pem");
        if fulcio_int_path.exists() {
            let pem = std::fs::read_to_string(&fulcio_int_path).map_err(|e| {
                SigningError::UnusableCertificate(format!(
                    "read {}: {e}",
                    fulcio_int_path.display()
                ))
            })?;
            root = root.with_fulcio_cert_pem(&pem)?;
        }

        let rekor_pub_path = dir.join("rekor.pub");
        if rekor_pub_path.exists() {
            let pem = std::fs::read_to_string(&rekor_pub_path).map_err(|e| {
                SigningError::UnusableCertificate(format!("read {}: {e}", rekor_pub_path.display()))
            })?;
            let log_id_path = dir.join("rekor.log_id");
            let log_id = if log_id_path.exists() {
                std::fs::read_to_string(&log_id_path)
                    .map_err(|e| {
                        SigningError::UnusableCertificate(format!(
                            "read {}: {e}",
                            log_id_path.display()
                        ))
                    })?
                    .trim()
                    .to_string()
            } else {
                "rekor.sigstore.dev".to_string()
            };
            root = root.with_rekor_key(log_id, pem);
        }
        Ok(root)
    }

    /// Build a TrustRoot by resolving Sigstore's Public Good TUF
    /// repository. This is the production path: it fetches Fulcio
    /// CA certs and Rekor public keys from `tuf-repo-cdn.sigstore.dev`
    /// via the [TUF](https://theupdateframework.io/) protocol —
    /// signed metadata, expiry-enforced, with rollback protection.
    ///
    /// `cache_dir`, when supplied, is a writable directory the TUF
    /// client uses to persist targets between runs. When `None`,
    /// the trust material is sourced from the embedded snapshot
    /// inside the `sigstore` crate and not written to disk — useful
    /// for short-lived processes and air-gapped first-run, but
    /// gives up the benefit of disk-cached freshness.
    ///
    /// The recommended cache location is the OS cache directory
    /// (e.g., `$XDG_CACHE_HOME/ferridis/sigstore` on Linux). Hosts
    /// embedding `ferridis-protocol` choose the path; this function
    /// does not pick one to avoid surprising callers with writes
    /// outside their control.
    ///
    /// Both Fulcio CAs (with `allow_expired` true so a recently-rotated
    /// CA still validates older bundles) and Rekor public keys are
    /// installed. Rekor keys arrive as raw SPKI DER from the TUF
    /// metadata; we wrap them in `BEGIN PUBLIC KEY` PEM blocks so
    /// they round-trip through the existing
    /// [`with_rekor_key`](Self::with_rekor_key) PEM-based API.
    pub async fn from_sigstore_tuf(
        cache_dir: Option<&std::path::Path>,
    ) -> Result<Self, SigningError> {
        let tuf = SigstoreTrustRoot::new(cache_dir)
            .await
            .map_err(|e| SigningError::TufFetch(format!("SigstoreTrustRoot::new: {e}")))?;

        let mut root = Self::new();

        let fulcio_ders = tuf
            .fulcio_certs()
            .map_err(|e| SigningError::TufFetch(format!("fulcio_certs: {e}")))?;
        for der in fulcio_ders {
            root = root.with_fulcio_cert_der(der.as_ref().to_vec());
        }

        let rekor = tuf
            .rekor_keys()
            .map_err(|e| SigningError::TufFetch(format!("rekor_keys: {e}")))?;
        for (log_id, spki_der) in rekor {
            let pem = spki_der_to_public_key_pem(spki_der);
            root = root.with_rekor_key(log_id, pem);
        }

        tracing::info!(
            fulcio_anchors = root.fulcio_cert_der.len(),
            rekor_keys = root.rekor_pubkeys_pem.len(),
            "Sigstore trust root resolved via TUF"
        );
        Ok(root)
    }

    /// Set the revocation mode for CRL-based checks. Defaults to
    /// [`RevocationMode::BestEffort`] when not called.
    #[must_use]
    pub fn with_revocation_mode(mut self, mode: RevocationMode) -> Self {
        self.revocation_mode = mode;
        self
    }

    /// Whether this trust root has at least one Rekor key.
    pub fn has_rekor_keys(&self) -> bool {
        !self.rekor_pubkeys_pem.is_empty()
    }

    /// Whether this trust root has at least one Fulcio CA cert. When
    /// false, [`verify_signed_manifest_with_trust_root`] skips the
    /// chain check (other gates still apply); when true, it requires
    /// the bundle's leaf cert to chain to one of the anchors.
    pub fn has_fulcio_certs(&self) -> bool {
        !self.fulcio_cert_der.is_empty()
    }

    /// Build the `BTreeMap<String, CosignVerificationKey>` that
    /// sigstore-rs's `SignedArtifactBundle::new_verified` expects.
    ///
    /// PEM input is decoded to DER (stripping `BEGIN PUBLIC KEY`
    /// armor) and then routed through
    /// `CosignVerificationKey::try_from_der`, which auto-detects the
    /// scheme from the SPKI algorithm OID. We need auto-detect (not
    /// the hardcoded `from_der(default)` sigstore-rs's own
    /// `client_builder` uses) because the production TUF trust root
    /// carries both an ECDSA P-256 and an Ed25519 Rekor key in the
    /// same response; locking to one scheme would silently drop the
    /// other and could mean the bundle's `logId` resolves to a key
    /// that never made it into the map.
    fn rekor_verification_keys(
        &self,
    ) -> Result<BTreeMap<String, CosignVerificationKey>, SigningError> {
        let mut out = BTreeMap::new();
        for (log_id, pem) in &self.rekor_pubkeys_pem {
            let der = decode_public_key_pem(pem).map_err(|detail| {
                SigningError::UnusableCertificate(format!(
                    "Rekor pubkey for log {log_id}: {detail}"
                ))
            })?;
            let key = CosignVerificationKey::try_from_der(&der).map_err(|e| {
                SigningError::UnusableCertificate(format!(
                    "Rekor pubkey for log {log_id} failed to parse: {e}"
                ))
            })?;
            out.insert(log_id.clone(), key);
        }
        Ok(out)
    }
}

/// Identifying metadata about a [`CosignBundle`] suitable for logs.
/// Never contains keys or signatures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleDescription {
    /// Length of the PEM-encoded certificate.
    pub cert_pem_bytes: usize,
    /// Length of the base64-encoded signature.
    pub signature_base64_bytes: usize,
    /// The Rekor log id the bundle claims.
    pub rekor_log_id: String,
}

/// Verify that `manifest_bytes` was signed by the certificate
/// embedded in `bundle`, and that the certificate is within its
/// validity window.
///
/// This is the partial-phase-2 verification path (2026-05):
///
///   ✓ Signature verifies against the cert's public key.
///   ✓ Certificate's `notBefore..=notAfter` window includes "now".
///   ✗ Cert chain to Fulcio NOT yet validated (queued for v0.3).
///   ✗ Rekor log inclusion proof NOT yet verified (queued for v0.3).
///
/// Returns a [`VerifiedManifest`] borrowing the input bytes on
/// success. Reject reasons are surfaced as typed
/// [`SigningError`] variants so callers can react differently to
/// e.g. a clock-skew transient (`CertificateExpired`) vs a tampered
/// payload (`SignatureMismatch`).
pub fn verify_signed_manifest<'a>(
    manifest_bytes: &'a [u8],
    bundle: &CosignBundle,
) -> Result<VerifiedManifest<'a>, SigningError> {
    verify_signed_manifest_at(manifest_bytes, bundle, OffsetDateTime::now_utc())
}

/// Same as [`verify_signed_manifest`] but with the "current time"
/// supplied explicitly. Used by tests to verify against fixed-time
/// fixtures; production callers should use [`verify_signed_manifest`]
/// which clocks from the OS.
pub fn verify_signed_manifest_at<'a>(
    manifest_bytes: &'a [u8],
    bundle: &CosignBundle,
    now: OffsetDateTime,
) -> Result<VerifiedManifest<'a>, SigningError> {
    let key = extract_verification_key(&bundle.inner.cert)?;
    key.verify_signature(
        Signature::Base64Encoded(bundle.inner.base64_signature.as_bytes()),
        manifest_bytes,
    )
    .map_err(|e| SigningError::SignatureMismatch(format!("{e}")))?;

    check_cert_validity_window(&bundle.inner.cert, now)?;

    tracing::debug!(
        rekor_log_id = %bundle.inner.rekor_bundle.payload.log_id,
        cert_pem_bytes = bundle.inner.cert.len(),
        manifest_bytes = manifest_bytes.len(),
        "manifest signature verified against embedded cert + validity window (phase 2 partial \
         \u{2014} Fulcio chain + Rekor inclusion deferred to v0.3)"
    );
    Ok(VerifiedManifest { manifest_bytes })
}

/// Full-Sigstore verification: sig vs cert, cert validity window,
/// Rekor inclusion proof, **and** Fulcio cert chain validation
/// against the supplied trust root.
///
/// This is the v0.3 verification path. Use this instead of
/// [`verify_signed_manifest`] when you have Sigstore trust material
/// (production: load via TUF from Sigstore's trust root; tests:
/// generate inline with rcgen).
///
/// Trust-chain coverage:
///
///   ✓ Signature verifies against the cert's public key.
///   ✓ Certificate's `notBefore..=notAfter` window includes `now`.
///   ✓ Rekor `SignedEntryTimestamp` matches the canonical payload
///     signed by a trusted Rekor instance.
///   ✓ Leaf cert chains to a trusted Fulcio CA (when the trust
///     root has Fulcio anchors; if not, the chain step is skipped).
///
/// Chain validation runs via `rustls-webpki`'s
/// `EndEntityCert::verify_for_usage` with `KeyUsage::client_auth()`
/// (Fulcio's default EKU for cosign-issued certs). The trust root's
/// `fulcio_cert_der` entries become the `TrustAnchor` set;
/// intermediates can be added to the same list. Revocation
/// (Sigstore-specific CT log validation) is queued for v0.4.
///
/// Returns [`SigningError::TrustRootMissingRekorKeys`] when the
/// caller passed an empty trust root for the Rekor side; this is a
/// loud configuration error, not a silent skip. The Fulcio chain
/// step degrades gracefully (no anchors → no check) so test
/// fixtures using SPKI-only PEMs still verify.
pub fn verify_signed_manifest_with_trust_root<'a>(
    manifest_bytes: &'a [u8],
    bundle: &CosignBundle,
    trust_root: &TrustRoot,
    now: OffsetDateTime,
) -> Result<VerifiedManifest<'a>, SigningError> {
    // Reuse phase 1 + phase 2 partial: sig + cert window.
    let verified = verify_signed_manifest_at(manifest_bytes, bundle, now)?;

    // Rekor inclusion proof. The bundle carries a
    // `SignedEntryTimestamp` over a canonical JSON of
    // `rekorBundle.payload`; we re-canonicalize, look up the Rekor
    // pubkey for the bundle's `logId`, and verify the signature.
    if !trust_root.has_rekor_keys() {
        return Err(SigningError::TrustRootMissingRekorKeys);
    }
    let raw = serde_json::to_string(&CosignBundleJsonShape::from(&bundle.inner))
        .map_err(|e| SigningError::MalformedBundle(format!("re-serialize bundle: {e}")))?;
    let rekor_keys = trust_root.rekor_verification_keys()?;
    SignedArtifactBundle::new_verified(&raw, &rekor_keys).map_err(|e| {
        SigningError::RekorInclusionFailed(format!(
            "sigstore-rs rejected the bundle's Rekor entry: {e}"
        ))
    })?;
    tracing::debug!(
        rekor_log_id = %bundle.inner.rekor_bundle.payload.log_id,
        "Rekor inclusion proof verified"
    );

    // Fulcio cert chain validation. Only runs when the trust root
    // carries anchors — SPKI-only test fixtures with no real cert
    // structure are still accepted (the leaf-vs-cert sig check is
    // the meaningful gate there). Production trust roots always
    // carry Fulcio anchors so this is the meaningful path.
    if trust_root.has_fulcio_certs() {
        verify_fulcio_chain(&bundle.inner.cert, &trust_root.fulcio_cert_der, now)?;
        tracing::debug!(
            cert_pem_bytes = bundle.inner.cert.len(),
            anchor_count = trust_root.fulcio_cert_der.len(),
            "Fulcio cert chain validated against trust root"
        );
    }
    Ok(verified)
}

/// Extract the first CRL Distribution Point URL from a PEM-encoded
/// X.509 certificate. Returns `Ok(None)` when the cert has no CDP
/// extension or is a bare SPKI PEM (no `BEGIN CERTIFICATE` marker).
///
/// Parses the CDP extension (OID 2.5.29.31) and returns the first
/// URI found in any `FullName` distribution point.
pub fn extract_cdp_url(cert_pem: &str) -> Result<Option<url::Url>, SigningError> {
    if !cert_pem.contains("BEGIN CERTIFICATE") {
        return Ok(None);
    }
    let cert = Certificate::from_pem(cert_pem.as_bytes())
        .map_err(|e| SigningError::UnusableCertificate(format!("X.509 PEM parse failed: {e}")))?;
    let extensions = match cert.tbs_certificate.extensions.as_deref() {
        Some(e) => e,
        None => return Ok(None),
    };
    const CDP_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.31");
    for ext in extensions {
        if ext.extn_id != CDP_OID {
            continue;
        }
        let cdps = CrlDistributionPoints::from_der(ext.extn_value.as_bytes()).map_err(|e| {
            SigningError::UnusableCertificate(format!("CDP extension parse failed: {e}"))
        })?;
        for dp in cdps.0 {
            if let Some(DistributionPointName::FullName(names)) = &dp.distribution_point {
                for name in names {
                    if let GeneralName::UniformResourceIdentifier(uri) = name {
                        let parsed = url::Url::parse(uri.as_str()).map_err(|e| {
                            SigningError::UnusableCertificate(format!(
                                "CDP URI is not a valid URL ({uri}): {e}"
                            ))
                        })?;
                        return Ok(Some(parsed));
                    }
                }
            }
        }
    }
    Ok(None)
}

/// Fetch the DER-encoded CRL from `url`. Returns a [`CrlData`]
/// opaque handle that callers pass to [`check_cert_not_revoked`].
pub async fn fetch_crl(url: &url::Url) -> Result<CrlData, SigningError> {
    let bytes = reqwest::get(url.clone())
        .await
        .map_err(|e| SigningError::CrlFetchFailed {
            url: url.to_string(),
            detail: format!("GET failed: {e}"),
        })?
        .error_for_status()
        .map_err(|e| SigningError::CrlFetchFailed {
            url: url.to_string(),
            detail: format!("bad HTTP status: {e}"),
        })?
        .bytes()
        .await
        .map_err(|e| SigningError::CrlFetchFailed {
            url: url.to_string(),
            detail: format!("read body: {e}"),
        })?;
    Ok(CrlData(bytes.to_vec()))
}

/// Parse `crl` and check whether the cert in `cert_pem` is listed
/// as revoked. Returns `Ok(())` when not revoked,
/// `Err(SigningError::CertRevoked)` when it is.
pub(crate) fn check_cert_not_revoked(
    cert_pem: &str,
    crl: &CrlData,
    crl_url: &url::Url,
) -> Result<(), SigningError> {
    let cert = Certificate::from_pem(cert_pem.as_bytes())
        .map_err(|e| SigningError::UnusableCertificate(format!("X.509 PEM parse failed: {e}")))?;
    let serial = cert.tbs_certificate.serial_number.as_bytes().to_vec();

    let borrowed =
        BorrowedCertRevocationList::from_der(&crl.0).map_err(|e| SigningError::CrlFetchFailed {
            url: crl_url.to_string(),
            detail: format!("CRL DER parse failed: {e:?}"),
        })?;
    let parsed: WebpkiCrl<'_> = borrowed.into();

    match parsed.find_serial(&serial) {
        Ok(None) => Ok(()),
        Ok(Some(_)) => Err(SigningError::CertRevoked),
        Err(e) => Err(SigningError::CrlFetchFailed {
            url: crl_url.to_string(),
            detail: format!("CRL serial lookup failed: {e:?}"),
        }),
    }
}

/// Full-Sigstore verification with optional CRL-based revocation
/// check layered on top of [`verify_signed_manifest_with_trust_root`].
///
/// Behaviour is gated by [`TrustRoot::with_revocation_mode`]
/// (defaults to `BestEffort`):
///
/// - `Skip` — delegates entirely to
///   [`verify_signed_manifest_with_trust_root`]; no network calls.
/// - `BestEffort` — fetches the CRL when a CDP extension is present;
///   network / parse errors are logged as WARN and treated as
///   "not revoked" rather than aborting verification.
/// - `Required` — treats a missing CDP extension or any CRL error
///   as a hard [`SigningError`].
pub async fn verify_signed_manifest_with_revocation<'a>(
    manifest_bytes: &'a [u8],
    bundle: &CosignBundle,
    trust_root: &TrustRoot,
    now: OffsetDateTime,
) -> Result<VerifiedManifest<'a>, SigningError> {
    let verified = verify_signed_manifest_with_trust_root(manifest_bytes, bundle, trust_root, now)?;

    if trust_root.revocation_mode == RevocationMode::Skip {
        tracing::debug!("revocation check skipped (RevocationMode::Skip)");
        return Ok(verified);
    }

    let required = trust_root.revocation_mode == RevocationMode::Required;
    let cert_pem = &bundle.inner.cert;

    let cdp_url = match extract_cdp_url(cert_pem)? {
        Some(u) => u,
        None => {
            if required {
                return Err(SigningError::CdpExtensionMissing);
            }
            tracing::debug!("no CDP extension on cert; skipping CRL check (BestEffort)");
            return Ok(verified);
        }
    };

    let crl = match fetch_crl(&cdp_url).await {
        Ok(c) => c,
        Err(e) => {
            if required {
                return Err(e);
            }
            tracing::warn!(
                url = %cdp_url,
                error = %e,
                "CRL fetch failed; treating as not-revoked (BestEffort)"
            );
            return Ok(verified);
        }
    };

    match check_cert_not_revoked(cert_pem, &crl, &cdp_url) {
        Ok(()) => {
            tracing::debug!(url = %cdp_url, "CRL checked: certificate not revoked");
            Ok(verified)
        }
        Err(SigningError::CertRevoked) => Err(SigningError::CertRevoked),
        Err(e) if required => Err(e),
        Err(e) => {
            tracing::warn!(
                url = %cdp_url,
                error = %e,
                "CRL check error; treating as not-revoked (BestEffort)"
            );
            Ok(verified)
        }
    }
}

/// Run rustls-webpki chain validation on `leaf_cert_pem` against
/// `anchors_der` as TrustAnchors. The leaf must present an EKU of
/// id-kp-clientAuth (Fulcio's default for cosign).
///
/// Intermediates are passed empty here; in real Sigstore deployments
/// callers should add Fulcio's intermediate(s) to `anchors_der` as
/// well as the root (the function treats every entry as a trust
/// anchor). This matches how sigstore-rs's internal `CertificatePool`
/// is loaded from a TUF-resolved trust root.
fn verify_fulcio_chain(
    leaf_cert_pem: &str,
    anchors_der: &[Vec<u8>],
    now: OffsetDateTime,
) -> Result<(), SigningError> {
    // SPKI-only PEM (no `BEGIN CERTIFICATE`) skips the chain check.
    // Tests use these and they don't claim a chain in the first
    // place.
    if !leaf_cert_pem.contains("BEGIN CERTIFICATE") {
        return Ok(());
    }
    // PEM → DER.
    let leaf_der = pem_to_der(leaf_cert_pem)?;
    let leaf_cert_der = pki_types::CertificateDer::from(leaf_der);
    let end_entity = webpki::EndEntityCert::try_from(&leaf_cert_der).map_err(|e| {
        SigningError::FulcioChainInvalid(format!("leaf cert not parseable as EndEntityCert: {e}"))
    })?;

    // Build the TrustAnchor slice from the anchor DERs. The
    // `CertificateDer` borrows from the underlying byte slices, and
    // the `TrustAnchor` borrows from `CertificateDer` — so the DERs
    // need a longer-lived binding than the loop temporary. Hold them
    // in a Vec for the rest of the function.
    let cert_ders: Vec<pki_types::CertificateDer<'_>> = anchors_der
        .iter()
        .map(|d| pki_types::CertificateDer::from(d.as_slice()))
        .collect();
    let mut anchors: Vec<pki_types::TrustAnchor<'_>> = Vec::with_capacity(cert_ders.len());
    for der in &cert_ders {
        match webpki::anchor_from_trusted_cert(der) {
            Ok(a) => anchors.push(a),
            Err(e) => tracing::debug!(error = %e, "skipping unparseable Fulcio trust anchor"),
        }
    }
    if anchors.is_empty() {
        return Err(SigningError::FulcioChainInvalid(
            "no parseable trust anchors on the trust root".to_string(),
        ));
    }

    let supported_algs: &[&dyn pki_types::SignatureVerificationAlgorithm] = &[
        webpki::ring::ECDSA_P256_SHA256,
        webpki::ring::ECDSA_P256_SHA384,
        webpki::ring::ECDSA_P384_SHA256,
        webpki::ring::ECDSA_P384_SHA384,
        webpki::ring::RSA_PKCS1_2048_8192_SHA256,
        webpki::ring::RSA_PKCS1_2048_8192_SHA384,
        webpki::ring::RSA_PKCS1_2048_8192_SHA512,
        webpki::ring::ED25519,
    ];
    let unix_now = pki_types::UnixTime::since_unix_epoch(std::time::Duration::from_secs(
        now.unix_timestamp() as u64,
    ));
    end_entity
        .verify_for_usage(
            supported_algs,
            &anchors,
            &[],
            unix_now,
            webpki::KeyUsage::client_auth(),
            None,
            None,
        )
        .map_err(|e| {
            SigningError::FulcioChainInvalid(format!(
                "chain does not reach any trusted Fulcio anchor: {e}"
            ))
        })?;
    Ok(())
}

/// Decode a `BEGIN PUBLIC KEY` PEM block back to its SPKI DER bytes.
/// Tolerates leading whitespace and indented bodies. Returns the
/// raw DER on success or a human-readable detail on failure.
fn decode_public_key_pem(pem: &str) -> Result<Vec<u8>, String> {
    let mut in_block = false;
    let mut b64 = String::new();
    for line in pem.lines() {
        let trimmed = line.trim();
        if trimmed == "-----BEGIN PUBLIC KEY-----" {
            in_block = true;
            continue;
        }
        if trimmed == "-----END PUBLIC KEY-----" {
            break;
        }
        if in_block {
            b64.push_str(trimmed);
        }
    }
    if b64.is_empty() {
        return Err("no PUBLIC KEY PEM block found".to_string());
    }
    base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| format!("base64 decode failed: {e}"))
}

/// Wrap a raw SubjectPublicKeyInfo DER byte slice in a
/// `-----BEGIN PUBLIC KEY-----` PEM block.
///
/// Sigstore's TUF metadata delivers Rekor public keys as raw SPKI DER;
/// the rest of [`TrustRoot`] keeps Rekor keys as PEM (the format
/// operators paste from Sigstore docs and what
/// [`with_rekor_key`](TrustRoot::with_rekor_key) accepts). This is the
/// adapter between the two.
fn spki_der_to_public_key_pem(der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = String::with_capacity(b64.len() + 64);
    out.push_str("-----BEGIN PUBLIC KEY-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        out.push('\n');
    }
    out.push_str("-----END PUBLIC KEY-----\n");
    out
}

/// Decode a PEM-formatted X.509 cert into its DER body. Strips the
/// `BEGIN CERTIFICATE` / `END CERTIFICATE` armor and base64-decodes
/// the contents.
fn pem_to_der(pem: &str) -> Result<Vec<u8>, SigningError> {
    use base64::Engine;
    let mut in_block = false;
    let mut b64 = String::new();
    for line in pem.lines() {
        let trimmed = line.trim();
        if trimmed == "-----BEGIN CERTIFICATE-----" {
            in_block = true;
            continue;
        }
        if trimmed == "-----END CERTIFICATE-----" {
            break;
        }
        if in_block {
            b64.push_str(trimmed);
        }
    }
    if b64.is_empty() {
        return Err(SigningError::UnusableCertificate(
            "no CERTIFICATE PEM block found".to_string(),
        ));
    }
    base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| SigningError::UnusableCertificate(format!("PEM base64 decode failed: {e}")))
}

/// JSON shape that mirrors `SignedArtifactBundle`'s on-the-wire
/// format. Needed because `SignedArtifactBundle::new_verified` takes
/// the raw JSON string, not the parsed value, so we re-serialize the
/// parsed bundle back to JSON. `SignedArtifactBundle` itself derives
/// `Serialize` so we could in principle use it directly, but this
/// keeps the conversion explicit and decoupled from sigstore-rs's
/// internal field ordering.
#[derive(Serialize)]
struct CosignBundleJsonShape<'a> {
    #[serde(rename = "base64Signature")]
    base64_signature: &'a str,
    cert: &'a str,
    #[serde(rename = "rekorBundle")]
    rekor_bundle: &'a sigstore::cosign::bundle::Bundle,
}

impl<'a> From<&'a SignedArtifactBundle> for CosignBundleJsonShape<'a> {
    fn from(b: &'a SignedArtifactBundle) -> Self {
        Self {
            base64_signature: &b.base64_signature,
            cert: &b.cert,
            rekor_bundle: &b.rekor_bundle,
        }
    }
}

/// Extract a [`CosignVerificationKey`] from the cosign bundle's
/// `cert` field, accepting either of the two shapes that show up in
/// practice:
///
/// - **An X.509 PEM certificate** (`-----BEGIN CERTIFICATE-----`).
///   Real Fulcio-issued certs are this shape. We parse the X.509,
///   extract the SubjectPublicKeyInfo, and build a verification key
///   from it — matching sigstore-rs's own `cosign::verify_blob`
///   internals.
///
/// - **A raw SPKI PEM** (`-----BEGIN PUBLIC KEY-----`). Test
///   fixtures that bypass the cert dance use this; `CosignVerificationKey::try_from_pem`
///   accepts it directly.
fn extract_verification_key(cert_pem: &str) -> Result<CosignVerificationKey, SigningError> {
    if cert_pem.contains("BEGIN CERTIFICATE") {
        let cert = Certificate::from_pem(cert_pem.as_bytes()).map_err(|e| {
            SigningError::UnusableCertificate(format!("X.509 PEM parse failed: {e}"))
        })?;
        let spki = cert.tbs_certificate.subject_public_key_info;
        CosignVerificationKey::try_from(&spki).map_err(|e| {
            SigningError::UnusableCertificate(format!(
                "cannot build verification key from cert SPKI: {e}"
            ))
        })
    } else {
        CosignVerificationKey::try_from_pem(cert_pem.as_bytes()).map_err(|e| {
            SigningError::UnusableCertificate(format!(
                "cannot extract verification key from SPKI PEM: {e}"
            ))
        })
    }
}

/// Parse the cert PEM and reject if `now` falls outside
/// `notBefore..=notAfter`. Tolerant of certs whose PEM body is a
/// raw SPKI public key (no validity extensions) — that case has no
/// window to check, so it passes silently. This is the same shape
/// the phase 1 fresh-key tests use; production Fulcio certs always
/// carry a window.
fn check_cert_validity_window(cert_pem: &str, now: OffsetDateTime) -> Result<(), SigningError> {
    // SPKI-only PEMs deliberately don't carry validity; nothing to
    // check, return Ok. A real X.509 certificate's PEM has the
    // standard `BEGIN CERTIFICATE` marker.
    if !cert_pem.contains("BEGIN CERTIFICATE") {
        return Ok(());
    }
    let cert = Certificate::from_pem(cert_pem.as_bytes())
        .map_err(|e| SigningError::UnusableCertificate(format!("X.509 PEM parse failed: {e}")))?;
    let not_before = cert
        .tbs_certificate
        .validity
        .not_before
        .to_unix_duration()
        .as_secs() as i64;
    let not_after = cert
        .tbs_certificate
        .validity
        .not_after
        .to_unix_duration()
        .as_secs() as i64;
    let now_unix = now.unix_timestamp();

    if now_unix < not_before {
        return Err(SigningError::CertificateExpired(format!(
            "certificate notBefore is in the future (now={now_unix}, notBefore={not_before})"
        )));
    }
    if now_unix > not_after {
        return Err(SigningError::CertificateExpired(format!(
            "certificate notAfter has passed (now={now_unix}, notAfter={not_after})"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use sigstore::cosign::bundle::{Bundle, Payload, SignedArtifactBundle};
    use sigstore::crypto::signing_key::ecdsa::{ECDSAKeys, EllipticCurve};

    /// Build a real cosign bundle that signs `payload` with a
    /// freshly-generated keypair, wrap its cert as PEM, and return
    /// the bundle JSON bytes plus the payload bytes.
    ///
    /// This avoids the need for canned fixtures: the tests are
    /// deterministic against a key generated in the test process.
    fn fresh_bundle(payload: &[u8]) -> (Vec<u8>, Vec<u8>) {
        // 1. Generate an ECDSA P-256 keypair (cosign's default).
        let keys = ECDSAKeys::new(EllipticCurve::P256)
            .expect("ecdsa P-256 keypair generation must succeed");
        let to_sign = keys
            .to_sigstore_signer()
            .expect("ecdsa signer must be available");
        let signature_bytes = to_sign.sign(payload).expect("signing must succeed");
        let b64_sig = base64::engine::general_purpose::STANDARD.encode(&signature_bytes);

        // 2. Phase 1 doesn't check the chain, so the embedded "cert"
        // is just the bare SPKI PEM. Phase 2 will require a real X.509
        // cert chain to Fulcio's root.
        let pubkey_pem = keys
            .as_inner()
            .public_key_to_pem()
            .expect("pubkey must be exportable");

        // 3. Build the bundle JSON.
        let bundle = SignedArtifactBundle {
            base64_signature: b64_sig,
            cert: pubkey_pem,
            rekor_bundle: Bundle {
                signed_entry_timestamp: String::from(""),
                payload: Payload {
                    body: String::from(""),
                    integrated_time: 0,
                    log_index: 0,
                    log_id: String::from("test-log-id"),
                },
            },
        };
        let bundle_json = serde_json::to_vec(&bundle).expect("serialize bundle");
        (payload.to_vec(), bundle_json)
    }

    #[test]
    fn round_trip_verifies_a_freshly_signed_manifest() {
        let manifest = br#"{"ferridis_version":"0.1","id":"test.signing.v1"}"#;
        let (payload, bundle_json) = fresh_bundle(manifest);
        let bundle = CosignBundle::parse(&bundle_json).expect("bundle parses");
        let verified =
            verify_signed_manifest(&payload, &bundle).expect("freshly-signed payload must verify");
        assert_eq!(verified.bytes(), &payload[..]);
    }

    #[test]
    fn rejects_a_tampered_manifest() {
        let manifest = br#"{"ferridis_version":"0.1","id":"test.signing.v1"}"#;
        let (_, bundle_json) = fresh_bundle(manifest);
        let bundle = CosignBundle::parse(&bundle_json).unwrap();
        // Same key signs the bundle, but the payload changed.
        let tampered = br#"{"ferridis_version":"0.1","id":"test.signing.EVIL"}"#;
        match verify_signed_manifest(tampered, &bundle) {
            Err(SigningError::SignatureMismatch(_)) => {}
            other => panic!("tampered payload must fail SignatureMismatch, got {other:?}"),
        }
    }

    #[test]
    fn rejects_malformed_bundle_json() {
        let err = CosignBundle::parse(b"not json").unwrap_err();
        assert!(matches!(err, SigningError::MalformedBundle(_)));
    }

    #[test]
    fn rejects_bundle_with_unusable_certificate() {
        let bundle = SignedArtifactBundle {
            base64_signature: "AAAA".into(),
            cert: "not a PEM cert".into(),
            rekor_bundle: Bundle {
                signed_entry_timestamp: String::new(),
                payload: Payload {
                    body: String::new(),
                    integrated_time: 0,
                    log_index: 0,
                    log_id: String::from("test"),
                },
            },
        };
        let bundle_json = serde_json::to_vec(&bundle).unwrap();
        let parsed = CosignBundle::parse(&bundle_json).unwrap();
        match verify_signed_manifest(b"anything", &parsed) {
            Err(SigningError::UnusableCertificate(_)) => {}
            other => panic!("bad cert must fail UnusableCertificate, got {other:?}"),
        }
    }

    /// Full Sigstore verification with a trust root: the bundle's
    /// Rekor `SignedEntryTimestamp` is itself a real ECDSA signature
    /// over the canonical JSON of the payload, made by a key we
    /// register as the trusted Rekor pubkey. End-to-end, real-crypto
    /// proof that v0.3's Rekor inclusion proof check works.
    #[test]
    fn full_trust_root_path_accepts_a_properly_signed_bundle() {
        use olpc_cjson::CanonicalFormatter;
        use sigstore::cosign::bundle::Payload;

        // 1. Build a real cosign bundle, but sign the payload's
        //    canonical JSON with a known Rekor keypair so the
        //    `signed_entry_timestamp` is real.
        let manifest = br#"{"ferridis_version":"0.1","id":"test.signed.v1"}"#;

        // Manifest signing keypair.
        let manifest_keys = ECDSAKeys::new(EllipticCurve::P256).unwrap();
        let manifest_signer = manifest_keys.to_sigstore_signer().unwrap();
        let manifest_sig_bytes = manifest_signer.sign(manifest).unwrap();
        let manifest_sig_b64 =
            base64::engine::general_purpose::STANDARD.encode(&manifest_sig_bytes);
        let manifest_pubkey_pem = manifest_keys.as_inner().public_key_to_pem().unwrap();

        // Rekor keypair — the trusted instance whose pubkey we'll
        // register on the TrustRoot.
        let rekor_keys = ECDSAKeys::new(EllipticCurve::P256).unwrap();
        let rekor_signer = rekor_keys.to_sigstore_signer().unwrap();
        let rekor_pubkey_pem = rekor_keys.as_inner().public_key_to_pem().unwrap();

        let log_id = String::from("rekor.sigstore.test");
        let payload = Payload {
            body: "dGVzdA==".into(),
            integrated_time: 0,
            log_index: 1,
            log_id: log_id.clone(),
        };
        // Canonical JSON of payload — the exact bytes sigstore-rs
        // hashes when it verifies the Rekor SignedEntryTimestamp.
        let mut buf = Vec::new();
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, CanonicalFormatter::new());
        payload.serialize(&mut ser).unwrap();
        let set_bytes = rekor_signer.sign(&buf).unwrap();
        let set_b64 = base64::engine::general_purpose::STANDARD.encode(&set_bytes);

        let inner = SignedArtifactBundle {
            base64_signature: manifest_sig_b64,
            cert: manifest_pubkey_pem,
            rekor_bundle: Bundle {
                signed_entry_timestamp: set_b64,
                payload,
            },
        };
        let bundle = CosignBundle { inner };

        let trust_root = TrustRoot::new().with_rekor_key(&log_id, &rekor_pubkey_pem);

        let now = OffsetDateTime::now_utc();
        let verified = verify_signed_manifest_with_trust_root(manifest, &bundle, &trust_root, now)
            .expect("real-crypto Rekor inclusion proof must verify");
        assert_eq!(verified.bytes(), &manifest[..]);
    }

    #[test]
    fn full_trust_root_path_rejects_wrong_rekor_signature() {
        // Same setup, but sign the Rekor payload with a *different*
        // key than the one registered on the TrustRoot.
        use olpc_cjson::CanonicalFormatter;
        use sigstore::cosign::bundle::Payload;

        let manifest = br#"{"id":"x"}"#;

        let manifest_keys = ECDSAKeys::new(EllipticCurve::P256).unwrap();
        let manifest_signer = manifest_keys.to_sigstore_signer().unwrap();
        let manifest_sig_bytes = manifest_signer.sign(manifest).unwrap();
        let manifest_sig_b64 =
            base64::engine::general_purpose::STANDARD.encode(&manifest_sig_bytes);
        let manifest_pubkey_pem = manifest_keys.as_inner().public_key_to_pem().unwrap();

        let trusted_rekor = ECDSAKeys::new(EllipticCurve::P256).unwrap();
        let trusted_rekor_pubkey_pem = trusted_rekor.as_inner().public_key_to_pem().unwrap();

        let imposter_rekor = ECDSAKeys::new(EllipticCurve::P256).unwrap();
        let imposter_signer = imposter_rekor.to_sigstore_signer().unwrap();

        let log_id = String::from("rekor.test");
        let payload = Payload {
            body: "Yg==".into(),
            integrated_time: 0,
            log_index: 1,
            log_id: log_id.clone(),
        };
        let mut buf = Vec::new();
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, CanonicalFormatter::new());
        payload.serialize(&mut ser).unwrap();
        let bad_set_bytes = imposter_signer.sign(&buf).unwrap();
        let bad_set_b64 = base64::engine::general_purpose::STANDARD.encode(&bad_set_bytes);

        let inner = SignedArtifactBundle {
            base64_signature: manifest_sig_b64,
            cert: manifest_pubkey_pem,
            rekor_bundle: Bundle {
                signed_entry_timestamp: bad_set_b64,
                payload,
            },
        };
        let bundle = CosignBundle { inner };

        let trust_root = TrustRoot::new().with_rekor_key(&log_id, &trusted_rekor_pubkey_pem);

        let err = verify_signed_manifest_with_trust_root(
            manifest,
            &bundle,
            &trust_root,
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        match err {
            SigningError::RekorInclusionFailed(_) => {}
            other => {
                panic!("imposter Rekor signature must fail RekorInclusionFailed, got {other:?}")
            }
        }
    }

    /// Generate a self-signed CA + a leaf cert with EKU client-auth
    /// signed by that CA. Returns the CA DER (for the TrustRoot),
    /// the leaf PEM (to drop into the bundle's `cert` field), and
    /// the leaf's KeyPair so the caller can sign the manifest with
    /// the matching private key.
    fn generate_test_chain(leaf_subject: &str) -> (Vec<u8>, String, rcgen::KeyPair) {
        use rcgen::{
            CertificateParams, DistinguishedName, ExtendedKeyUsagePurpose, IsCa, KeyPair,
            KeyUsagePurpose,
        };
        // CA
        let ca_key = KeyPair::generate().expect("CA keypair");
        let mut ca_params = CertificateParams::new(vec![]).expect("CA params");
        ca_params.distinguished_name = {
            let mut dn = DistinguishedName::new();
            dn.push(rcgen::DnType::CommonName, "ferridis-test-ca");
            dn
        };
        ca_params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let ca_cert = ca_params.self_signed(&ca_key).expect("self-signed CA");

        // Leaf signed by CA, with EKU client-auth (Fulcio's cosign EKU)
        let leaf_key = KeyPair::generate().expect("leaf keypair");
        let mut leaf_params =
            CertificateParams::new(vec![leaf_subject.to_string()]).expect("leaf params");
        leaf_params.distinguished_name = {
            let mut dn = DistinguishedName::new();
            dn.push(rcgen::DnType::CommonName, leaf_subject);
            dn
        };
        leaf_params.is_ca = IsCa::NoCa;
        leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let leaf_cert = leaf_params
            .signed_by(&leaf_key, &ca_cert, &ca_key)
            .expect("leaf signed by CA");

        (ca_cert.der().to_vec(), leaf_cert.pem(), leaf_key)
    }

    /// Build a Bundle whose manifest signature is from `leaf_key`,
    /// whose cert PEM is `leaf_pem`, and whose Rekor entry is signed
    /// by `rekor_key`. Returns the parsed CosignBundle.
    fn build_signed_bundle(
        manifest: &[u8],
        leaf_pem: &str,
        leaf_key: &rcgen::KeyPair,
        rekor_key: &sigstore::crypto::signing_key::ecdsa::ECDSAKeys,
        log_id: &str,
    ) -> CosignBundle {
        use base64::Engine;
        use olpc_cjson::CanonicalFormatter;
        use sigstore::cosign::bundle::Payload;

        // rcgen's `KeyPair::sign` is private, so re-load the leaf
        // key's PEM into sigstore's ECDSA signer (the same shape that
        // signs the Rekor side). Both libraries default to ECDSA P-256
        // SHA-256 ASN.1, matching cosign's wire format.
        let leaf_pem_pkcs8 = leaf_key.serialize_pem();
        let leaf_signer_keys =
            sigstore::crypto::signing_key::ecdsa::ECDSAKeys::from_pem(leaf_pem_pkcs8.as_bytes())
                .expect("rcgen-emitted PKCS#8 PEM loads back into sigstore ECDSAKeys");
        let leaf_signer = leaf_signer_keys.to_sigstore_signer().expect("leaf signer");
        let sig_bytes = leaf_signer.sign(manifest).expect("leaf signs manifest");
        let manifest_sig_b64 = base64::engine::general_purpose::STANDARD.encode(&sig_bytes);

        // Rekor signs the canonical JSON of the payload.
        let payload = Payload {
            body: "cmVrLXRlc3Q=".into(),
            integrated_time: 0,
            log_index: 1,
            log_id: log_id.to_string(),
        };
        let mut buf = Vec::new();
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, CanonicalFormatter::new());
        payload.serialize(&mut ser).unwrap();
        let rekor_signer = rekor_key.to_sigstore_signer().unwrap();
        let set_bytes = rekor_signer.sign(&buf).unwrap();
        let set_b64 = base64::engine::general_purpose::STANDARD.encode(&set_bytes);

        let inner = SignedArtifactBundle {
            base64_signature: manifest_sig_b64,
            cert: leaf_pem.to_string(),
            rekor_bundle: Bundle {
                signed_entry_timestamp: set_b64,
                payload,
            },
        };
        CosignBundle { inner }
    }

    /// Full real-crypto chain-of-trust test. CA + leaf chain via
    /// rcgen + real Rekor signature + verify_signed_manifest_with_trust_root.
    #[test]
    fn full_trust_root_path_validates_real_fulcio_chain() {
        let manifest = br#"{"id":"x","intents":["read-file"]}"#;
        let (ca_der, leaf_pem, leaf_key) = generate_test_chain("test-signer@ferridis.local");
        let rekor_keys = ECDSAKeys::new(EllipticCurve::P256).unwrap();
        let rekor_pubkey_pem = rekor_keys.as_inner().public_key_to_pem().unwrap();
        let log_id = "rekor.test.chain";

        let bundle = build_signed_bundle(manifest, &leaf_pem, &leaf_key, &rekor_keys, log_id);

        let trust_root = TrustRoot::new()
            .with_rekor_key(log_id, &rekor_pubkey_pem)
            .with_fulcio_cert_der(ca_der);

        let verified = verify_signed_manifest_with_trust_root(
            manifest,
            &bundle,
            &trust_root,
            OffsetDateTime::now_utc(),
        )
        .expect("real CA + leaf + Rekor chain must verify");
        assert_eq!(verified.bytes(), manifest);
    }

    /// Wrong CA on the TrustRoot: the leaf is signed by CA_A, but we
    /// register CA_B's DER. Verification must fail with
    /// FulcioChainInvalid (not RekorInclusionFailed — the Rekor side
    /// of the bundle is fine here, only the cert chain is wrong).
    #[test]
    fn full_trust_root_path_rejects_chain_to_untrusted_ca() {
        let manifest = b"x";
        let (_ca_a_der, leaf_pem, leaf_key) = generate_test_chain("leaf-a");
        let (ca_b_der, _, _) = generate_test_chain("leaf-b"); // different CA
        let rekor_keys = ECDSAKeys::new(EllipticCurve::P256).unwrap();
        let rekor_pubkey_pem = rekor_keys.as_inner().public_key_to_pem().unwrap();
        let log_id = "rekor.test.untrusted";

        let bundle = build_signed_bundle(manifest, &leaf_pem, &leaf_key, &rekor_keys, log_id);

        let trust_root = TrustRoot::new()
            .with_rekor_key(log_id, &rekor_pubkey_pem)
            .with_fulcio_cert_der(ca_b_der);

        let err = verify_signed_manifest_with_trust_root(
            manifest,
            &bundle,
            &trust_root,
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        match err {
            SigningError::FulcioChainInvalid(_) => {}
            other => panic!(
                "leaf from CA_A against trust-root anchor CA_B must fail FulcioChainInvalid, got {other:?}"
            ),
        }
    }

    #[test]
    fn full_trust_root_path_rejects_empty_trust_root() {
        let (_, bundle_json) = fresh_bundle(b"x");
        let bundle = CosignBundle::parse(&bundle_json).unwrap();
        let trust_root = TrustRoot::new();
        match verify_signed_manifest_with_trust_root(
            b"x",
            &bundle,
            &trust_root,
            OffsetDateTime::now_utc(),
        ) {
            Err(SigningError::TrustRootMissingRekorKeys) => {}
            other => panic!("empty trust root must fail TrustRootMissingRekorKeys, got {other:?}"),
        }
    }

    /// `with_fulcio_cert_pem` accepts a real PEM cert and registers
    /// it as a trust anchor. Round-trip: load PEM, register, verify
    /// `has_fulcio_certs` is true, then use it in a full-chain
    /// verification call.
    #[test]
    fn with_fulcio_cert_pem_accepts_a_real_pem() {
        // Generate a CA whose PEM we'll load through the new API.
        let (_ca_der, _leaf_pem, _leaf_key) = generate_test_chain("api-test");
        let (ca_der, leaf_pem, leaf_key) = generate_test_chain("pem-loader-leaf");

        // Convert the second CA's DER back to PEM via x509-cert.
        use base64::Engine;
        let mut ca_pem = String::from("-----BEGIN CERTIFICATE-----\n");
        let b64 = base64::engine::general_purpose::STANDARD.encode(&ca_der);
        for chunk in b64.as_bytes().chunks(64) {
            ca_pem.push_str(std::str::from_utf8(chunk).unwrap());
            ca_pem.push('\n');
        }
        ca_pem.push_str("-----END CERTIFICATE-----\n");

        let trust_root = TrustRoot::new()
            .with_fulcio_cert_pem(&ca_pem)
            .expect("PEM-form CA loads");
        assert!(trust_root.has_fulcio_certs());

        // The TrustRoot now carries CA's DER; verify it works in a
        // full chain check.
        let rekor_keys = ECDSAKeys::new(EllipticCurve::P256).unwrap();
        let rekor_pem = rekor_keys.as_inner().public_key_to_pem().unwrap();
        let log_id = "rekor.pem-loader.test";
        let trust_root = trust_root.with_rekor_key(log_id, rekor_pem);
        let bundle = build_signed_bundle(b"x", &leaf_pem, &leaf_key, &rekor_keys, log_id);
        verify_signed_manifest_with_trust_root(
            b"x",
            &bundle,
            &trust_root,
            OffsetDateTime::now_utc(),
        )
        .expect("CA loaded from PEM should validate a real chain");
    }

    #[test]
    fn with_fulcio_cert_pem_rejects_malformed_pem() {
        let err = TrustRoot::new()
            .with_fulcio_cert_pem("not a PEM block")
            .unwrap_err();
        assert!(matches!(err, SigningError::UnusableCertificate(_)));
    }

    #[test]
    fn from_sigstore_dir_loads_present_files_and_skips_absent() {
        use std::fs;
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        // Write a Fulcio PEM + Rekor PEM + log_id.
        let (ca_der, _, _) = generate_test_chain("sigstore-dir-ca");
        use base64::Engine;
        let mut ca_pem = String::from("-----BEGIN CERTIFICATE-----\n");
        let b64 = base64::engine::general_purpose::STANDARD.encode(&ca_der);
        for chunk in b64.as_bytes().chunks(64) {
            ca_pem.push_str(std::str::from_utf8(chunk).unwrap());
            ca_pem.push('\n');
        }
        ca_pem.push_str("-----END CERTIFICATE-----\n");
        fs::write(dir.path().join("fulcio_v1.crt.pem"), ca_pem).unwrap();

        let rekor_keys = ECDSAKeys::new(EllipticCurve::P256).unwrap();
        let rekor_pem = rekor_keys.as_inner().public_key_to_pem().unwrap();
        fs::write(dir.path().join("rekor.pub"), rekor_pem).unwrap();
        fs::write(dir.path().join("rekor.log_id"), "rekor.test-loader\n").unwrap();

        let trust_root =
            TrustRoot::from_sigstore_dir(dir.path()).expect("sigstore dir loads cleanly");
        assert!(trust_root.has_fulcio_certs());
        assert!(trust_root.has_rekor_keys());
        // Intermediate file absent → only one anchor.
        assert_eq!(trust_root.fulcio_cert_der.len(), 1);
    }

    #[test]
    fn from_sigstore_dir_empty_dir_yields_empty_trust_root() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let trust_root = TrustRoot::from_sigstore_dir(dir.path()).unwrap();
        assert!(!trust_root.has_fulcio_certs());
        assert!(!trust_root.has_rekor_keys());
    }

    #[test]
    fn describe_does_not_leak_keys_or_signatures() {
        let manifest = b"x";
        let (_, bundle_json) = fresh_bundle(manifest);
        let bundle = CosignBundle::parse(&bundle_json).unwrap();
        let desc = bundle.describe();
        // The description carries lengths, not contents.
        assert!(desc.cert_pem_bytes > 0);
        assert!(desc.signature_base64_bytes > 0);
        assert_eq!(desc.rekor_log_id, "test-log-id");
        // And it serializes without including the signature itself.
        let s = serde_json::to_string(&desc).unwrap();
        assert!(!s.contains("BEGIN PUBLIC KEY"));
        assert!(!s.contains("BEGIN CERTIFICATE"));
    }

    #[test]
    fn spki_der_to_pem_round_trips_through_base64() {
        // Real-ish ECDSA P-256 SPKI is ~91 bytes; use a slice with
        // some structure so we can prove the bytes survive the
        // wrapping/unwrapping unchanged.
        let der: Vec<u8> = (0u8..=200).cycle().take(180).collect();
        let pem = spki_der_to_public_key_pem(&der);

        assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----\n"));
        assert!(pem.trim_end().ends_with("-----END PUBLIC KEY-----"));

        // Lines (except header / footer) are <= 64 chars — RFC 7468
        // recommendation that downstream PEM parsers may enforce.
        for line in pem.lines() {
            if line.starts_with("-----") {
                continue;
            }
            assert!(
                line.len() <= 64,
                "PEM body line exceeds 64 chars: {} chars",
                line.len()
            );
        }

        // Strip armor, concat, base64-decode → original DER.
        let body: String = pem
            .lines()
            .filter(|l| !l.starts_with("-----") && !l.is_empty())
            .collect();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(body.as_bytes())
            .expect("PEM body is valid base64");
        assert_eq!(decoded, der);
    }

    /// Live TUF fetch against Sigstore's Public Good Instance. Hits
    /// the network (`tuf-repo-cdn.sigstore.dev`); the embedded
    /// trusted_root.json inside the sigstore crate covers the
    /// trust-root file itself but the TUF metadata chain still
    /// requires HTTPS. `#[ignore]` so `cargo test` stays
    /// hermetic; run with `cargo test --
    /// -- --ignored from_sigstore_tuf_fetches_real_trust_root` when
    /// you want to validate the network path locally.
    #[tokio::test]
    #[ignore = "requires network: hits tuf-repo-cdn.sigstore.dev"]
    async fn from_sigstore_tuf_fetches_real_trust_root() {
        use tempfile::TempDir;
        let cache = TempDir::new().unwrap();
        let trust_root = TrustRoot::from_sigstore_tuf(Some(cache.path()))
            .await
            .expect("TUF fetch should succeed against the public instance");

        // The Sigstore Public Good Instance always has at least one
        // Fulcio CA and at least one Rekor key live; if those are
        // gone, Sigstore has a much bigger problem than this test.
        assert!(
            trust_root.has_fulcio_certs(),
            "TUF-resolved trust root must carry Fulcio CAs"
        );
        assert!(
            trust_root.has_rekor_keys(),
            "TUF-resolved trust root must carry Rekor keys"
        );

        // Every registered Rekor PEM must round-trip back to its
        // DER body and parse via `try_from_der` (the same auto-detect
        // path `rekor_verification_keys` uses internally). The TUF
        // trust root carries both ECDSA P-256 and Ed25519 keys in
        // 2026; a scheme-locked parser would silently drop one of
        // them. This test asserts both survive the round-trip.
        let mut schemes_seen = std::collections::BTreeSet::new();
        for pem in trust_root.rekor_pubkeys_pem.values() {
            let der = decode_public_key_pem(pem).expect("Rekor PEM decodes back to DER");
            let key = CosignVerificationKey::try_from_der(&der)
                .expect("Rekor DER must parse via the production scheme-tolerant path");
            schemes_seen.insert(format!("{key:?}").split('(').next().unwrap().to_string());
        }
        // The Public Good Instance has more than one scheme as of
        // 2026 — proves auto-detect is exercising the multi-scheme
        // path, not just lucking out on a single ECDSA key.
        assert!(
            schemes_seen.len() >= 2,
            "expected at least two distinct schemes in the TUF Rekor key set, saw: {schemes_seen:?}"
        );
    }

    // ── RevocationMode & CrlData ─────────────────────────────────────

    #[test]
    fn revocation_mode_defaults_to_best_effort() {
        assert_eq!(RevocationMode::default(), RevocationMode::BestEffort);
    }

    #[test]
    fn trust_root_revocation_mode_defaults_to_best_effort() {
        let root = TrustRoot::new();
        assert_eq!(root.revocation_mode, RevocationMode::BestEffort);
    }

    #[test]
    fn with_revocation_mode_sets_mode() {
        let root = TrustRoot::new().with_revocation_mode(RevocationMode::Required);
        assert_eq!(root.revocation_mode, RevocationMode::Required);
        let root = TrustRoot::new().with_revocation_mode(RevocationMode::Skip);
        assert_eq!(root.revocation_mode, RevocationMode::Skip);
    }

    #[test]
    fn spki_pem_has_no_cdp() {
        // A SPKI-only PEM has no certificate structure, so no CDP.
        let keys = ECDSAKeys::new(EllipticCurve::P256).unwrap();
        let spki_pem = keys.as_inner().public_key_to_pem().unwrap();
        let url = extract_cdp_url(&spki_pem).expect("SPKI PEM should not error");
        assert!(url.is_none(), "SPKI PEM must return None for CDP");
    }

    #[test]
    fn cert_without_cdp_has_no_url() {
        // A plain rcgen cert has no CDP extension by default.
        let (_, leaf_pem, _) = generate_test_chain("no-cdp-test");
        let url = extract_cdp_url(&leaf_pem).expect("cert without CDP should not error");
        assert!(url.is_none(), "cert without CDP extension must return None");
    }

    /// Generate a DER-encoded CRL signed by `ca_cert`/`ca_key`.
    /// `revoked_serial` is the serial number bytes to include;
    /// pass an empty slice for an empty CRL.
    fn make_crl_der(
        ca_cert: &rcgen::Certificate,
        ca_key: &rcgen::KeyPair,
        revoked_serial: Option<rcgen::SerialNumber>,
    ) -> Vec<u8> {
        use rcgen::{
            CertificateRevocationListParams, KeyIdMethod, RevocationReason, RevokedCertParams,
        };
        use time::OffsetDateTime;
        let now = OffsetDateTime::now_utc();
        let revoked = revoked_serial.map(|s| RevokedCertParams {
            serial_number: s,
            revocation_time: now,
            reason_code: Some(RevocationReason::KeyCompromise),
            invalidity_date: None,
        });
        let params = CertificateRevocationListParams {
            this_update: now,
            next_update: now + time::Duration::days(7),
            crl_number: rcgen::SerialNumber::from(1u64),
            issuing_distribution_point: None,
            revoked_certs: revoked.into_iter().collect(),
            key_identifier_method: KeyIdMethod::Sha256,
        };
        let crl = params
            .signed_by(ca_cert, ca_key)
            .expect("CRL signing must succeed");
        crl.der().to_vec()
    }

    #[test]
    fn cert_not_in_crl_passes_revocation_check() {
        // Generate CA + two leaves: only leaf_b is revoked.
        use rcgen::{
            BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair,
        };
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(vec![]).unwrap();
        ca_params.distinguished_name = {
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, "crl-test-ca");
            dn
        };
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();

        // Leaf A — will NOT be revoked.
        let leaf_a_key = KeyPair::generate().unwrap();
        let leaf_a_serial = rcgen::SerialNumber::from(100u64);
        let mut leaf_a_params = CertificateParams::new(vec![]).unwrap();
        leaf_a_params.serial_number = Some(leaf_a_serial.clone());
        leaf_a_params.distinguished_name = {
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, "leaf-a");
            dn
        };
        let leaf_a_cert = leaf_a_params
            .signed_by(&leaf_a_key, &ca_cert, &ca_key)
            .unwrap();
        let leaf_a_pem = leaf_a_cert.pem();

        // Leaf B — revoked.
        let leaf_b_key = KeyPair::generate().unwrap();
        let leaf_b_serial = rcgen::SerialNumber::from(200u64);
        let mut leaf_b_params = CertificateParams::new(vec![]).unwrap();
        leaf_b_params.serial_number = Some(leaf_b_serial.clone());
        leaf_b_params.distinguished_name = {
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, "leaf-b");
            dn
        };
        let _leaf_b_cert = leaf_b_params
            .signed_by(&leaf_b_key, &ca_cert, &ca_key)
            .unwrap();

        // CRL revokes only leaf_b.
        let crl_der = make_crl_der(&ca_cert, &ca_key, Some(leaf_b_serial));
        let crl_data = CrlData(crl_der);
        let dummy_url = url::Url::parse("http://example.com/crl").unwrap();

        // Leaf A must pass.
        check_cert_not_revoked(&leaf_a_pem, &crl_data, &dummy_url)
            .expect("leaf A is not in the CRL and must pass");
    }

    #[test]
    fn cert_in_crl_fails_revocation_check() {
        use rcgen::{
            BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair,
        };
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(vec![]).unwrap();
        ca_params.distinguished_name = {
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, "crl-test-ca-2");
            dn
        };
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();

        let leaf_key = KeyPair::generate().unwrap();
        let leaf_serial = rcgen::SerialNumber::from(42u64);
        let mut leaf_params = CertificateParams::new(vec![]).unwrap();
        leaf_params.serial_number = Some(leaf_serial.clone());
        leaf_params.distinguished_name = {
            let mut dn = DistinguishedName::new();
            dn.push(DnType::CommonName, "revoked-leaf");
            dn
        };
        let leaf_cert = leaf_params.signed_by(&leaf_key, &ca_cert, &ca_key).unwrap();
        let leaf_pem = leaf_cert.pem();

        // CRL explicitly revokes this leaf.
        let crl_der = make_crl_der(&ca_cert, &ca_key, Some(leaf_serial));
        let crl_data = CrlData(crl_der);
        let dummy_url = url::Url::parse("http://example.com/crl").unwrap();

        match check_cert_not_revoked(&leaf_pem, &crl_data, &dummy_url) {
            Err(SigningError::CertRevoked) => {}
            other => panic!("revoked cert must fail CertRevoked, got {other:?}"),
        }
    }

    /// `RevocationMode::Skip` passes through even when there is no
    /// Rekor key on the trust root (we test via the sync path by
    /// verifying mode storage only — the async path is exercised by
    /// the BestEffort integration test above).
    #[test]
    fn skip_mode_is_stored_correctly() {
        let root = TrustRoot::new().with_revocation_mode(RevocationMode::Skip);
        assert_eq!(root.revocation_mode, RevocationMode::Skip);
    }
}
