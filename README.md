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
| `src/sidecar/rpc/indexing/text/mod.rs` | Added rate limiter for Voyage free tier |
| `db/queries.hx` | UpsertV/UpsertE replaced with AddV/AddE; SearchAssetEmbeddings returns both assets and embeddings |
| `db/schema.hx` | No changes — compatible as-is |

### Key fix: HNSW index must exist before inserts

HelixDB HNSW vector index must be created BEFORE any embedding nodes are inserted.
Nodes inserted before the index exists are not backfilled and are invisible to vector search.

`ensure_indexes()` is now called automatically at the start of every indexing job, creating:
- HNSW vector index on AssetEmbedding/vector
- Equality index on Asset/content_hash (speeds up duplicate detection)

Because HelixDB's HNSW index does not appear to pick up nodes inserted *after* the index already exists, `rebuild_vector_index()` is also called at the end of every indexing job. This drops and recreates the vector index so newly inserted embeddings (including video frame summaries added later in the job) are guaranteed to be searchable.

---

## Image & Video Indexing

In addition to text files, the indexer can process images and videos and make their content semantically searchable.

### How it works

**Images**
- Each image is sent to a Groq vision model (`meta-llama/llama-4-scout-17b-16e-instruct`), which returns a structured JSON summary (summary, objects, actions, setting, OCR text, quality).
- The summary is formatted into embeddable text and stored as an `AssetEmbedding` linked to the image's `Asset` node.

**Videos**
- Each video is split into fixed-length chunks with `ffmpeg`.
- Per chunk, the indexer extracts:
  - An audio track (if present), transcribed with Groq Whisper (`whisper-large-v3-turbo`)
  - One representative thumbnail frame, summarized with the same Groq vision model used for images
- Transcript text and frame summaries are each stored as separate `AssetEmbedding` units (`video_transcript`, `video_frame_summary`) linked to the video's `Asset` node, so a search query can match either the spoken content or the visual content of a video.
- A `video_index_state` marker is written once a video finishes processing, used to detect and skip already-completed videos on re-index.

### Requirements

Image and video indexing require:
- `ffmpeg` and `ffprobe` installed and available on `PATH` (videos only)
- `GROQ_API_KEY` set in `.env`

If `GROQ_API_KEY` is missing or invalid, image and video indexing are skipped automatically — text indexing still completes normally.

```bash
# ffmpeg/ffprobe (Ubuntu)
sudo apt-get install -y ffmpeg
```

### Search results

Search results for images and videos include:
- `match_kind` — which part of the asset matched (`video_transcript`, `video_frame_summary`, or the image summary)
- `content` — a text preview of the matched transcript or summary
- `thumbnail_url` — a cached preview thumbnail (videos only)

### Known limitations

- HelixDB's enterprise-dev container stores everything in memory — restarting it wipes all indexed images and videos along with text.
- Groq's free tier has both per-minute and per-day token limits. Large batches of images/videos can hit these limits; when this happens, image/video indexing for the affected files will show as errored in the job status while text indexing is unaffected.

---

## Setup & Running (Reviewer Guide)

Tested and verified on Ubuntu 24.04 LTS.

### System Requirements

- Ubuntu 24.04+ — REQUIRED. HelixDB CLI requires GLIBC 2.39 which ships with Ubuntu 24.04+. Ubuntu 22.04 and older will not work.
- Docker — required by HelixDB CLI to run the database container
- Rust stable — for building the sidecar backend
- Node.js 20+ — react-router-dom and other packages require Node 20+
- build-essential — C compiler required for cargo build
- ffmpeg — required for video indexing (see Image & Video Indexing section above)

### Step 1 — Install system dependencies

```bash
# C compiler and OpenSSL (required for cargo build)
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libssl-dev

# ffmpeg (required for video indexing)
sudo apt-get install -y ffmpeg

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
ffmpeg -version
ffprobe -version

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
| `ffprobe failed: No such file or directory` | ffmpeg not installed | `sudo apt-get install -y ffmpeg` |
| Video/image indexing skipped silently | GROQ_API_KEY missing or invalid | Add a valid key to `.env`; text indexing continues regardless |
| Frame summaries missing from search results | Groq token rate limit hit during indexing | Re-index later once the limit resets; check sidecar logs for `429` errors |

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, dev workflow, and frontend website instructions.

## License

GPL-3.0-only. See `LICENSE` for details.