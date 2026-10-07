#!/bin/sh
# Installs stet and its Claude Code skill.
#
#   sh scripts/install.sh            from a checkout
#   gh repo clone ArchAstro/stet && sh stet/scripts/install.sh
#
# Puts the `stet` binary in ~/.cargo/bin and the skill in ~/.claude/skills/stet.
set -eu

repo="$(cd "$(dirname "$0")/.." && pwd)"

if ! command -v cargo >/dev/null 2>&1; then
    echo "stet needs Rust to build. Install it from https://rustup.rs and run this again." >&2
    exit 1
fi

echo "Building stet (a few minutes the first time)..."
cargo install --path "$repo/crates/stet-app" --locked

skills="${CLAUDE_SKILLS_DIR:-$HOME/.claude/skills}"
mkdir -p "$skills"
if [ -e "$skills/stet" ] || [ -L "$skills/stet" ]; then
    echo "Skill already present at $skills/stet; leaving it as it is."
else
    # A link, so the skill follows the checkout when it is updated.
    ln -s "$repo/skill/stet" "$skills/stet"
    echo "Linked the skill into $skills/stet"
fi

case ":$PATH:" in
    *":$HOME/.cargo/bin:"*) ;;
    *) echo "Add ~/.cargo/bin to your PATH to run stet from anywhere." ;;
esac
echo "Done. Open a document with: stet notes.md"
