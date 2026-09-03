#!/usr/bin/env bash
# Upload dist/* to a GitHub release and drop assets that are no longer in dist/.
set -euo pipefail
tag=${1:?usage: sync_release_assets.sh <tag>}
shopt -s nullglob
assets=(dist/*)
if (( ${#assets[@]} == 0 )); then
  echo "no files in dist/" >&2
  exit 1
fi
if gh release view "$tag" >/dev/null 2>&1; then
  while IFS= read -r name; do
    [[ -z "$name" ]] && continue
    if [[ ! -f "dist/$name" ]]; then
      gh release delete-asset "$tag" "$name" --yes
    fi
  done < <(gh release view "$tag" --json assets --jq '.assets[].name')
fi
gh release upload "$tag" "${assets[@]}" --clobber
