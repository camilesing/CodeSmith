//! # Retrieval primitives
//!
//! The LangChain retrieval contracts — `Document` (a chunk of text plus
//! opaque metadata), [`Retriever`] (`BaseRetriever` analog, the query side),
//! [`Embeddings`] (vectorize texts/queries), and [`VectorStore`] (add +
//! similarity search) — with an [`InMemoryVectorStore`] default so a host
//! can run RAG with zero extra dependencies. `#[async_trait]` (not the
//! manual `Pin<Box>` style of `Tool`/`Callback`) because these traits are
//! implemented by *external* crates — the same reason the extension traits
//! chose it.
//!
//! Known limitations (deliberate):
//! - The in-memory store is unbounded and non-persistent; there is no MMR,
//!   reranking, or filtering. A real backend (sqlite-vss, a vector db, an
//!   API) implements [`VectorStore`]/[`Embeddings`] and swaps in — that is
//!   the seam's point.
//! - No `DocumentLoader`/text-splitter contracts: how documents get chunked
//!   is host territory today.
//! - The retriever is the only consumer-facing query surface; similarity
//!   scores are not returned (return `Document`s enriched with a score in
//!   `metadata` if a caller needs them).

use anyhow::Result;
use async_trait::async_trait;

/// A unit of retrieved text: `page_content` plus opaque metadata (the
/// LangChain `Document` shape — a free-form JSON object keeps the contract
/// dependency-free).
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub page_content: String,
    pub metadata: serde_json::Value,
}

impl Document {
    /// A document with no metadata.
    #[must_use]
    pub fn new(page_content: impl Into<String>) -> Self {
        Self {
            page_content: page_content.into(),
            metadata: serde_json::Value::Null,
        }
    }
}

/// Query-side contract: return the `k` most relevant [`Document`]s for
/// `query` (the LangChain `BaseRetriever` analog).
#[async_trait]
pub trait Retriever: Send + Sync {
    async fn retrieve(&self, query: &str, k: usize) -> Result<Vec<Document>>;
}

/// Vectorization contract (the LangChain `Embeddings` analog). Documents and
/// queries may embed differently (symmetric vs asymmetric models), hence the
/// two methods.
#[async_trait]
pub trait Embeddings: Send + Sync {
    async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
}

/// Storage + similarity contract (the LangChain `VectorStore` analog).
/// `add_documents` returns the store-assigned ids in insertion order.
#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn add_documents(
        &mut self,
        documents: Vec<Document>,
        embeddings: &dyn Embeddings,
    ) -> Result<Vec<String>>;
    async fn similarity_search(
        &self,
        query: &str,
        k: usize,
        embeddings: &dyn Embeddings,
    ) -> Result<Vec<Document>>;
}

/// Cosine similarity; a zero vector is treated as orthogonal to everything
/// (score `0.0`) rather than dividing by zero.
#[must_use]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// The default [`VectorStore`]: documents + vectors in a `Vec`, similarity
/// by cosine over a full scan. Fine for thousands of chunks; a real index
/// belongs behind the trait.
#[derive(Debug, Default)]
pub struct InMemoryVectorStore {
    entries: Vec<(String, Vec<f32>, Document)>,
}

impl InMemoryVectorStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored documents.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[async_trait]
impl VectorStore for InMemoryVectorStore {
    async fn add_documents(
        &mut self,
        documents: Vec<Document>,
        embeddings: &dyn Embeddings,
    ) -> Result<Vec<String>> {
        let texts: Vec<String> = documents.iter().map(|d| d.page_content.clone()).collect();
        let vectors = embeddings.embed_documents(&texts).await?;
        if vectors.len() != documents.len() {
            return Err(anyhow::anyhow!(
                "embeddings returned {} vectors for {} documents",
                vectors.len(),
                documents.len()
            ));
        }
        let ids = documents
            .into_iter()
            .zip(vectors)
            .map(|(document, vector)| {
                let id = uuid::Uuid::new_v4().to_string();
                self.entries.push((id.clone(), vector, document));
                id
            })
            .collect();
        Ok(ids)
    }

    async fn similarity_search(
        &self,
        query: &str,
        k: usize,
        embeddings: &dyn Embeddings,
    ) -> Result<Vec<Document>> {
        let query_vector = embeddings.embed_query(query).await?;
        let mut scored: Vec<(f32, &Document)> = self
            .entries
            .iter()
            .map(|(_, vector, document)| (cosine_similarity(&query_vector, vector), document))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored
            .into_iter()
            .take(k)
            .map(|(_, document)| document.clone())
            .collect())
    }
}

/// [`Retriever`] over any shared [`VectorStore`] — the `as_retriever()`
/// analog. Holds the store behind an async `Mutex` (the trait needs `&self`
/// while `add_documents` takes `&mut`) and owns the [`Embeddings`] handle.
pub struct VectorStoreRetriever<S> {
    store: std::sync::Arc<tokio::sync::Mutex<S>>,
    embeddings: std::sync::Arc<dyn Embeddings>,
}

impl<S: VectorStore> VectorStoreRetriever<S> {
    #[must_use]
    pub fn new(
        store: std::sync::Arc<tokio::sync::Mutex<S>>,
        embeddings: std::sync::Arc<dyn Embeddings>,
    ) -> Self {
        Self { store, embeddings }
    }
}

#[async_trait]
impl<S: VectorStore + 'static> Retriever for VectorStoreRetriever<S> {
    async fn retrieve(&self, query: &str, k: usize) -> Result<Vec<Document>> {
        self.store
            .lock()
            .await
            .similarity_search(query, k, self.embeddings.as_ref())
            .await
    }
}

// === Tests ==================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Deterministic test embedder: hashes each word into one of 32
    /// dimensions, so texts sharing words land near each other. Dim 0 is a
    /// constant offset every text carries.
    struct HashEmbeddings;

    #[async_trait]
    impl Embeddings for HashEmbeddings {
        async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            texts.iter().map(|t| Ok(embed(t))).collect()
        }
        async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            Ok(embed(text))
        }
    }

    fn embed(text: &str) -> Vec<f32> {
        let mut v = vec![1.0; 32];
        for word in text.to_lowercase().split_whitespace() {
            let hash = word.bytes().fold(0usize.wrapping_mul(31), |acc, b| {
                acc.wrapping_mul(31).wrapping_add(usize::from(b))
            });
            v[hash % 32] += 1.0;
        }
        v
    }

    #[tokio::test]
    async fn ranks_shared_word_documents_first() {
        let mut store = InMemoryVectorStore::new();
        let docs = vec![
            Document::new("rust language guide"),
            Document::new("rust ownership and borrowing"),
            Document::new("cooking pasta recipes"),
        ];
        let ids = store
            .add_documents(docs, &HashEmbeddings)
            .await
            .expect("add");
        assert_eq!(ids.len(), 3);
        assert_eq!(store.len(), 3);

        let hits = store
            .similarity_search("rust borrowing", 2, &HashEmbeddings)
            .await
            .expect("search");
        // The order, not membership: insertion order is the reverse, so a
        // store that returns its hits unranked fails here.
        let texts: Vec<&str> = hits.iter().map(|d| d.page_content.as_str()).collect();
        assert_eq!(
            texts,
            vec!["rust ownership and borrowing", "rust language guide"],
            "the two-shared-word doc outranks the one-shared-word doc"
        );
    }

    #[tokio::test]
    async fn retriever_wraps_the_store() {
        let store = std::sync::Arc::new(tokio::sync::Mutex::new(InMemoryVectorStore::new()));
        store
            .lock()
            .await
            .add_documents(
                vec![
                    Document {
                        page_content: "tokio async runtime".into(),
                        metadata: json!({"topic": "rust"}),
                    },
                    Document::new("gardening tips"),
                ],
                &HashEmbeddings,
            )
            .await
            .expect("add");

        let retriever = VectorStoreRetriever::new(store, std::sync::Arc::new(HashEmbeddings));
        let hits = retriever
            .retrieve("async tokio runtime", 1)
            .await
            .expect("retrieve");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].metadata["topic"], "rust");
    }

    #[tokio::test]
    async fn embedding_count_mismatch_fails_loud() {
        struct Broken;
        #[async_trait]
        impl Embeddings for Broken {
            async fn embed_documents(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
                Ok(vec![]) // wrong count on purpose
            }
            async fn embed_query(&self, _text: &str) -> Result<Vec<f32>> {
                Ok(vec![1.0])
            }
        }
        let mut store = InMemoryVectorStore::new();
        let err = store
            .add_documents(vec![Document::new("x")], &Broken)
            .await
            .expect_err("mismatch must fail");
        assert!(format!("{err:#}").contains("0 vectors for 1 documents"));
    }

    #[test]
    fn cosine_handles_zero_vector_and_length_mismatch() {
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 1.0]), 0.0);
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    }
}
