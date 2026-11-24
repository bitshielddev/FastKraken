#!/bin/bash
set -euo pipefail

invoked_from=$PWD
project=$(cd "$(dirname "$0")/.." && pwd)
cd "$project"
if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    echo "This package targets Apple Silicon macOS." >&2
    exit 1
fi

mode=${1:-build}
case "$mode:$#" in
    build:0|--prepare-git:1|--binary:2) ;;
    *) echo "Usage: $0 [--prepare-git | --binary PATH]" >&2; exit 2 ;;
esac

cache="$project/target/portable-git"
mkdir -p "$cache" "$project/dist"
stage=$(mktemp -d "$project/dist/.fastkraken-build.XXXXXX")
trap 'rm -rf "$stage"' EXIT

version=2.56.1
git_rev=03b41523c576f26ad34085378717a0c1b287470f
toolchain_rev=238326e93b097bb31e138a75dbec433d789ef7a2
archive="dugite-native-v$version-238326e-macOS-arm64.tar.gz"
git_source="git-$version-source.tar.gz"
build_source="dugite-native-$version-source.tar.gz"

fetch_verified() {
    local name=$1 url=$2 expected=$3 input="$cache/$1"
    if [[ -f "$input" ]] && [[ $(shasum -a 256 "$input" | cut -d' ' -f1) == "$expected" ]]; then
        return
    fi
    curl --fail --location --silent --show-error --retry 2 "$url" -o "$stage/$name"
    if [[ $(shasum -a 256 "$stage/$name" | cut -d' ' -f1) != "$expected" ]]; then
        echo "SHA-256 verification failed for $name" >&2
        exit 1
    fi
    mv -f "$stage/$name" "$input"
}

fetch_verified "$archive" "https://github.com/desktop/dugite-native/releases/download/v$version/$archive" \
    a882ec6882416b57d212a22bac845ae27944f96cf73453a864f6a12c17863a79
fetch_verified "$git_source" "https://codeload.github.com/git/git/tar.gz/$git_rev" \
    234c011d07f7a7c8420ad3a1b692e71ec5b5a62a4626b07bc8d20a332eb3b1a8
fetch_verified "$build_source" "https://codeload.github.com/desktop/dugite-native/tar.gz/$toolchain_rev" \
    1a0f4513f6773472181efdd67052b3d2be04d8def7315570f9b3e46b9ef48697

mkdir "$stage/runtime"
tar -xzf "$cache/$archive" -C "$stage/runtime" \
    '--exclude=*/git-lfs' '--exclude=*/git-credential-manager*' \
    './bin/git' './libexec/git-core/git*' './libexec/git-core/mergetools' './share/git-core/templates'
if [[ $("$stage/runtime/bin/git" --version) != "git version $version" ]]; then
    echo "Unexpected Git runtime in verified archive" >&2
    exit 1
fi

if [[ "$mode" == --prepare-git ]]; then
    if [[ -e "$cache/runtime" ]]; then mv "$cache/runtime" "$stage/prior-runtime"; fi
    mv "$stage/runtime" "$cache/runtime"
    echo "$cache/runtime/bin/git"
    exit 0
fi

metadata=$(cargo metadata --locked --no-deps --format-version=1)
target_dir=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])' <<<"$metadata")
app_version=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])' <<<"$metadata")
if [[ "$mode" == --binary ]]; then
    case $2 in
        /*) binary=$2 ;;
        *) binary="$invoked_from/$2" ;;
    esac
else
    cargo build --locked --release --bin fastkraken
    binary="$target_dir/release/fastkraken"
fi
[[ -x "$binary" ]] || { echo "Missing FastKraken executable: $binary" >&2; exit 1; }
if ! lipo -verify_arch arm64 "$binary"; then
    echo "The app needs an Apple Silicon executable." >&2
    exit 1
fi

app="$stage/FastKraken.app"
contents="$app/Contents"
licenses="$contents/Resources/Licenses"
mkdir -p "$contents/MacOS" "$contents/Helpers" "$licenses"
cp packaging/macos/Info.plist "$contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$app_version" "$contents/Info.plist"
plutil -replace CFBundleVersion -string "$(git rev-list --count HEAD 2>/dev/null || echo 0)" "$contents/Info.plist"
plutil -lint "$contents/Info.plist" >/dev/null
cp packaging/macos/FastKraken.icns "$contents/Resources/FastKraken.icns"
install -m 755 "$binary" "$contents/MacOS/fastkraken"
engine="$contents/Helpers/git"
resources="$contents/Resources/git"
mv "$stage/runtime" "$engine"
mkdir -p "$resources/libexec/git-core"
mv "$engine/share" "$resources/share"
ln -s ../../Resources/git/share "$engine/share"
mv "$engine/libexec/git-core/mergetools" "$resources/libexec/git-core/"
ln -s ../../../../Resources/git/libexec/git-core/mergetools "$engine/libexec/git-core/mergetools"
for helper in "$engine"/libexec/git-core/*; do
    [[ ! -L "$helper" && -f "$helper" ]] || continue
    case $(file -b "$helper") in
        Mach-O*) ;;
        *)
            name=${helper##*/}
            mv "$helper" "$resources/libexec/git-core/$name"
            ln -s "../../../../Resources/git/libexec/git-core/$name" "$helper"
            ;;
    esac
done
tar -xOzf "$cache/$git_source" "git-$git_rev/COPYING" > "$licenses/Git-COPYING"
tar -xOzf "$cache/$build_source" "dugite-native-$toolchain_rev/LICENSE.md" > "$licenses/dugite-native-LICENSE"
cp packaging/macos/git-notices.txt "$licenses/"
codesign --force --sign - "$app"
codesign --verify --deep --strict "$app"

mkdir -p dist/Git-sources
cp "$cache/$git_source" "$cache/$build_source" dist/Git-sources/
cp scripts/package-macos.sh dist/Git-sources/package-macos.sh
if [[ -e dist/FastKraken.app ]]; then
    mv dist/FastKraken.app "$stage/prior.app"
fi
if ! mv "$app" dist/FastKraken.app; then
    if [[ -e "$stage/prior.app" ]]; then mv "$stage/prior.app" dist/FastKraken.app; fi
    exit 1
fi
echo "$project/dist/FastKraken.app"
