#!/bin/bash
# Automatically export custom portable toolchains
export PATH="/home/demon/.gemini/antigravity/scratch/bin:/home/demon/.gemini/antigravity/scratch/node-v20.11.1-linux-x64/bin:$PATH"

echo "=================================================="
echo " Starting The-Search-Thing Electron Client App    "
echo "=================================================="

cd "$(dirname "$0")/client"
npm run dev
