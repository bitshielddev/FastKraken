#!/bin/bash
set -euo pipefail

project=$(cd "$(dirname "$0")/.." && pwd)
cd "$project"
case "${1:-}:$#" in
    :0) dry_run= ;;
    --dry-run:1) dry_run=1 ;;
    *) echo "Usage: $0 [--dry-run]" >&2; exit 2 ;;
esac
fail() { echo "$1" >&2; exit 1; }

version=$(cargo metadata --locked --no-deps --format-version=1 |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')
tag="v$version"

[[ $(git branch --show-current) == main ]] || fail "Release from main."
[[ -z $(git status --porcelain) ]] || fail "Commit or stash local changes first."
git fetch --quiet --tags origin
[[ $(git rev-parse HEAD) == $(git rev-parse origin/main) ]] || fail "main differs from origin/main. Push or pull first."
gh auth status >/dev/null 2>&1 || fail "Log in to GitHub with gh auth login first."
if gh release view "$tag" >/dev/null 2>&1; then
    fail "Release $tag already exists. Set a new version in Cargo.toml."
fi
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null &&
    [[ $(git rev-parse "$tag^{commit}") != $(git rev-parse HEAD) ]]; then
    fail "Tag $tag already marks another commit. Set a new version in Cargo.toml."
fi
previous=$(git describe --tags --abbrev=0 --match 'v*' HEAD^ 2>/dev/null || true)

echo "Checking FastKraken $version at $(git rev-parse --short HEAD)"
scripts/package-macos.sh
cargo test --locked --release
python3 scripts/test-bundled-git.py
python3 scripts/check-acceptance.py

app=dist/FastKraken.app
[[ $(plutil -extract CFBundleShortVersionString raw "$app/Contents/Info.plist") == "$version" ]] ||
    fail "The packaged app does not report version $version."

out=$(mktemp -d "$project/dist/.release.XXXXXX")
trap 'rm -rf "$out"' EXIT
app_zip="FastKraken-$version-macos-arm64.zip"
sources_zip="FastKraken-$version-Git-sources.zip"
ditto -c -k --keepParent "$app" "$out/$app_zip"
ditto -c -k --keepParent dist/Git-sources "$out/$sources_zip"
mkdir "$out/unpacked"
ditto -x -k "$out/$app_zip" "$out/unpacked"
codesign --verify --deep --strict "$out/unpacked/FastKraken.app" || fail "The zipped app fails signature checks."
(cd "$out" && shasum -a 256 "$app_zip" "$sources_zip" > SHA256SUMS)

git_version=$("$app/Contents/Helpers/git/bin/git" --version)
{
    echo "FastKraken $version for Apple Silicon Macs, built from $(git rev-parse --short HEAD)."
    echo
    if [[ -n $previous ]]; then
        echo "## Changes since $previous"
        echo
        git log --reverse --format='- %s' "$previous..HEAD"
    else
        echo "This is the first release."
    fi
    echo
    echo "## Install"
    echo
    echo "Unzip \`$app_zip\` and move FastKraken.app to Applications. The app is not notarised yet, so macOS blocks the first launch: try to open it, then choose **Open Anyway** in **System Settings → Privacy & Security**."
    echo
    echo "## Bundled Git"
    echo
    echo "FastKraken includes Git ${git_version#git version }, which is licensed under GPLv2. \`$sources_zip\` holds its source, the source of the dugite-native build that packages it, and the script that puts it in the app."
    echo
    echo "## SHA-256"
    echo
    echo '```'
    cat "$out/SHA256SUMS"
    echo '```'
} > "$out/notes.md"

if [[ -n $dry_run ]]; then
    echo "Dry run: would tag $tag and publish these notes with $app_zip, $sources_zip and SHA256SUMS."
    cat "$out/notes.md"
    exit 0
fi

git rev-parse -q --verify "refs/tags/$tag" >/dev/null || git tag -a "$tag" -m "FastKraken $version"
git push --quiet origin "refs/tags/$tag"
gh release create "$tag" --verify-tag --title "FastKraken $version" --notes-file "$out/notes.md" \
    "$out/$app_zip" "$out/$sources_zip" "$out/SHA256SUMS"
