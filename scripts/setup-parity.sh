#!/usr/bin/env bash
# Set up the libmypaint parity-test environment.
#
# Installs the libmypaint reference renderer's dependencies, builds the
# C wrapper, and clones the upstream brush pack used by
# `cargo xtask brush-pack-report`.
#
# Idempotent — safe to re-run; existing artefacts are reused.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT=$(pwd)

echo "==> Installing libmypaint + json-c"
case "$(uname -s)" in
    Darwin)
        if ! command -v brew >/dev/null 2>&1; then
            echo "Homebrew not found. Install from https://brew.sh and re-run." >&2
            exit 1
        fi
        brew list libmypaint >/dev/null 2>&1 || brew install libmypaint
        brew list json-c     >/dev/null 2>&1 || brew install json-c
        brew list pkg-config >/dev/null 2>&1 || brew install pkg-config
        ;;
    Linux)
        if command -v apt-get >/dev/null 2>&1; then
            sudo apt-get update
            sudo apt-get install -y libmypaint-dev libjson-c-dev pkg-config build-essential
        elif command -v dnf >/dev/null 2>&1; then
            sudo dnf install -y libmypaint-devel json-c-devel pkgconf-pkg-config gcc make
        elif command -v pacman >/dev/null 2>&1; then
            sudo pacman -S --needed --noconfirm libmypaint json-c pkgconf base-devel
        else
            echo "Unknown package manager. Install libmypaint, json-c, pkg-config manually." >&2
            exit 1
        fi
        ;;
    *)
        echo "Unsupported OS: $(uname -s). Please install libmypaint + json-c manually." >&2
        exit 1
        ;;
esac

echo "==> Verifying pkg-config can find libmypaint"
pkg-config --modversion libmypaint
pkg-config --modversion json-c

echo "==> Cloning mypaint-brushes (CC0) to tmp/mypaint-brushes"
mkdir -p tmp
if [ ! -d tmp/mypaint-brushes/.git ]; then
    git clone --depth 1 https://github.com/mypaint/mypaint-brushes.git tmp/mypaint-brushes
else
    echo "    already cloned — skipping"
fi

echo "==> Building the libmypaint C wrapper (tools/libmypaint-render)"
make -C tools/libmypaint-render

echo
echo "Done. You can now run the parity reports:"
echo
echo "    cargo xtask brush-pack-report       # full 196-brush MAD table"
echo "    cargo xtask parity-report           # HTML side-by-side for hokusai-compat fixtures"
echo
echo "Outputs land in tmp/brush-pack-report.md and tmp/parity-report/."
echo
echo "To trace per-dab values (hokusai vs libmypaint, line-for-line):"
echo "    HOKUSAI_TRACE_DABS=1 cargo xtask brush-pack-report 2> hok.log"
echo "    HOKUSAI_TRACE_DABS=1 ./tools/libmypaint-render/libmypaint-render \\"
echo "        tmp/_brush_pack_script.json \\"
echo "        \$(realpath tmp/mypaint-brushes/brushes/classic/imp_details.myb) \\"
echo "        > /dev/null 2> lmp.log"
echo "    paste <(grep hok# hok.log) <(grep lmp# lmp.log) | less"
