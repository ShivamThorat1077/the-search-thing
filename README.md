<h1 align="center">the-search-thing</h1>
<div align="center">
  <img src="branding/logo-white-bg.webp" alt="the-search-thing" width="400" />
  <p>Semantically search for your files, instantly*</p>
</div>

## What it is

the-search-thing is a local-first search system that makes your files, images, and videos semantically searchable from one place.

## Features

- Semantic search across files, images, and videos
- Sub-millisecond response targets for interactive search
- Directory indexing with ignore rules

---

## Migration: HelixDB Dynamic Query API (v2.0.1)

This fork migrates the HelixDB integration from the old `helix-rs` SDK (stored `.hx` queries) to the new `helix-db` SDK (v2.0.1) using the dynamic query DSL (`POST /v1/query`).

### What changed

| File | Change |
|------|--------|
| `Cargo.toml` | Replaced `helix-rs` with `helix-db` local path dependency |
| `src/sidecar/rpc/indexing/adapters/helix.rs` | Full rewrite — all 6 queries use dynamic query DSL |
| `src/sidecar/rpc/search.rs` | Updated to use `HelixTextStore.search_asset_embeddings()`, fixed response parsing |
| `src/sidecar/rpc/indexing/text/mod.rs` | Added rate limiter (21s delay) for Voyage free tier |
| `db/queries.hx` | UpsertV/UpsertE replaced with AddV/AddE; SearchAssetEmbeddings returns both assets and embeddings |
| `db/schema.hx` | No changes — compatible as-is |

### Key fix: HNSW index must exist before inserts

HelixDB HNSW vector index must be created BEFORE any embedding nodes are inserted.
Nodes inserted before the index exists are not backfilled and are invisible to vector search.

`ensure_indexes()` is now called automatically at the start of every indexing job, creating:
- HNSW vector index on AssetEmbedding/vector
- Equality index on Asset/content_hash (speeds up duplicate detection)

---

## Setup & Running (Reviewer Guide)

### Prerequisites

- Rust (stable) — https://rustup.rs
- Node.js 18+ and npm
- Docker
- HelixDB CLI — https://helix-db.com/docs/installation

### 1. Clone this fork

```bash
git clone https://github.com/ShivamThorat1077/the-search-thing.git
cd the-search-thing
```

### 2. Set up the HelixDB local SDK

The project depends on the `helix-db` Rust SDK as a local path dependency.
The folder MUST be named `helix-db-local` and placed directly inside the project root.

Expected structure:

    the-search-thing/       <- project root
        helix-db-local/     <- SDK goes HERE (this exact name, this exact location)
            Cargo.toml
            src/
        src/
        client/
        Cargo.toml

Clone it with this command from inside the project root:

```bash
git clone https://github.com/HelixDB/helix-db.git helix-db-local
```

Verify it worked:

```bash
ls helix-db-local/
# Should show: Cargo.toml  src/  ...
```

`Cargo.toml` already points to `helix-db-local/` — no other changes needed.

### 3. Create your .env file

Copy the example file and fill in your keys:

```bash
cp .env.example .env
```

Then edit `.env` and replace the placeholder values:

```env
# Required — get your key at https://dashboard.voyageai.com
VOYAGE_API_KEY=your_actual_key_here
VOYAGE_EMBED_MODEL=voyage-3.5
VOYAGE_RETRIEVAL_MODEL=voyage-3.5

# HelixDB (default values — change only if your setup differs)
HELIX_ENDPOINT=http://localhost
HELIX_PORT=6969

# Optional — enables image and video indexing
# Get your key at https://console.groq.com
GROQ_API_KEY=your_groq_api_key_here
```

IMPORTANT: The .env file must have NO quotes around values and NO extra spaces.
Correct:   VOYAGE_API_KEY=pa-abc123
Wrong:     VOYAGE_API_KEY="pa-abc123"
Wrong:     VOYAGE_API_KEY = pa-abc123

Never commit your .env file — it is already in .gitignore.

### 4. Start HelixDB

```bash
helix run dev
```

This starts the enterprise-dev container at http://localhost:6969.

WARNING: This uses in-memory storage — stopping the container wipes all indexed data.
Re-indexing after a restart takes only a few minutes for small directories.

### 5. Build the Rust sidecar

```bash
cargo build --bin the-search-thing-sidecar
```

### 6. Install frontend dependencies and run

```bash
cd client
npm install
npm run dev
```

The Electron app will launch. Use the search bar to index a directory and start searching.

---

## Testing search from the CLI

First create a test directory with sample files:

```bash
mkdir -p /tmp/test-index
echo "This is a test file about semantic search and AI embeddings" > /tmp/test-index/readme.txt
echo "Authentication happens in jwt_middleware and token_validator" > /tmp/test-index/auth.txt
echo "Database connection pooling and query optimization" > /tmp/test-index/db.txt
```

Then run the indexer and search:

```bash
cd ~/the-search-thing
set -a && source .env && set +a

(
  echo '{"jsonrpc":"2.0","id":1,"method":"index.start","params":{"dir":"/tmp/test-index"}}'
  sleep 90
  echo '{"jsonrpc":"2.0","id":2,"method":"search.query","params":{"q":"authentication jwt"}}'
  sleep 2
  echo '{"jsonrpc":"2.0","id":3,"method":"search.query","params":{"q":"semantic search"}}'
) | ./target/debug/the-search-thing-sidecar 2>&1 | cat
```

Expected output: search results showing the correct files ranked by semantic similarity.

Common errors and fixes:

1. "Model  is not supported" — VOYAGE_EMBED_MODEL is missing from .env
2. "command not found" after source .env — .env has wrong format (remove quotes around values)
3. "connection refused" — HelixDB is not running, run: helix run dev
4. indexed=0, errors=1 — check VOYAGE_API_KEY is valid at https://dashboard.voyageai.com

---

## Rate limits

Voyage AI free tier: 3 RPM. The indexer includes a 21-second delay between embedding calls.

To remove the delay, add a payment method at https://dashboard.voyageai.com (rate limit
increases to 300+ RPM), then remove the two sleep(Duration::from_secs(21)) calls in
src/sidecar/rpc/indexing/text/mod.rs.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, dev workflow, and frontend website instructions.

## License

GPL-3.0-only. See `LICENSE` for details.
