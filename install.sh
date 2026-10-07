#!/bin/sh
# Installs the latest released stet, and its Claude Code skill.
#
#   curl -fsSL https://raw.githubusercontent.com/ArchAstro/stet/main/install.sh | sh
#
# Settings (environment variables):
#   STET_VERSION      release to install, for example v0.1.0 (default: the latest)
#   STET_INSTALL_DIR  where the stet binary goes (default: ~/.local/bin)
#   STET_APP          on macOS, set to 0 for the command only, without Stet.app
#   STET_APP_DIR      where Stet.app goes (default: /Applications, else ~/Applications)
#   STET_SKILL        set to 0 to skip the Claude Code skill
#   CLAUDE_SKILLS_DIR  where skills live (default: ~/.claude/skills)
set -eu

repo="ArchAstro/stet"
version="${STET_VERSION:-latest}"
bin_dir="${STET_INSTALL_DIR:-$HOME/.local/bin}"
skills_dir="${CLAUDE_SKILLS_DIR:-$HOME/.claude/skills}"

say() { printf '%s\n' "$*"; }
fail() { printf 'stet install: %s\n' "$*" >&2; exit 1; }

case "$(uname -s)" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    *) fail "no prebuilt stet for $(uname -s). On Windows, download stet-windows-x64.zip from https://github.com/$repo/releases" ;;
esac
case "$(uname -m)" in
    arm64 | aarch64) arch=arm64 ;;
    x86_64 | amd64) arch=x64 ;;
    *) fail "no prebuilt stet for $(uname -m); build it with: cargo install --git https://github.com/$repo stet-app" ;;
esac
asset="stet-$os-$arch.tar.gz"
if [ "$os-$arch" = "linux-arm64" ]; then
    fail "no prebuilt stet for Linux on arm64 yet; build it with: cargo install --git https://github.com/$repo stet-app"
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

# On a Mac, Stet.app goes in Applications and `stet` on the command line is the
# same program inside it. STET_APP=0 installs only the command.
if [ "$os" = "darwin" ] && [ "${STET_APP:-1}" != "0" ] && verified stet-macos-app.zip; then
    apps="${STET_APP_DIR:-/Applications}"
    if [ ! -w "$apps" ]; then
        apps="$HOME/Applications"
        mkdir -p "$apps"
    fi
    rm -rf "$apps/Stet.app"
    ditto -x -k "$work/stet-macos-app.zip" "$apps"
    ln -sf "$apps/Stet.app/Contents/MacOS/stet" "$bin_dir/stet"
    skill="$apps/Stet.app/Contents/Resources/skill/stet/SKILL.md"
    say "Installed $("$bin_dir/stet" --version) to $apps/Stet.app, and the stet command to $bin_dir/stet"
else
    say "Downloading $asset ($version)..."
    verified "$asset" || fail "could not download $asset from $base (if the repository is private, sign in with: gh auth login)"
    mkdir "$work/unpacked"
    tar -xzf "$work/$asset" -C "$work/unpacked"
    # Replace by rename, so an stet that is running keeps its own copy.
    rm -f "$bin_dir/.stet.new"
    cp "$work/unpacked/stet" "$bin_dir/.stet.new"
    chmod 755 "$bin_dir/.stet.new"
    mv -f "$bin_dir/.stet.new" "$bin_dir/stet"
    skill="$work/unpacked/skill/stet/SKILL.md"
    say "Installed $("$bin_dir/stet" --version) to $bin_dir/stet"
fi

if [ "${STET_SKILL:-1}" != "0" ] && [ -d "$(dirname "$skills_dir")" ]; then
    if [ -L "$skills_dir/stet" ]; then
        say "The skill at $skills_dir/stet is a link (a development checkout); left as it is."
    else
        mkdir -p "$skills_dir/stet"
        cp "$skill" "$skills_dir/stet/SKILL.md"
        say "Installed the Claude Code skill to $skills_dir/stet"
    fi
fi

case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) say "Add $bin_dir to your PATH to run stet from anywhere." ;;
esac
say "Open a document with: stet notes.md"
