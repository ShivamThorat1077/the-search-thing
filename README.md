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

Tested and verified on Ubuntu 24.04 LTS.

### System Requirements

- Ubuntu 24.04+ — REQUIRED. HelixDB CLI requires GLIBC 2.39 which ships with Ubuntu 24.04+. Ubuntu 22.04 and older will not work.
- Docker — required by HelixDB CLI to run the database container
- Rust stable — for building the sidecar backend
- Node.js 20+ — react-router-dom and other packages require Node 20+
- build-essential — C compiler required for cargo build

### Step 1 — Install system dependencies

```bash
# C compiler and OpenSSL (required for cargo build)
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libssl-dev

# Docker
sudo apt-get install -y docker.io
sudo systemctl start docker
sudo systemctl enable docker
sudo usermod -aG docker $USER
newgrp docker

# Rust (use rustup, NOT apt)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Node.js 20+ (NOT the default apt version which is 18)
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt-get install -y nodejs

# Verify versions
node --version    # must be v20+
rustc --version
docker --version

# HelixDB CLI
curl -fsSL https://install.helix-db.com | bash
source ~/.bashrc
helix --version   # should show 3.0.2
```

### Step 2 — Clone this fork

```bash
git clone https://github.com/ShivamThorat1077/the-search-thing.git
cd the-search-thing
```

### Step 3 — Set up the HelixDB local SDK

The project depends on the `helix-db` Rust SDK as a local path dependency.
The folder MUST be named `helix-db-local` and placed directly inside the project root.

Expected structure:

    the-search-thing/       <- project root
        helix-db-local/     <- SDK goes HERE (this exact name, this exact location)
            Cargo.toml
            helix-cli/
            sdks/
        src/
        client/
        Cargo.toml

Clone it from inside the project root:

```bash
git clone https://github.com/HelixDB/helix-db.git helix-db-local
```

Verify:

```bash
ls helix-db-local/
# Should show: Cargo.toml  helix-cli/  sdks/  assets/  ...
```

`Cargo.toml` already points to `helix-db-local/` — no other changes needed.

### Step 4 — Create your .env file

```bash
cp .env.example .env
nano .env
```

Fill in your keys. The file must look exactly like this — NO quotes, NO spaces around =:

```env
VOYAGE_API_KEY=your_actual_key_here
VOYAGE_EMBED_MODEL=voyage-3.5
VOYAGE_RETRIEVAL_MODEL=voyage-3.5
HELIX_ENDPOINT=http://localhost
HELIX_PORT=6969
GROQ_API_KEY=your_groq_key_here
```

Get your Voyage API key at https://dashboard.voyageai.com
Get your Groq API key at https://console.groq.com (optional — only needed for image/video indexing)

IMPORTANT:
Correct:   VOYAGE_API_KEY=pa-abc123
Wrong:     VOYAGE_API_KEY="pa-abc123"
Wrong:     VOYAGE_API_KEY = pa-abc123

Wrong format causes keys to be executed as shell commands and leaks them to the terminal.


### Step 5 — Start HelixDB

```bash
helix run dev
```

This starts the enterprise-dev container at http://localhost:6969.

Verify it is running:

```bash
curl http://localhost:6969
```

WARNING: HelixDB enterprise-dev uses in-memory storage.
Stopping or restarting the container wipes all indexed data.
Re-indexing after a restart takes only a few minutes for small directories.

### Step 6 — Build the Rust sidecar

```bash
cargo build --bin the-search-thing-sidecar
```

This takes 2-5 minutes on first build.

### Step 7 — Install frontend dependencies and run

```bash
cd client
npm install
npm run dev
```

The Electron app will launch automatically.
Use the search bar to index a directory and start searching.
Click a result to see the file content preview in the right panel.

---

## Verify everything works (CLI test)

Before using the UI, confirm the full pipeline works from the command line:

```bash
# Create sample test files
mkdir -p /tmp/test-index
echo "semantic search using AI embeddings" > /tmp/test-index/readme.txt
echo "jwt authentication middleware token validator" > /tmp/test-index/auth.txt

# Load env
cd ~/the-search-thing
set -a && source .env && set +a

# Run index + search
(
  echo '{"jsonrpc":"2.0","id":1,"method":"index.start","params":{"dir":"/tmp/test-index"}}'
  sleep 90
  echo '{"jsonrpc":"2.0","id":2,"method":"search.query","params":{"q":"authentication"}}'
  sleep 2
  echo '{"jsonrpc":"2.0","id":3,"method":"search.query","params":{"q":"semantic search"}}'
) | ./target/debug/the-search-thing-sidecar 2>&1 | cat
```

Expected result: both queries return auth.txt and readme.txt ranked by semantic similarity.
If you see `indexed=2, errors=0` and results in the search response — everything is working.

---

## Common errors and fixes

| Error | Cause | Fix |
|-------|-------|-----|
| `GLIBC_2.39 not found` | Ubuntu version too old | Upgrade to Ubuntu 24.04+ |
| `linker cc not found` | build-essential not installed | `sudo apt-get install -y build-essential` |
| `cross-env not found` | Node.js version too old | Install Node 20+ via nodesource |
| `Model  is not supported` | VOYAGE_EMBED_MODEL missing or empty | Add `VOYAGE_EMBED_MODEL=voyage-3.5` to .env |
| Keys run as shell commands | .env values have quotes or spaces | Remove all quotes, remove spaces around = |
| `connection refused` on port 6969 | HelixDB not running | Run `helix run dev` |
| `indexed=0, errors=N` | Bad API key or HelixDB not running | Check VOYAGE_API_KEY and run `curl http://localhost:6969` |
| `pkg-config not found` | Missing system library | `sudo apt-get install -y pkg-config libssl-dev` |
| `Docker not available` | Docker not installed or not running | Install docker.io and run `sudo systemctl start docker` |

---

## Rate limits

Voyage AI free tier: 3 RPM. The indexer has a built-in 21-second delay between embedding calls to stay within this limit. Indexing 3 files takes approximately 2 minutes.

To remove the delay: add a payment method at https://dashboard.voyageai.com (rate limit increases to 300+ RPM), then remove the two `sleep(Duration::from_secs(21))` calls in `src/sidecar/rpc/indexing/text/mod.rs`.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, dev workflow, and frontend website instructions.

## License

GPL-3.0-only. See `LICENSE` for details.
