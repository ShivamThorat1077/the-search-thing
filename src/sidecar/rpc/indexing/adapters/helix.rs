//! HelixDB adapter — rewritten for the enterprise-dev dynamic query API.
//!
//! All six original named queries (CreateAsset, GetAssetByHash,
//! GetAssetEmbeddingsByHash, CreateAssetEmbeddingByHash,
//! SearchAssetEmbeddings, ClearSearchIndex) are implemented as
//! `POST /v1/query` dynamic queries using the `helix_db` SDK DSL.
//!
//! The public trait impls (TextIndexStore, ImageIndexStore, VideoIndexStore)
//! are unchanged — no other files need to be modified.


use async_trait::async_trait;
use helix_db::{
    g, read_batch, write_batch, Client as HelixClient, DynamicQueryRequest, NodeRef,
    SourcePredicate,
};
use serde_json::Value;
use std::env;
use std::sync::Mutex;


use crate::sidecar::rpc::indexing::adapters::store::{
    ExistingFileRecord, ExistingImageRecord, ExistingVideoRecord, ImageIndexStore, TextIndexStore,
    VideoIndexStore,
};
use crate::sidecar::rpc::indexing::adapters::voyage::{EmbeddingClient, VoyageClient};

// ---------------------------------------------------------------------------
// Store struct
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct HelixTextStore {
    endpoint: String,
    voyage: Mutex<Option<VoyageClient>>,
}


impl HelixTextStore {
    pub fn from_env() -> Result<Self, String> {
        let host = env::var("HELIX_ENDPOINT").unwrap_or_else(|_| "http://localhost".to_string());
        let port = env::var("HELIX_PORT")
            .unwrap_or_else(|_| "6969".to_string())
            .parse::<u16>()
            .map_err(|e| format!("invalid HELIX_PORT: {}", e))?;
        // Normalise to "http://host:port" — Client::new appends /v1/query itself.
        let endpoint = format!("{}:{}", host.trim_end_matches('/'), port);
        Ok(Self {
            endpoint,
            voyage: Mutex::new(None),
        })
    }

    fn client(&self) -> Result<HelixClient, String> {
        let api_key = env::var("HELIX_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty());
        HelixClient::new(Some(&self.endpoint))
            .map(|c| c.with_api_key(api_key.as_deref()))
            .map_err(|e| e.to_string())
    }

    // -----------------------------------------------------------------------
    // Index bootstrap (idempotent — safe to call on every container start)
    // -----------------------------------------------------------------------

    /// Creates all required indexes if they don't already exist.
    /// Must be called once at startup before any indexing begins.
    /// In-memory storage wipes on container restart so indexes must be
    /// recreated each time alongside data.
    pub async fn ensure_indexes(&self) -> Result<(), String> {
        let req = DynamicQueryRequest::write(
            write_batch()
                .var_as(
                    "idx_hash",
                    g().create_index_if_not_exists(
                        helix_db::IndexSpec::node_equality("Asset", "content_hash"),
                    ),
                )
                .var_as(
                    "idx_vec",
                    g().create_vector_index_nodes("AssetEmbedding", "vector", None::<&str>),
                )
                .returning(["idx_hash", "idx_vec"]),
        );
        self.exec(req).await.map(|_| ())
    }

    /// Rate-limited query embedding for search — same 21s gap as document indexing.
    pub async fn embed_search_query(&self, query: &str) -> Result<Vec<f32>, String> {
        self.build_document_vector(query).await
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn is_not_found_error(msg: &str) -> bool {
        let low = msg.to_ascii_lowercase();
        low.contains("graph error: no value found")
            || low.contains("\"error\":\"graph error: no value found\"")
    }

    /// Recursively find an `id` or `asset_id` field anywhere in the response.
    fn extract_asset_id(value: &Value) -> Option<String> {
        if let Some(id) = value.get("asset_id").and_then(Value::as_str) {
            return Some(id.to_string());
        }
        if let Some(id) = value.get("id").and_then(Value::as_str) {
            return Some(id.to_string());
        }
        if let Some(arr) = value.as_array() {
            for item in arr {
                if let Some(id) = Self::extract_asset_id(item) {
                    return Some(id);
                }
            }
        }
        if let Some(obj) = value.as_object() {
            for v in obj.values() {
                if let Some(id) = Self::extract_asset_id(v) {
                    return Some(id);
                }
            }
        }
        None
    }

    fn has_video_completion_marker(value: &Value) -> bool {
        if value.is_null() {
            return false;
        }
        if let Some(arr) = value.as_array() {
            return arr.iter().any(Self::has_video_completion_marker);
        }
        if let Some(obj) = value.as_object() {
            if let (Some(uk), Some(uu)) = (
                obj.get("unit_kind").and_then(Value::as_str),
                obj.get("unit_key").and_then(Value::as_str),
            ) {
                if uk == "video_index_state" && uu == "complete" {
                    return true;
                }
            }
            return obj.values().any(Self::has_video_completion_marker);
        }
        false
    }

    fn embedding_unit_exists(value: &Value, unit_kind: &str, unit_key: &str) -> bool {
        if value.is_null() {
            return false;
        }
        if let Some(arr) = value.as_array() {
            return arr
                .iter()
                .any(|v| Self::embedding_unit_exists(v, unit_kind, unit_key));
        }
        if let Some(obj) = value.as_object() {
            if let (Some(k), Some(u)) = (
                obj.get("unit_kind").and_then(Value::as_str),
                obj.get("unit_key").and_then(Value::as_str),
            ) {
                if k == unit_kind && u == unit_key {
                    return true;
                }
            }
            return obj
                .values()
                .any(|v| Self::embedding_unit_exists(v, unit_kind, unit_key));
        }
        false
    }

    // -----------------------------------------------------------------------
    // Voyage vector builder — centralized rate limiter (3 RPM free tier)
    // -----------------------------------------------------------------------

    /// Calls Voyage to embed `content`, enforcing a 21-second minimum gap
    /// between successive calls. All three pipelines (text, image, video)
    /// share this single choke point automatically.
    async fn build_document_vector(&self, content: &str) -> Result<Vec<f32>, String> {
        let voyage = {
            let mut slot = self
                .voyage
                .lock()
                .map_err(|e| format!("voyage client lock poisoned: {}", e))?;
            match slot.as_mut() {
                Some(c) => c.clone(),
                None => {
                    let c = VoyageClient::from_env()?;
                    *slot = Some(c.clone());
                    c
                }
            }
        };

        let vec_f64 = voyage.embed_document(content).await?;
        Ok(vec_f64.into_iter().map(|v| v as f32).collect())
    }

    // -----------------------------------------------------------------------
    // Dynamic query execution helpers
    // -----------------------------------------------------------------------

    /// Execute a dynamic request and return the raw JSON response.
    async fn exec(&self, req: DynamicQueryRequest) -> Result<Value, String> {
        let client = self.client()?;
        client
            .query::<Value>()
            .dynamic(req)
            .send()
            .await
            .map_err(|e| e.to_string())
    }

    /// Execute and treat "no value found" as Ok(Null).
    async fn exec_optional(&self, req: DynamicQueryRequest) -> Result<Value, String> {
        match self.exec(req).await {
            Ok(v) => Ok(v),
            Err(e) if Self::is_not_found_error(&e) => Ok(Value::Null),
            Err(e) => Err(e),
        }
    }

    // -----------------------------------------------------------------------
    // The six logical queries implemented as dynamic queries
    // -----------------------------------------------------------------------

    /// GetAssetByHash — read
    async fn get_asset_by_hash(&self, content_hash: &str) -> Result<Value, String> {
        let req = DynamicQueryRequest::read(
            read_batch()
                .var_as(
                    "asset",
                    g().n_where(SourcePredicate::and(vec![
                        SourcePredicate::eq("$label", "Asset"),
                        SourcePredicate::eq("content_hash", content_hash),
                    ]))
                    .value_map(None::<Vec<&str>>),
                )
                .returning(["asset"]),
        );
        self.exec_optional(req).await
    }

    /// CreateAsset — write (get-or-create pattern).
    /// Note: `ensure_indexes` is NOT called here — it must be called once
    /// at startup via `HelixTextStore::ensure_indexes()`.
    async fn create_asset(
        &self,
        content_hash: &str,
        kind: &str,
        path: &str,
    ) -> Result<Value, String> {
        // Return early if the asset already exists.
        let existing = self.get_asset_by_hash(content_hash).await?;
        if Self::extract_asset_id(&existing).is_some() {
            return Ok(existing);
        }
        let req = DynamicQueryRequest::write(
            write_batch()
                .var_as(
                    "asset",
                    g().add_n(
                        "Asset",
                        vec![
                            ("content_hash", content_hash),
                            ("kind", kind),
                            ("path", path),
                        ],
                    )
                    .value_map(None::<Vec<&str>>),
                )
                .returning(["asset"]),
        );
        self.exec(req).await
    }

    /// GetAssetEmbeddingsByHash — read
    async fn get_asset_embeddings_by_hash(&self, content_hash: &str) -> Result<Value, String> {
        let req = DynamicQueryRequest::read(
            read_batch()
                .var_as(
                    "asset",
                    g().n_where(SourcePredicate::and(vec![
                        SourcePredicate::eq("$label", "Asset"),
                        SourcePredicate::eq("content_hash", content_hash),
                    ])),
                )
                .var_as(
                    "embeddings",
                    g().n(NodeRef::var("asset"))
                        .out(Some("HasAssetEmbedding"))
                        .value_map(None::<Vec<&str>>),
                )
                .returning(["embeddings"]),
        );
        self.exec_optional(req).await
    }

    /// CreateAssetEmbeddingByHash — write
    async fn create_asset_embedding(
        &self,
        content_hash: &str,
        unit_kind: &str,
        unit_key: &str,
        content: &str,
        vector: Vec<f32>,
    ) -> Result<(), String> {
        let req = DynamicQueryRequest::write(
            write_batch()
                .var_as(
                    "asset",
                    g().n_where(SourcePredicate::and(vec![
                        SourcePredicate::eq("$label", "Asset"),
                        SourcePredicate::eq("content_hash", content_hash),
                    ])),
                )
                .var_as(
                    "embedding",
                    g().add_n(
                        "AssetEmbedding",
                        vec![
                            (
                                "unit_kind",
                                helix_db::PropertyInput::from(unit_kind),
                            ),
                            (
                                "unit_key",
                                helix_db::PropertyInput::from(unit_key),
                            ),
                            (
                                "content",
                                helix_db::PropertyInput::from(content),
                            ),
                            (
                                "vector",
                                helix_db::PropertyInput::from(
                                    helix_db::PropertyValue::F32Array(vector),
                                ),
                            ),
                        ],
                    ),
                )
                .var_as(
                    "edge",
                    g().n(NodeRef::var("asset"))
                        .add_e("HasAssetEmbedding", NodeRef::var("embedding"), vec![
                            ("created_at", helix_db::PropertyInput::from(
                                chrono::Utc::now()
                                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                            )),
                        ]),
                )
                .returning(["embedding"]),
        );
        self.exec(req).await.map(|_| ())
    }

    /// SearchAssetEmbeddings — read (vector similarity, top 50)
    pub async fn search_asset_embeddings(&self, query_vector: Vec<f32>) -> Result<Value, String> {
    let req = DynamicQueryRequest::read(
        read_batch()
            .var_as(
                "embeddings",
                g().vector_search_nodes("AssetEmbedding", "vector", query_vector, 50, None)
                    .value_map(None::<Vec<&str>>),
            )
            .returning(["embeddings"]),
    );
    self.exec_optional(req).await
}



    /// Given a list of AssetEmbedding $ids, fetch each one's connected Asset
    /// (content_hash, kind, path). Returns parallel array, one Asset per id,
    /// preserving order and duplicates.
    pub async fn get_assets_for_embedding_ids(&self, ids: Vec<i64>) -> Result<Value, String> {
        if ids.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }
        let mut results = Vec::new();
        for id in ids {
            let req = DynamicQueryRequest::read(
                read_batch()
                    .var_as("emb", g().n_where(SourcePredicate::eq("$id", id)))
                    .var_as(
                        "asset",
                        g().n(NodeRef::var("emb"))
                            .in_(Some("HasAssetEmbedding"))
                            .value_map(None::<Vec<&str>>),
                    )
                    .returning(["asset"]),
            );
            let resp = self.exec_optional(req).await?;
            let asset = resp
                .get("asset")
                .map(|v| v.get("properties").unwrap_or(v))
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
                .cloned()
                .unwrap_or(Value::Null);
            results.push(asset);
        }
        Ok(Value::Array(results))
    }

    /// ClearSearchIndex — write
    pub async fn clear_search_index(&self) -> Result<Value, String> {
        let req = DynamicQueryRequest::write(
            write_batch()
                .var_as(
                    "embeddings",
                    g().n_with_label("Asset")
                        .out(Some("HasAssetEmbedding")),
                )
                .var_as("drop_embeddings", g().n(NodeRef::var("embeddings")).drop())
                .var_as("assets", g().n_with_label("Asset"))
                .var_as("drop_assets", g().n(NodeRef::var("assets")).drop())
                .returning(["drop_assets"]),
        );
        self.exec(req).await
    }

    // -----------------------------------------------------------------------
    // Shared upsert-embedding helper (dedup guard + Voyage call + insert)
    // -----------------------------------------------------------------------

    async fn upsert_asset_embedding(
        &self,
        content_hash: &str,
        unit_kind: &str,
        unit_key: &str,
        content: &str,
    ) -> Result<(), String> {
        // Dedup: skip if this (unit_kind, unit_key) pair already exists.
        let existing = self.get_asset_embeddings_by_hash(content_hash).await?;
        if Self::embedding_unit_exists(&existing, unit_kind, unit_key) {
            return Ok(());
        }

        // Rate-limited Voyage call — shared across all three pipelines.
        let vector = self.build_document_vector(content).await?;
        self.create_asset_embedding(content_hash, unit_kind, unit_key, content, vector)
            .await
    }
}

// ---------------------------------------------------------------------------
// TextIndexStore
// ---------------------------------------------------------------------------

#[async_trait]
impl TextIndexStore for HelixTextStore {
    async fn get_file_by_hash(
        &self,
        content_hash: &str,
    ) -> Result<Option<ExistingFileRecord>, String> {
        let result = self.get_asset_by_hash(content_hash).await?;
        Ok(Self::extract_asset_id(&result).map(|asset_id| ExistingFileRecord { asset_id }))
    }

    async fn create_file_asset(
        &self,
        content_hash: &str,
        kind: &str,
        path: &str,
    ) -> Result<(), String> {
        self.create_asset(content_hash, kind, path).await.map(|_| ())
    }

    async fn create_file_asset_embeddings(
        &self,
        content_hash: &str,
        unit_kind: &str,
        unit_key: &str,
        content: &str,
    ) -> Result<(), String> {
        self.upsert_asset_embedding(content_hash, unit_kind, unit_key, content)
            .await
    }
}

// ---------------------------------------------------------------------------
// ImageIndexStore
// ---------------------------------------------------------------------------

#[async_trait]
impl ImageIndexStore for HelixTextStore {
    async fn get_image_by_hash(
        &self,
        content_hash: &str,
    ) -> Result<Option<ExistingImageRecord>, String> {
        let result = self.get_asset_by_hash(content_hash).await?;
        Ok(Self::extract_asset_id(&result).map(|asset_id| ExistingImageRecord { asset_id }))
    }

    async fn create_image_asset(
        &self,
        content_hash: &str,
        kind: &str,
        path: &str,
    ) -> Result<(), String> {
        self.create_asset(content_hash, kind, path).await.map(|_| ())
    }

    async fn create_image_asset_embeddings(
        &self,
        content_hash: &str,
        unit_kind: &str,
        unit_key: &str,
        content: &str,
    ) -> Result<(), String> {
        self.upsert_asset_embedding(content_hash, unit_kind, unit_key, content)
            .await
    }
}

// ---------------------------------------------------------------------------
// VideoIndexStore
// ---------------------------------------------------------------------------

#[async_trait]
impl VideoIndexStore for HelixTextStore {
    async fn get_video_by_hash(
        &self,
        content_hash: &str,
    ) -> Result<Option<ExistingVideoRecord>, String> {
        let result = self.get_asset_by_hash(content_hash).await?;
        Ok(Self::extract_asset_id(&result).map(|asset_id| ExistingVideoRecord { asset_id }))
    }

    async fn video_asset_has_embeddings(&self, content_hash: &str) -> Result<bool, String> {
        let result = self.get_asset_embeddings_by_hash(content_hash).await?;
        Ok(Self::has_video_completion_marker(&result))
    }

    async fn create_video_asset(
        &self,
        content_hash: &str,
        kind: &str,
        path: &str,
    ) -> Result<(), String> {
        self.create_asset(content_hash, kind, path).await.map(|_| ())
    }

    async fn create_video_asset_embeddings(
        &self,
        content_hash: &str,
        unit_kind: &str,
        unit_key: &str,
        content: &str,
    ) -> Result<(), String> {
        self.upsert_asset_embedding(content_hash, unit_kind, unit_key, content)
            .await
    }
}