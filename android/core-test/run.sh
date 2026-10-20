#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Unit tests of the pure-Java core (org.arca.viewer.core) on a desktop JVM.
set -euo pipefail
cd "$(dirname "$0")/../.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
javac -Xlint:all -Werror -d "$work" android/app/src/main/java/org/arca/viewer/core/*.java android/core-test/CoreTest.java
java -cp "$work" CoreTest
