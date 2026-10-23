#!/usr/bin/env bash
# Build one Maul release archive and a basename SHA256 checksum.
#
# Usage: ci/package_release.sh <target> <version> <binary> <dist-dir>
#
# The checksum line is "<digest>  <archive>.tar.gz" with no directory prefix,
# so both `sha256sum -c` (publish job) and the setup-maul installer can verify
# the file from the directory that contains it.
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: ci/package_release.sh <target> <version> <binary> <dist-dir>" >&2
  exit 2
fi

target="$1"
version="$2"
binary="$3"
dist="$4"

if [[ ! -f "${binary}" ]]; then
  echo "binary not found: ${binary}" >&2
  exit 1
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
archive="maul-${version}-${target}"

rm -rf "${dist:?}/${archive:?}" "${dist:?}/${archive:?}.tar.gz" "${dist:?}/${archive:?}.tar.gz.sha256"
mkdir -p "${dist}/${archive}"
cp "${binary}" "${dist}/${archive}/maul"
chmod +x "${dist}/${archive}/maul"
cp "${root}/README.md" "${root}/LICENSE" "${root}/SECURITY.md" "${dist}/${archive}/"

tar -C "${dist}" -czf "${dist}/${archive}.tar.gz" "${archive}"

(
  cd "${dist}"
  if command -v sha256sum >/dev/null 2>&1; then
    digest="$(sha256sum "${archive}.tar.gz" | awk '{print $1}')"
  else
    digest="$(shasum -a 256 "${archive}.tar.gz" | awk '{print $1}')"
  fi
  printf '%s  %s\n' "${digest}" "${archive}.tar.gz" > "${archive}.tar.gz.sha256"
)
