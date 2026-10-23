#!/usr/bin/env bash
# Prove release archives use the names setup-maul downloads.
#
# Optional real binaries (local validation, not required in CI):
#   MAUL_REAL_X64_BINARY=/path/to/linux-x86_64/maul
#   MAUL_REAL_ARM64_BINARY=/path/to/linux-arm64/maul
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
install_sh="${root}/.github/actions/setup-maul/install.sh"
release_yml="${root}/.github/workflows/release.yml"
readme="${root}/README.md"
version="v0.1.0"

targets=(
  x86_64-unknown-linux-gnu
  aarch64-unknown-linux-gnu
  x86_64-apple-darwin
  aarch64-apple-darwin
)

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

for target in "${targets[@]}"; do
  for file in "${release_yml}" "${install_sh}" "${readme}"; do
    if ! grep -q -F "${target}" "${file}"; then
      fail "${file} does not mention ${target}"
    fi
  done
done

work="$(mktemp -d)"
server_pid=""
real_pid=""
cleanup() {
  if [[ -n "${server_pid}" ]]; then
    kill "${server_pid}" >/dev/null 2>&1 || true
  fi
  if [[ -n "${real_pid}" ]]; then
    kill "${real_pid}" >/dev/null 2>&1 || true
  fi
  rm -rf "${work}"
}
trap cleanup EXIT

dist="${work}/dist"
mkdir -p "${dist}"

for target in "${targets[@]}"; do
  stub="${work}/stub-${target}"
  printf '#!/bin/sh\necho "maul 0.1.0 %s"\n' "${target}" > "${stub}"
  chmod +x "${stub}"
  bash "${root}/ci/package_release.sh" "${target}" "${version}" "${stub}" "${dist}"
done

for target in "${targets[@]}"; do
  archive="maul-${version}-${target}.tar.gz"
  checksum="${dist}/${archive}.sha256"
  [[ -f "${dist}/${archive}" ]] || fail "missing ${archive}"
  [[ -f "${checksum}" ]] || fail "missing ${archive}.sha256"
  if grep -q '/' "${checksum}"; then
    fail "checksum for ${archive} contains a directory prefix"
  fi
  if ! grep -q -F "  ${archive}" "${checksum}"; then
    fail "checksum line for ${archive} is not a basename"
  fi
  (
    cd "${dist}"
    sha256sum -c "${archive}.sha256"
    shasum -a 256 -c "${archive}.sha256"
  )
  if ! tar -tzf "${dist}/${archive}" | grep -q -F "maul-${version}-${target}/maul"; then
    fail "archive ${archive} missing maul binary path"
  fi
done

# Publish verifies from inside dist/. A checksum that records dist/<file>
# (the previous `shasum -a 256 dist/<file>` output) does not match that layout.
prefixed_archive="maul-${version}-${targets[0]}.tar.gz"
prefixed_checksum="${work}/prefixed.sha256"
digest="$(awk '{print $1}' "${dist}/${prefixed_archive}.sha256")"
printf '%s  %s\n' "${digest}" "dist/${prefixed_archive}" > "${prefixed_checksum}"
if (cd "${dist}" && sha256sum -c "${prefixed_checksum}" >/dev/null 2>&1); then
  fail "directory-prefixed checksum unexpectedly verified"
fi

port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
python3 -m http.server "${port}" --bind 127.0.0.1 --directory "${dist}" >"${work}/http.log" 2>&1 &
server_pid=$!

ready=0
for _ in $(seq 1 50); do
  if curl -fsS "http://127.0.0.1:${port}/${prefixed_archive}" -o /dev/null 2>/dev/null; then
    ready=1
    break
  fi
  sleep 0.05
done
[[ "${ready}" -eq 1 ]] || fail "local release server did not start"

install_env() {
  local os="$1"
  local arch="$2"
  local tool_cache="$3"
  local tag="$4"
  mkdir -p "${work}/tmp-${tag}"
  env -u MAUL_BINARY_PATH -u GITHUB_TOKEN \
    MAUL_VERSION="${version}" \
    MAUL_RELEASE_BASE_URL="http://127.0.0.1:${port}" \
    RUNNER_OS="${os}" \
    RUNNER_ARCH="${arch}" \
    RUNNER_TEMP="${work}/tmp-${tag}" \
    RUNNER_TOOL_CACHE="${tool_cache}" \
    GITHUB_PATH="${work}/path-${tag}" \
    GITHUB_OUTPUT="${work}/output-${tag}" \
    bash "${install_sh}"
}

install_env Linux X64 "${work}/tools" linux-x64 >"${work}/linux-x64.log" 2>&1
grep -q '^version=v0.1.0$' "${work}/output-linux-x64" || fail "linux x64 did not record the version"
x64_path="$(sed -n 's/^maul-path=//p' "${work}/output-linux-x64")"
[[ -x "${x64_path}" ]] || fail "linux x64 maul-path is not executable"
if ! "${x64_path}" --version | grep -q -F "maul 0.1.0 x86_64-unknown-linux-gnu"; then
  fail "linux x64 installed the wrong archive"
fi

install_env Linux X64 "${work}/tools" linux-x64-cached >"${work}/cached.log" 2>&1
if ! grep -q "Using cached Maul ${version}" "${work}/cached.log"; then
  fail "cache hit did not skip the download"
fi

install_env Linux ARM64 "${work}/tools-arm" linux-arm >"${work}/linux-arm.log" 2>&1
arm_path="$(sed -n 's/^maul-path=//p' "${work}/output-linux-arm")"
if ! "${arm_path}" --version | grep -q -F "maul 0.1.0 aarch64-unknown-linux-gnu"; then
  fail "linux arm64 installed the wrong archive"
fi

install_env macOS X64 "${work}/tools-mac-x64" mac-x64 >"${work}/mac-x64.log" 2>&1
mac_x64_path="$(sed -n 's/^maul-path=//p' "${work}/output-mac-x64")"
if ! "${mac_x64_path}" --version | grep -q -F "maul 0.1.0 x86_64-apple-darwin"; then
  fail "macOS x64 installed the wrong archive"
fi

install_env macOS ARM64 "${work}/tools-mac-arm" mac-arm >"${work}/mac-arm.log" 2>&1
mac_arm_path="$(sed -n 's/^maul-path=//p' "${work}/output-mac-arm")"
if ! "${mac_arm_path}" --version | grep -q -F "maul 0.1.0 aarch64-apple-darwin"; then
  fail "macOS arm64 installed the wrong archive"
fi

reject() {
  local label="$1"
  shift
  if env -u MAUL_BINARY_PATH "$@" bash "${install_sh}" >"${work}/reject.log" 2>&1; then
    fail "${label} was accepted"
  fi
}

mkdir -p "${work}/tmp-reject"
reject "unpinned version" \
  MAUL_VERSION=latest \
  RUNNER_OS=Linux \
  RUNNER_ARCH=X64 \
  RUNNER_TEMP="${work}/tmp-reject" \
  RUNNER_TOOL_CACHE="${work}/tools-reject" \
  GITHUB_PATH="${work}/path-reject" \
  GITHUB_OUTPUT="${work}/output-reject"
reject "unsupported runner" \
  MAUL_VERSION="${version}" \
  RUNNER_OS=Windows \
  RUNNER_ARCH=X64 \
  RUNNER_TEMP="${work}/tmp-reject" \
  RUNNER_TOOL_CACHE="${work}/tools-reject" \
  GITHUB_PATH="${work}/path-reject" \
  GITHUB_OUTPUT="${work}/output-reject"

local_bin="${work}/local-maul"
printf '#!/bin/sh\necho maul local\n' > "${local_bin}"
chmod +x "${local_bin}"
mkdir -p "${work}/tmp-local"
env -u GITHUB_TOKEN \
  MAUL_BINARY_PATH="${local_bin}" \
  RUNNER_TEMP="${work}/tmp-local" \
  GITHUB_PATH="${work}/path-local" \
  GITHUB_OUTPUT="${work}/output-local" \
  bash "${install_sh}" >"${work}/local.log" 2>&1
grep -q '^version=local$' "${work}/output-local" || fail "binary-path install did not report local"

if [[ -n "${MAUL_REAL_X64_BINARY:-}" ]]; then
  real_dist="${work}/real-dist"
  mkdir -p "${real_dist}"
  bash "${root}/ci/package_release.sh" \
    x86_64-unknown-linux-gnu "${version}" "${MAUL_REAL_X64_BINARY}" "${real_dist}"
  real_port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
  python3 -m http.server "${real_port}" --bind 127.0.0.1 --directory "${real_dist}" >"${work}/real-http.log" 2>&1 &
  real_pid=$!
  for _ in $(seq 1 50); do
    if curl -fsS "http://127.0.0.1:${real_port}/maul-${version}-x86_64-unknown-linux-gnu.tar.gz" -o /dev/null 2>/dev/null; then
      break
    fi
    sleep 0.05
  done
  mkdir -p "${work}/tmp-real"
  env -u MAUL_BINARY_PATH -u GITHUB_TOKEN \
    MAUL_VERSION="${version}" \
    MAUL_RELEASE_BASE_URL="http://127.0.0.1:${real_port}" \
    RUNNER_OS=Linux \
    RUNNER_ARCH=X64 \
    RUNNER_TEMP="${work}/tmp-real" \
    RUNNER_TOOL_CACHE="${work}/tools-real" \
    GITHUB_PATH="${work}/path-real" \
    GITHUB_OUTPUT="${work}/output-real" \
    bash "${install_sh}" >"${work}/real.log" 2>&1
  real_path="$(sed -n 's/^maul-path=//p' "${work}/output-real")"
  "${real_path}" --version >"${work}/real-version.txt" 2>"${work}/real-version.err"
  if ! grep -q "maul 0.1.0" "${work}/real-version.txt"; then
    fail "real binary --version did not report 0.1.0"
  fi
  echo "real x86_64 binary installed and reported: $(cat "${work}/real-version.txt")"
fi

if [[ -n "${MAUL_REAL_ARM64_BINARY:-}" ]]; then
  arm_dist="${work}/arm-dist"
  mkdir -p "${arm_dist}"
  bash "${root}/ci/package_release.sh" \
    aarch64-unknown-linux-gnu "${version}" "${MAUL_REAL_ARM64_BINARY}" "${arm_dist}"
  extracted="${work}/arm-maul"
  tar -xOf "${arm_dist}/maul-${version}-aarch64-unknown-linux-gnu.tar.gz" \
    "maul-${version}-aarch64-unknown-linux-gnu/maul" > "${extracted}"
  if ! file "${extracted}" | grep -q "ARM aarch64"; then
    fail "arm64 archive does not contain an aarch64 binary"
  fi
  echo "real arm64 archive contains: $(file "${extracted}")"
fi

echo "release archive contract ok (${version}, ${#targets[@]} targets)"
