#!/bin/bash
# Export toolchains including Cargo and our Zig CC wrappers
export PATH="/home/demon/.gemini/antigravity/scratch/bin:/home/demon/.gemini/antigravity/scratch/node-v20.11.1-linux-x64/bin:$HOME/.cargo/bin:$PATH"

echo "=================================================="
echo " Building & Staging the Rust Sidecar              "
echo "=================================================="

cd "$(dirname "$0")/client"
npm run sidecar:stage
