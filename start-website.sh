#!/bin/bash
# Automatically export custom portable toolchains
export PATH="/home/demon/.gemini/antigravity/scratch/bin:/home/demon/.gemini/antigravity/scratch/node-v20.11.1-linux-x64/bin:$PATH"

echo "=================================================="
echo " Starting Next.js Marketing/Docs Website          "
echo "=================================================="

cd "$(dirname "$0")/website"
npm run dev -- -p 3000
