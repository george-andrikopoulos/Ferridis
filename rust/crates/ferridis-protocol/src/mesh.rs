//! Public mesh registry — fetch and verify capability manifests
//! published to a central / federated mesh.
//!
//! v0.2 phase 1 implements the **fetch + verify** half of the
//! three-tier discovery model. The mesh hosts signed manifests at a
//! predictable URL; the client pulls both the manifest and its
//! cosign bundle, verifies the bundle through
//! [`crate::signing::verify_signed_manifest`], and hands the result
//! back to the consumer crate which decides where to register it.
//!
//! # URL convention
//!
//! Given a capability reference
//! `ferridis://{registry}/{namespace}/{id}@{version}` and a mesh
//! root URL (e.g. `https://mesh.ferridis.io`), the mesh artifacts
//! live at:
//!
//! ```text
//! {mesh_root}/{registry}/{namespace}/{id}/{version}/manifest.json
//! {mesh_root}/{registry}/{namespace}/{id}/{version}/manifest.json.cosign.bundle
//! ```
//!
//! For `ferridis://public.ferridis.io/google/calendar@v3` against
//! mesh root `https://mesh.ferridis.io`, that's:
//!
//! - `https://mesh.ferridis.io/public.ferridis.io/google/calendar/v3/manifest.json`
//! - `https://mesh.ferridis.io/public.ferridis.io/google/calendar/v3/manifest.json.cosign.bundle`
//!
//! The mesh server is just an HTTP-accessible blob store (S3, Git
//! over CDN, anything that serves bytes by path). It does not need
//! to know about Ferridis semantics — signatures are verified
//! client-side.
//!
//! # v0.3 additions
//!
//! - **Signed mesh-index.** `{mesh_root}/index.json` plus a
//!   companion `.cosign.bundle` enumerates the capabilities the
//!   mesh hosts. See [`MeshIndex`] for the typed payload and
//!   [`MeshClient::fetch_index_and_verify`] for the verified-fetch
//!   path. Enables discovery by intent / category, not just by
//!   exact `CapabilityRef`.
//!
//! - **Federation.** [`FederatedMesh`] wraps an ordered list of
//!   [`MeshClient`]s. `fetch_and_verify` tries each in priority
//!   order, falling through on 404. Lets a host prefer its org's
//!   mesh and fall back to the public one.
//!
//! # Phase 2 (v0.2) recap
//!
//! - **TTL cache.** Repeated `fetch_and_verify` for the same
//!   capability within [`DEFAULT_MESH_CACHE_TTL`] returns the cached
//!   verified bundle bytes without a network round-trip or
//!   re-verification. Override with [`MeshClient::with_cache_ttl`].
//!
//! See [`crate::signing`] for the trust-chain machinery the verified
//! fetch uses.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use ferridis_core::{CapabilityRef, IntentVerb};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::Mutex;
use url::Url;

use crate::client::Client;
use crate::error::ProtocolError;
use crate::signing::{CosignBundle, SigningError, TrustRoot, verify_signed_manifest, verify_signed_manifest_with_trust_root};

/// Default TTL for the mesh client's per-capability cache. Long
/// enough that a cold start that fetches a dozen capabilities does
/// not pay the verification cost twice in a row; short enough that
/// mesh-side revocations propagate within a session.
pub const DEFAULT_MESH_CACHE_TTL: StdDuration = StdDuration::from_secs(300);

/// One signed-artifact pair fetched from a mesh.
///
/// The verification step has not run yet at this stage — callers
/// should pass the bytes through
/// [`crate::signing::verify_signed_manifest`] (or use
/// [`MeshClient::fetch_and_verify`], which does it for you) before
/// trusting the manifest.
#[derive(Debug)]
pub struct MeshArtifact {
    /// The raw manifest body. Pass to
    /// [`ferridis_core::Manifest::parse`] after verification.
    pub manifest_bytes: Vec<u8>,
    /// The cosign bundle that signs the manifest.
    pub bundle: CosignBundle,
}

/// A mesh's signed catalogue of capabilities.
///
/// Published at `{mesh_root}/index.json`, signed with cosign and
/// accompanied by `{mesh_root}/index.json.cosign.bundle`. Lets a
/// client discover capabilities by intent or category without
/// needing to know the exact `CapabilityRef` in advance.
///
/// The index is the mesh's view; per-capability manifests are still
/// the authoritative source for that capability's metadata. The
/// index entries carry just enough to route a discovery query —
/// capability ref, declared intents, and the manifest's category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshIndex {
    /// The wire-format version of the index document. Bumped when
    /// the schema changes incompatibly.
    pub mesh_version: String,
    /// Stable identifier for the mesh as a whole — typically the
    /// public host name (`public.ferridis.io`).
    pub mesh_id: String,
    /// When the mesh signed this index.
    pub issued_at: OffsetDateTime,
    /// After this point the index should be refetched. Clients
    /// should treat a past `expires_at` as a hard cache miss.
    pub expires_at: OffsetDateTime,
    /// One row per published capability.
    pub entries: Vec<MeshIndexEntry>,
}

/// One row in a [`MeshIndex`]. Refers to a single capability the
/// mesh hosts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshIndexEntry {
    /// The capability this row describes.
    pub capability: CapabilityRef,
    /// Intent verbs the capability's manifest declares. Duplicates
    /// the manifest's `intents` field so a discovery query doesn't
    /// have to fetch every manifest. The authoritative copy is on
    /// the manifest itself.
    pub intents: BTreeSet<IntentVerb>,
    /// Capability category (e.g. `"calendar"`, `"messaging"`).
    /// Mirrors `Manifest::category` for the same reason as
    /// [`intents`](Self::intents) above.
    pub category: String,
}

impl MeshIndex {
    /// Capabilities whose entry declares `intent`. Empty if none.
    pub fn capabilities_for_intent(&self, intent: &IntentVerb) -> Vec<CapabilityRef> {
        self.entries
            .iter()
            .filter(|e| e.intents.contains(intent))
            .map(|e| e.capability.clone())
            .collect()
    }

    /// Capabilities whose entry declares `category`. Empty if none.
    pub fn capabilities_in_category(&self, category: &str) -> Vec<CapabilityRef> {
        self.entries
            .iter()
            .filter(|e| e.category == category)
            .map(|e| e.capability.clone())
            .collect()
    }

    /// Whether this index's `expires_at` has passed `now`. Clients
    /// should refetch after this.
    pub fn is_expired(&self, now: OffsetDateTime) -> bool {
        now > self.expires_at
    }

    /// Total number of capabilities advertised.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// HTTP client for fetching capability manifests from a single mesh
/// root.
///
/// Cheap to clone; the underlying [`Client`] and verified-bundle
/// cache are both reference-counted.
#[derive(Debug, Clone)]
pub struct MeshClient {
    http: Client,
    mesh_root: Url,
    cache: Arc<Mutex<HashMap<CapabilityRef, CacheEntry>>>,
    cache_ttl: StdDuration,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    manifest_bytes: Vec<u8>,
    bundle_json: Vec<u8>,
    inserted_at: Instant,
}

impl MeshClient {
    /// Build a mesh client pointed at `mesh_root`. The URL should
    /// have a trailing slash; one is added if missing so
    /// [`Url::join`] appends rather than replaces.
    pub fn new(http: Client, mesh_root: Url) -> Self {
        let mesh_root = with_trailing_slash(mesh_root);
        Self {
            http,
            mesh_root,
            cache: Arc::new(Mutex::new(HashMap::new())),
            cache_ttl: DEFAULT_MESH_CACHE_TTL,
        }
    }

    /// Override the per-capability verified-bundle TTL.
    ///
    /// Set to `Duration::ZERO` to disable caching entirely — useful
    /// for tests that want to exercise the network path twice.
    #[must_use]
    pub fn with_cache_ttl(mut self, ttl: StdDuration) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// The mesh root URL this client points at.
    pub fn mesh_root(&self) -> &Url {
        &self.mesh_root
    }

    /// Fetch the manifest + cosign bundle for `capability` from the
    /// mesh. Does **not** verify the signature; callers must verify
    /// before parsing the manifest body. Use
    /// [`Self::fetch_and_verify`] for the combined flow.
    pub async fn fetch(
        &self,
        capability: &CapabilityRef,
    ) -> Result<MeshArtifact, ProtocolError> {
        let (manifest_bytes, _, bundle) = self.fetch_raw(capability).await?;
        Ok(MeshArtifact {
            manifest_bytes,
            bundle,
        })
    }

    /// Like [`Self::fetch`] but returns the raw bundle bytes
    /// alongside the parsed bundle. The raw bytes are what the
    /// TTL cache stores so a hit can re-instantiate the parsed
    /// bundle without going to the network.
    async fn fetch_raw(
        &self,
        capability: &CapabilityRef,
    ) -> Result<(Vec<u8>, Vec<u8>, CosignBundle), ProtocolError> {
        let manifest_url = self.manifest_url(capability)?;
        let bundle_url = self.bundle_url(capability)?;

        let manifest_bytes = fetch_bytes(&self.http, &manifest_url).await?;
        let bundle_bytes = fetch_bytes(&self.http, &bundle_url).await?;
        let bundle = CosignBundle::parse(&bundle_bytes).map_err(signing_to_protocol)?;

        Ok((manifest_bytes, bundle_bytes, bundle))
    }

    /// Fetch + verify in one step. On success the returned
    /// [`MeshArtifact`]'s `manifest_bytes` have passed the phase 1
    /// signature check + cert validity window; callers can safely
    /// [`ferridis_core::Manifest::parse`] them.
    ///
    /// Cached: a successful fetch+verify for a given capability is
    /// held for [`Self::cache_ttl`]. Cached responses reuse the
    /// stored manifest + bundle bytes and re-verify on each return
    /// (so a clock-skew-revealing cert expiry within the TTL window
    /// still gets caught on the second call). Disable by
    /// constructing with [`Self::with_cache_ttl`]`(Duration::ZERO)`.
    pub async fn fetch_and_verify(
        &self,
        capability: &CapabilityRef,
    ) -> Result<MeshArtifact, ProtocolError> {
        if !self.cache_ttl.is_zero()
            && let Some(cached) = self.lookup_cached(capability).await
        {
            return cached;
        }
        let (manifest_bytes, bundle_json, bundle) = self.fetch_raw(capability).await?;
        verify_signed_manifest(&manifest_bytes, &bundle).map_err(signing_to_protocol)?;
        if !self.cache_ttl.is_zero() {
            self.cache.lock().await.insert(
                capability.clone(),
                CacheEntry {
                    manifest_bytes: manifest_bytes.clone(),
                    bundle_json,
                    inserted_at: Instant::now(),
                },
            );
        }
        Ok(MeshArtifact {
            manifest_bytes,
            bundle,
        })
    }

    async fn lookup_cached(
        &self,
        capability: &CapabilityRef,
    ) -> Option<Result<MeshArtifact, ProtocolError>> {
        let entry = {
            let mut guard = self.cache.lock().await;
            let entry = guard.get(capability)?.clone();
            if entry.inserted_at.elapsed() > self.cache_ttl {
                // Evict and pretend we missed.
                guard.remove(capability);
                return None;
            }
            entry
        };
        let bundle = match CosignBundle::parse(&entry.bundle_json) {
            Ok(b) => b,
            Err(e) => return Some(Err(signing_to_protocol(e))),
        };
        if let Err(e) = verify_signed_manifest(&entry.manifest_bytes, &bundle) {
            return Some(Err(signing_to_protocol(e)));
        }
        Some(Ok(MeshArtifact {
            manifest_bytes: entry.manifest_bytes,
            bundle,
        }))
    }

    /// Fetch + verify the mesh's signed index. Returns the typed
    /// [`MeshIndex`] on success. Verifies the signature through the
    /// full trust chain in [`TrustRoot`] (signature, cert validity
    /// window, Rekor inclusion proof, Fulcio chain), then parses the
    /// body. Rejects expired indexes with [`ProtocolError::Json`] —
    /// operators should refetch from upstream rather than serve a
    /// stale index.
    pub async fn fetch_index_and_verify(
        &self,
        trust_root: &TrustRoot,
    ) -> Result<MeshIndex, ProtocolError> {
        let index_url = self.index_url()?;
        let bundle_url = self.index_bundle_url()?;
        let index_bytes = fetch_bytes(&self.http, &index_url).await?;
        let bundle_bytes = fetch_bytes(&self.http, &bundle_url).await?;
        let bundle = CosignBundle::parse(&bundle_bytes).map_err(signing_to_protocol)?;
        verify_signed_manifest_with_trust_root(
            &index_bytes,
            &bundle,
            trust_root,
            OffsetDateTime::now_utc(),
        )
        .map_err(signing_to_protocol)?;
        let index: MeshIndex = serde_json::from_slice(&index_bytes)
            .map_err(|e| ProtocolError::Json(format!("mesh index parse: {e}")))?;
        if index.is_expired(OffsetDateTime::now_utc()) {
            return Err(ProtocolError::Json(format!(
                "mesh index from {} has expired at {}",
                self.mesh_root, index.expires_at
            )));
        }
        Ok(index)
    }

    /// URL the index body lives at on this mesh.
    pub fn index_url(&self) -> Result<Url, ProtocolError> {
        self.mesh_root
            .join("index.json")
            .map_err(|e| ProtocolError::InvalidUrl(format!("mesh index URL: {e}")))
    }

    /// URL the index's cosign bundle lives at on this mesh.
    pub fn index_bundle_url(&self) -> Result<Url, ProtocolError> {
        self.mesh_root
            .join("index.json.cosign.bundle")
            .map_err(|e| ProtocolError::InvalidUrl(format!("mesh index bundle URL: {e}")))
    }

    /// URL the manifest body lives at on this mesh.
    pub fn manifest_url(&self, capability: &CapabilityRef) -> Result<Url, ProtocolError> {
        let path = mesh_path(capability, "manifest.json");
        self.mesh_root
            .join(&path)
            .map_err(|e| ProtocolError::InvalidUrl(format!("mesh manifest URL: {e}")))
    }

    /// URL the cosign bundle lives at on this mesh.
    pub fn bundle_url(&self, capability: &CapabilityRef) -> Result<Url, ProtocolError> {
        let path = mesh_path(capability, "manifest.json.cosign.bundle");
        self.mesh_root
            .join(&path)
            .map_err(|e| ProtocolError::InvalidUrl(format!("mesh bundle URL: {e}")))
    }
}

fn with_trailing_slash(url: Url) -> Url {
    if url.path().ends_with('/') {
        url
    } else {
        let mut u = url;
        let new_path = format!("{}/", u.path());
        u.set_path(&new_path);
        u
    }
}

fn mesh_path(capability: &CapabilityRef, file: &str) -> String {
    format!(
        "{}/{}/{}/{}/{}",
        capability.registry(),
        capability.namespace(),
        capability.id(),
        capability.version().as_str(),
        file,
    )
}

async fn fetch_bytes(client: &Client, url: &Url) -> Result<Vec<u8>, ProtocolError> {
    let response = client
        .http()
        .get(url.clone())
        .send()
        .await
        .map_err(|source| ProtocolError::Transport {
            url: url.clone(),
            source,
        })?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let body_excerpt = if body.len() > 512 {
            format!("{}\u{2026}(truncated)", &body[..512])
        } else {
            body
        };
        return Err(ProtocolError::BadStatus {
            url: url.clone(),
            status: status.as_u16(),
            body: body_excerpt,
        });
    }
    response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|source| ProtocolError::Transport {
            url: url.clone(),
            source,
        })
}

/// An ordered chain of [`MeshClient`]s — federation.
///
/// `fetch_and_verify` tries each mesh in priority order, falling
/// through on a 404 (`CapabilityNotFound`-style) to the next one.
/// Other errors propagate. Lets a host prefer its org mesh, fall
/// back to a regional mesh, then the public mesh.
///
/// Priority is positional: index 0 is highest. The first mesh that
/// has the capability wins; subsequent meshes are never consulted
/// for that capability.
#[derive(Debug, Clone)]
pub struct FederatedMesh {
    meshes: Vec<MeshClient>,
}

impl FederatedMesh {
    /// Build a federation over `meshes`. The order matters — index 0
    /// is the highest-priority mesh.
    pub fn new(meshes: Vec<MeshClient>) -> Self {
        Self { meshes }
    }

    /// The mesh clients backing this federation, in priority order.
    pub fn meshes(&self) -> &[MeshClient] {
        &self.meshes
    }

    /// Fetch + verify a capability's manifest from the first mesh in
    /// the chain that has it. Falls through on `404 Not Found` /
    /// `CapabilityNotFound`; any other error halts the chain and
    /// propagates.
    ///
    /// Returns [`ProtocolError::CapabilityNotFound`] when every mesh
    /// in the chain returns 404 for this capability.
    pub async fn fetch_and_verify(
        &self,
        capability: &CapabilityRef,
    ) -> Result<MeshArtifact, ProtocolError> {
        let mut last_404_url: Option<Url> = None;
        for mesh in &self.meshes {
            match mesh.fetch_and_verify(capability).await {
                Ok(artifact) => return Ok(artifact),
                Err(ProtocolError::BadStatus { status: 404, url, .. }) => {
                    tracing::debug!(
                        capability = %capability,
                        mesh_root = %mesh.mesh_root(),
                        "capability not on this mesh; trying next federation tier"
                    );
                    last_404_url = Some(url);
                    continue;
                }
                Err(other) => return Err(other),
            }
        }
        Err(ProtocolError::CapabilityNotFound(
            last_404_url.unwrap_or_else(|| {
                Url::parse(&format!("ferridis-federation-empty:{capability}"))
                    .expect("synthetic URL for empty-federation case is well-formed")
            }),
        ))
    }
}

fn signing_to_protocol(e: SigningError) -> ProtocolError {
    // The protocol crate doesn't carry a typed signing variant yet —
    // surfacing as InvalidManifest keeps the error chain together
    // without an API change. v0.3 may add a SigningRejected variant
    // with the SigningError nested.
    ProtocolError::Json(format!("mesh artifact signing failure: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(s: &str) -> CapabilityRef {
        CapabilityRef::parse(s).unwrap()
    }

    #[test]
    fn manifest_url_follows_convention_for_a_root_mesh() {
        let mc = MeshClient::new(
            Client::new(),
            Url::parse("https://mesh.ferridis.io").unwrap(),
        );
        let cap = cap("ferridis://public.ferridis.io/google/calendar@v3");
        let url = mc.manifest_url(&cap).unwrap();
        assert_eq!(
            url.as_str(),
            "https://mesh.ferridis.io/public.ferridis.io/google/calendar/v3/manifest.json"
        );
    }

    #[test]
    fn bundle_url_uses_cosign_bundle_suffix() {
        let mc = MeshClient::new(
            Client::new(),
            Url::parse("https://mesh.ferridis.io").unwrap(),
        );
        let cap = cap("ferridis://public.ferridis.io/google/calendar@v3");
        let url = mc.bundle_url(&cap).unwrap();
        assert!(url.as_str().ends_with("/manifest.json.cosign.bundle"));
    }

    #[test]
    fn nested_namespace_round_trips_through_the_url_path() {
        // `wallet/local/home-server/lights` namespace exercises the
        // multi-segment namespace path.
        let mc = MeshClient::new(
            Client::new(),
            Url::parse("https://mesh.ferridis.io").unwrap(),
        );
        let cap = cap("ferridis://wallet/local/home-server/lights@v1");
        let url = mc.manifest_url(&cap).unwrap();
        assert_eq!(
            url.as_str(),
            "https://mesh.ferridis.io/wallet/local/home-server/lights/v1/manifest.json"
        );
    }

    #[test]
    fn index_url_lives_at_mesh_root() {
        let mc = MeshClient::new(
            Client::new(),
            Url::parse("https://mesh.ferridis.io").unwrap(),
        );
        assert_eq!(
            mc.index_url().unwrap().as_str(),
            "https://mesh.ferridis.io/index.json"
        );
        assert_eq!(
            mc.index_bundle_url().unwrap().as_str(),
            "https://mesh.ferridis.io/index.json.cosign.bundle"
        );
    }

    fn sample_index() -> MeshIndex {
        let cal = cap("ferridis://public.ferridis.io/google/calendar@v3");
        let slack = cap("ferridis://public.ferridis.io/slack/messages@v1");
        let github = cap("ferridis://public.ferridis.io/github/repos@v1");
        let mut cal_intents = BTreeSet::new();
        cal_intents.insert(IntentVerb::parse("list-events").unwrap());
        cal_intents.insert(IntentVerb::parse("create-event").unwrap());
        let mut slack_intents = BTreeSet::new();
        slack_intents.insert(IntentVerb::parse("send-message").unwrap());
        slack_intents.insert(IntentVerb::parse("read-messages").unwrap());
        let mut github_intents = BTreeSet::new();
        github_intents.insert(IntentVerb::parse("search-files").unwrap());
        MeshIndex {
            mesh_version: "0.1".into(),
            mesh_id: "public.ferridis.io".into(),
            issued_at: OffsetDateTime::now_utc() - time::Duration::hours(1),
            expires_at: OffsetDateTime::now_utc() + time::Duration::hours(23),
            entries: vec![
                MeshIndexEntry {
                    capability: cal,
                    intents: cal_intents,
                    category: "calendar".into(),
                },
                MeshIndexEntry {
                    capability: slack,
                    intents: slack_intents,
                    category: "messaging".into(),
                },
                MeshIndexEntry {
                    capability: github,
                    intents: github_intents,
                    category: "files".into(),
                },
            ],
        }
    }

    #[test]
    fn mesh_index_capabilities_for_intent_returns_matching_entries() {
        let idx = sample_index();
        let send = IntentVerb::parse("send-message").unwrap();
        let create = IntentVerb::parse("create-event").unwrap();
        let nope = IntentVerb::parse("delete-event").unwrap();
        let send_caps = idx.capabilities_for_intent(&send);
        assert_eq!(send_caps.len(), 1);
        assert!(send_caps[0].to_string().contains("slack"));
        assert_eq!(idx.capabilities_for_intent(&create).len(), 1);
        assert_eq!(idx.capabilities_for_intent(&nope).len(), 0);
    }

    #[test]
    fn mesh_index_capabilities_in_category_returns_matching_entries() {
        let idx = sample_index();
        assert_eq!(idx.capabilities_in_category("messaging").len(), 1);
        assert_eq!(idx.capabilities_in_category("calendar").len(), 1);
        assert_eq!(idx.capabilities_in_category("payments").len(), 0);
    }

    #[test]
    fn mesh_index_is_expired_compares_against_now() {
        let idx = sample_index();
        let past = idx.expires_at - time::Duration::hours(1);
        let future = idx.expires_at + time::Duration::seconds(1);
        assert!(!idx.is_expired(past));
        assert!(idx.is_expired(future));
    }

    #[test]
    fn mesh_index_round_trips_through_json() {
        let idx = sample_index();
        let json = serde_json::to_string(&idx).unwrap();
        let parsed: MeshIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.mesh_id, idx.mesh_id);
        assert_eq!(parsed.entries.len(), idx.entries.len());
        assert_eq!(parsed.entries[0].category, "calendar");
    }

    #[test]
    fn mesh_root_without_trailing_slash_still_appends_correctly() {
        // Regression guard for `Url::join`'s segment-replacing
        // behaviour: without a trailing slash, the last path segment
        // gets clobbered.
        let mc = MeshClient::new(
            Client::new(),
            Url::parse("https://mesh.ferridis.io/v0").unwrap(),
        );
        let cap = cap("ferridis://public.ferridis.io/x/y@v1");
        let url = mc.manifest_url(&cap).unwrap();
        assert_eq!(
            url.as_str(),
            "https://mesh.ferridis.io/v0/public.ferridis.io/x/y/v1/manifest.json"
        );
    }
}
