#!/bin/sh
# Installs md and its Claude Code skill.
#
#   sh scripts/install.sh            from a checkout
#   gh repo clone ArchAstro/md && sh md/scripts/install.sh
#
# Puts the `md` binary in ~/.cargo/bin and the skill in ~/.claude/skills/md.
set -eu

repo="$(cd "$(dirname "$0")/.." && pwd)"

if ! command -v cargo >/dev/null 2>&1; then
    echo "md needs Rust to build. Install it from https://rustup.rs and run this again." >&2
    exit 1
fi

echo "Building md (a few minutes the first time)..."
cargo install --path "$repo/crates/md-app" --locked

skills="${CLAUDE_SKILLS_DIR:-$HOME/.claude/skills}"
mkdir -p "$skills"
if [ -e "$skills/md" ] || [ -L "$skills/md" ]; then
    echo "Skill already present at $skills/md; leaving it as it is."
else
    # A link, so the skill follows the checkout when it is updated.
    ln -s "$repo/skill/md" "$skills/md"
    echo "Linked the skill into $skills/md"
fi

case ":$PATH:" in
    *":$HOME/.cargo/bin:"*) ;;
    *) echo "Add ~/.cargo/bin to your PATH to run md from anywhere." ;;
esac
echo "Done. Open a document with: md notes.md"
