#!/bin/sh
set -e

REPO="myersm0/montre"
BIN_DIR="${HOME}/.local/bin"
LIB_DIR="${HOME}/.local/lib"

info() { printf "\033[0;34m%s\033[0m\n" "$*"; }
err()  { printf "\033[0;31m%s\033[0m\n" "$*" >&2; exit 1; }

detect_platform() {
	os="$(uname -s)"
	arch="$(uname -m)"

	case "$os" in
		Linux)  os="linux"; lib="libmontre_ffi.so" ;;
		Darwin) os="macos"; lib="libmontre_ffi.dylib" ;;
		*)      err "Unsupported OS: $os" ;;
	esac

	case "$arch" in
		x86_64|amd64)  arch="x86_64" ;;
		arm64|aarch64) arch="aarch64" ;;
		*)             err "Unsupported architecture: $arch" ;;
	esac

	ARTIFACT="montre-${os}-${arch}"
	LIB_NAME="$lib"
}

main() {
	detect_platform
	info "Detected platform: ${ARTIFACT}"

	url="https://github.com/${REPO}/releases/latest/download/${ARTIFACT}.tar.gz"
	info "Downloading ${url}..."

	tmpdir="$(mktemp -d)"
	trap 'rm -rf "$tmpdir"' EXIT

	if command -v curl >/dev/null 2>&1; then
		curl -fsSL "$url" -o "${tmpdir}/montre.tar.gz"
	elif command -v wget >/dev/null 2>&1; then
		wget -qO "${tmpdir}/montre.tar.gz" "$url"
	else
		err "Neither curl nor wget found."
	fi

	tar xzf "${tmpdir}/montre.tar.gz" -C "$tmpdir"

	mkdir -p "$BIN_DIR"
	cp "${tmpdir}/${ARTIFACT}/montre" "${BIN_DIR}/montre"
	chmod +x "${BIN_DIR}/montre"
	info "Installed CLI to ${BIN_DIR}/montre"

	mkdir -p "$LIB_DIR"
	cp "${tmpdir}/${ARTIFACT}/${LIB_NAME}" "${LIB_DIR}/${LIB_NAME}"
	info "Installed library to ${LIB_DIR}/${LIB_NAME}"

	echo ""
	case ":$PATH:" in
		*":${BIN_DIR}:"*)
			;;
		*)
			info "Add to your shell profile:"
			echo ""
			echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
			echo "" ;;
	esac

	info "Done. Try: montre --help"
}

main
