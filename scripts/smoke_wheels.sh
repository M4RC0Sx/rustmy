#!/usr/bin/env bash
# Install the wheel built for each Python into a fresh environment and run
# scripts/smoke_test.py with it (.github/workflows/wheels.yml calls this on
# every platform, Windows included, through bash).
#
#   scripts/smoke_wheels.sh DIST_DIR "3.10 3.11 ... 3.15t"
#
# uv downloads each interpreter (its own builds, so free-threaded and musl ones
# too) and installs rustmy from DIST_DIR only, never from PyPI. The test
# environment comes from RUSTMY_SMOKE_* variables (see smoke_test.py).
set -euo pipefail

# Absolute paths: each test runs from a temporary directory. On Windows (Git
# Bash) they are converted to native ones for uv.
dist="$(cd "$1" && pwd)"
pythons=$2
script="$(cd "$(dirname "$0")" && pwd)/smoke_test.py"
if command -v cygpath >/dev/null; then
	dist="$(cygpath -m "$dist")"
	script="$(cygpath -m "$script")"
fi
export UV_PYTHON_PREFERENCE=only-managed

for python in $pythons; do
	echo "::group::Python $python"
	# --isolated and a temporary directory: no project, no source tree on sys.path.
	(cd "$(mktemp -d)" && uv run --isolated --no-project --python "$python" \
		--with rustmy --no-index --find-links "$dist" python "$script")
	echo "::endgroup::"
done
