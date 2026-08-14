#!/usr/bin/env bash
# Download or reuse a Maul binary. Used by setup-maul and maul-test.
set -euo pipefail

SUPPORTED_ERROR="Maul GitHub Action supports Linux x86_64 and macOS arm64."

resolve_target() {
  local os="${RUNNER_OS:-}"
  local arch="${RUNNER_ARCH:-}"
  case "${os}:${arch}" in
    Linux:X64) echo "x86_64-unknown-linux-gnu" ;;
    macOS:ARM64) echo "aarch64-apple-darwin" ;;
    *)
      echo "::error::${SUPPORTED_ERROR} This runner is ${os}/${arch}."
      exit 1
      ;;
  esac
}

add_to_path() {
  local install_dir="$1"
  mkdir -p "${install_dir}"
  echo "${install_dir}" >> "${GITHUB_PATH}"
}

write_outputs() {
  local maul_path="$1"
  local version="$2"
  {
    echo "maul-path=${maul_path}"
    echo "version=${version}"
  } >> "${GITHUB_OUTPUT}"
}

install_from_path() {
  local source="$1"
  local install_dir="${RUNNER_TEMP}/maul-local"
  mkdir -p "${install_dir}"
  if [[ -d "${source}" ]]; then
    source="${source%/}/maul"
  fi
  if [[ ! -x "${source}" && ! -f "${source}" ]]; then
    echo "::error::binary-path '${source}' does not exist."
    exit 1
  fi
  cp "${source}" "${install_dir}/maul"
  chmod +x "${install_dir}/maul"
  add_to_path "${install_dir}"
  write_outputs "${install_dir}/maul" "local"
  "${install_dir}/maul" --version
}

install_from_release() {
  local version="${MAUL_VERSION:-}"
  if [[ -z "${version}" ]]; then
    version="${GITHUB_ACTION_REF:-}"
  fi
  if [[ -z "${version}" || "${version}" == "latest" || "${version}" == refs/* ]]; then
    echo "::error::Pin a Maul release tag (for example v0.1.0). Mutable 'latest' is not supported."
    exit 1
  fi

  local target
  target="$(resolve_target)"
  local archive="maul-${version}-${target}.tar.gz"
  local tool_dir="${RUNNER_TOOL_CACHE}/maul/${version}/${target}"
  local binary="${tool_dir}/maul"

  if [[ -x "${binary}" ]]; then
    echo "Using cached Maul ${version} (${target})"
    add_to_path "${tool_dir}"
    write_outputs "${binary}" "${version}"
    return 0
  fi

  local repo="${GITHUB_ACTION_REPOSITORY:-invariant-sh/maul}"
  local base="https://github.com/${repo}/releases/download/${version}"
  local work
  work="$(mktemp -d)"
  local auth=()
  if [[ -n "${GITHUB_TOKEN:-}" ]]; then
    auth=(-H "Authorization: Bearer ${GITHUB_TOKEN}")
  fi

  echo "Downloading ${archive}"
  curl -fsSL "${auth[@]}" -o "${work}/${archive}" "${base}/${archive}"
  curl -fsSL "${auth[@]}" -o "${work}/${archive}.sha256" "${base}/${archive}.sha256"

  (
    cd "${work}"
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum -c "${archive}.sha256"
    else
      shasum -a 256 -c "${archive}.sha256"
    fi
  )

  tar -xzf "${work}/${archive}" -C "${work}"
  local extracted="${work}/maul-${version}-${target}/maul"
  if [[ ! -f "${extracted}" ]]; then
    shopt -s nullglob
    local candidates=( "${work}"/*/maul "${work}/maul" )
    extracted="${candidates[0]:-}"
    shopt -u nullglob
  fi
  if [[ -z "${extracted}" ]]; then
    echo "::error::Release archive ${archive} did not contain a maul binary."
    exit 1
  fi

  mkdir -p "${tool_dir}"
  cp "${extracted}" "${binary}"
  chmod +x "${binary}"
  add_to_path "${tool_dir}"
  write_outputs "${binary}" "${version}"
  "${binary}" --version
}

if [[ -n "${MAUL_BINARY_PATH:-}" ]]; then
  install_from_path "${MAUL_BINARY_PATH}"
else
  install_from_release
fi
