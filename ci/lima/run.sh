#!/usr/bin/env bash
# Runs simjig's gate (or any command) on Linux aarch64, in the Lima VM `simjig`
# defined by ci/lima/simjig.yaml. See CONTRIBUTING.md ("Linux").
#
#   ci/lima/run.sh                                          # pixi run cargo xtask ci
#   ci/lima/run.sh pixi run -e conformance conformance-regen --check
#   ci/lima/run.sh pixi run cargo xtask miri
#
# It creates and starts the VM if needed, then syncs the host checkout's committed HEAD
# into a VM-local clone (~/simjig in the guest), so that target/ and .pixi/ live on the
# guest's disk, never on the read-only home mount. Uncommitted changes are not synced:
# what runs is exactly a commit, and the script says which.
#
# Requires limactl (Lima >= 2.0) on PATH. The VM keeps running afterwards; stop it with
# `limactl stop simjig`, remove it with `limactl delete simjig`.
set -euo pipefail

VM=${SIMJIG_LIMA_VM:-simjig}
# Pinned like the host: the pixi installer and rustup's channel are the only unlocked
# downloads; everything else comes from pixi.lock, rust-toolchain.toml and DEV_TOOLS.
PIXI_VERSION=${SIMJIG_PIXI_VERSION:-0.82.0}

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
case "$repo" in
  "$HOME"/*) ;;
  *) echo "error: the checkout must be under \$HOME ($HOME), which Lima mounts" >&2; exit 2 ;;
esac

command -v limactl >/dev/null || { echo "error: limactl not on PATH (https://lima-vm.io)" >&2; exit 2; }

status=$(limactl list --format '{{.Status}}' "$VM" 2>/dev/null || true)
if [ -z "$status" ]; then
  limactl create --name="$VM" --tty=false "$here/simjig.yaml"
  status=Stopped
fi
[ "$status" = Running ] || limactl start --tty=false "$VM"

rev=$(git -C "$repo" rev-parse HEAD)
if [ -n "$(git -C "$repo" status --porcelain)" ]; then
  echo "warning: uncommitted changes in $repo are not synced; running $rev" >&2
fi
echo "==> $VM: $(git -C "$repo" log -1 --format='%h %s' "$rev")" >&2

[ $# -gt 0 ] || set -- pixi run cargo xtask ci

# The guest script reads the repository path, the commit and the command as arguments.
limactl shell --workdir / "$VM" -- bash -s -- "$repo" "$rev" "$PIXI_VERSION" "$@" <<'GUEST'
set -euo pipefail
repo=$1 rev=$2 pixi_version=$3; shift 3
export PATH="$HOME/.pixi/bin:$HOME/.cargo/bin:$PATH" PIXI_NO_TELEMETRY=1

if [ "$(pixi --version 2>/dev/null)" != "pixi $pixi_version" ]; then
  curl -fsSL https://pixi.sh/install.sh |
    PIXI_VERSION="v$pixi_version" PIXI_NO_PATH_UPDATE=1 bash
fi
# rustup with no default toolchain: rust-toolchain.toml picks it on the first cargo run.
command -v rustup >/dev/null ||
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --default-toolchain none --profile minimal

clone=$HOME/simjig
[ -d "$clone/.git" ] || git clone -q --no-checkout "$repo" "$clone"
cd "$clone"
git fetch -q "$repo" "$rev"
git checkout -q --force --detach "$rev"
git clean -q -fdx -e target -e .pixi

pixi install --locked
pixi run cargo xtask setup
# `cargo xtask miri` runs on nightly, which rust-toolchain.toml does not install.
case "$*" in
  *miri*) rustup component add --toolchain nightly miri rust-src ;;
esac
echo "==> guest: $*" >&2
"$@"
GUEST
