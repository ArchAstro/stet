#!/bin/sh
# Installs the latest released md, and its Claude Code skill.
#
#   curl -fsSL https://raw.githubusercontent.com/ArchAstro/md/main/install.sh | sh
#
# Settings (environment variables):
#   MD_VERSION      release to install, for example v0.1.0 (default: the latest)
#   MD_INSTALL_DIR  where the md binary goes (default: ~/.local/bin)
#   MD_APP          on macOS, set to 0 for the command only, without md.app
#   MD_APP_DIR      where md.app goes (default: /Applications, else ~/Applications)
#   MD_SKILL        set to 0 to skip the Claude Code skill
#   CLAUDE_SKILLS_DIR  where skills live (default: ~/.claude/skills)
set -eu

repo="ArchAstro/md"
version="${MD_VERSION:-latest}"
bin_dir="${MD_INSTALL_DIR:-$HOME/.local/bin}"
skills_dir="${CLAUDE_SKILLS_DIR:-$HOME/.claude/skills}"

say() { printf '%s\n' "$*"; }
fail() { printf 'md install: %s\n' "$*" >&2; exit 1; }

case "$(uname -s)" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    *) fail "no prebuilt md for $(uname -s). On Windows, download md-windows-x64.zip from https://github.com/$repo/releases" ;;
esac
case "$(uname -m)" in
    arm64 | aarch64) arch=arm64 ;;
    x86_64 | amd64) arch=x64 ;;
    *) fail "no prebuilt md for $(uname -m); build it with: cargo install --git https://github.com/$repo md-app" ;;
esac
asset="md-$os-$arch.tar.gz"
if [ "$os-$arch" = "linux-arm64" ]; then
    fail "no prebuilt md for Linux on arm64 yet; build it with: cargo install --git https://github.com/$repo md-app"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT INT TERM

if [ "$version" = "latest" ]; then
    base="https://github.com/$repo/releases/latest/download"
else
    base="https://github.com/$repo/releases/download/$version"
fi

# Anonymous download first. While the repository is private that fails, and
# a signed-in GitHub CLI is used instead.
fetch() {
    if command -v curl >/dev/null 2>&1 && curl -fsSL --retry 3 -o "$work/$1" "$base/$1" 2>/dev/null; then
        return 0
    fi
    if command -v gh >/dev/null 2>&1; then
        if [ "$version" = "latest" ]; then
            gh release download --repo "$repo" --pattern "$1" --dir "$work" --clobber >/dev/null 2>&1 && return 0
        else
            gh release download "$version" --repo "$repo" --pattern "$1" --dir "$work" --clobber >/dev/null 2>&1 && return 0
        fi
    fi
    return 1
}

# Downloads $1 and checks it against the published checksums.
verified() {
    fetch "$1" || return 1
    [ -f "$work/SHA256SUMS" ] || fetch SHA256SUMS || fail "could not download SHA256SUMS"
    expected="$(awk -v name="$1" '$2 == name { print $1 }' "$work/SHA256SUMS")"
    [ -n "$expected" ] || fail "$1 is not listed in SHA256SUMS"
    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$work/$1" | cut -d' ' -f1)"
    else
        actual="$(shasum -a 256 "$work/$1" | cut -d' ' -f1)"
    fi
    [ "$expected" = "$actual" ] || fail "checksum mismatch for $1; nothing was installed"
}

mkdir -p "$bin_dir"
skill=""

# On a Mac, md.app goes in Applications and `md` on the command line is the
# same program inside it. MD_APP=0 installs only the command.
if [ "$os" = "darwin" ] && [ "${MD_APP:-1}" != "0" ] && verified md-macos-app.zip; then
    apps="${MD_APP_DIR:-/Applications}"
    if [ ! -w "$apps" ]; then
        apps="$HOME/Applications"
        mkdir -p "$apps"
    fi
    rm -rf "$apps/md.app"
    ditto -x -k "$work/md-macos-app.zip" "$apps"
    ln -sf "$apps/md.app/Contents/MacOS/md" "$bin_dir/md"
    skill="$apps/md.app/Contents/Resources/skill/md/SKILL.md"
    say "Installed $("$bin_dir/md" --version) to $apps/md.app, and the md command to $bin_dir/md"
else
    say "Downloading $asset ($version)..."
    verified "$asset" || fail "could not download $asset from $base (if the repository is private, sign in with: gh auth login)"
    mkdir "$work/unpacked"
    tar -xzf "$work/$asset" -C "$work/unpacked"
    # Replace by rename, so an md that is running keeps its own copy.
    rm -f "$bin_dir/.md.new"
    cp "$work/unpacked/md" "$bin_dir/.md.new"
    chmod 755 "$bin_dir/.md.new"
    mv -f "$bin_dir/.md.new" "$bin_dir/md"
    skill="$work/unpacked/skill/md/SKILL.md"
    say "Installed $("$bin_dir/md" --version) to $bin_dir/md"
fi

if [ "${MD_SKILL:-1}" != "0" ] && [ -d "$(dirname "$skills_dir")" ]; then
    if [ -L "$skills_dir/md" ]; then
        say "The skill at $skills_dir/md is a link (a development checkout); left as it is."
    else
        mkdir -p "$skills_dir/md"
        cp "$skill" "$skills_dir/md/SKILL.md"
        say "Installed the Claude Code skill to $skills_dir/md"
    fi
fi

case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) say "Add $bin_dir to your PATH to run md from anywhere." ;;
esac
say "Open a document with: md notes.md"
