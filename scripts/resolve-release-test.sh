#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${ROOT}/scripts/resolve-release.sh"
# shellcheck source=scripts/resolve-release.sh
source "$SCRIPT"

fail=0
HEAD_SHA="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
OTHER_SHA="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
cleanup_dirs=()
trap 'rm -rf "${cleanup_dirs[@]}"' EXIT

assert_release() {
  local name="$1" expected_reason="$2" expected_tag="$3"
  shift 3
  local out reason tag release
  if ! out="$(decide_release "$@" 2>/dev/null)"; then
    echo "FAIL ${name} (unexpected error)"
    fail=1
    return
  fi
  reason="$(field reason "$out")"
  tag="$(field tag "$out")"
  release="$(field release "$out")"
  if [[ "$release" != true || "$reason" != "$expected_reason" || "$tag" != "$expected_tag" ]]; then
    echo "FAIL ${name}"
    printf '%s\n' "$out"
    fail=1
  else
    echo "ok ${name}"
  fi
}

assert_skip() {
  local name="$1" expected_reason="$2"
  shift 2
  local out reason release tag retarget
  if ! out="$(decide_release "$@" 2>/dev/null)"; then
    echo "FAIL ${name} (unexpected error)"
    fail=1
    return
  fi
  reason="$(field reason "$out")"
  release="$(field release "$out")"
  tag="$(field tag "$out")"
  retarget="$(field retarget "$out")"
  if [[ "$release" != false || "$reason" != "$expected_reason" || -n "$tag" || "$retarget" != false ]]; then
    echo "FAIL ${name}"
    printf '%s\n' "$out"
    fail=1
  else
    echo "ok ${name}"
  fi
}

assert_error() {
  local name="$1" needle="$2"
  shift 2
  local err
  err="$(mktemp)"
  if decide_release "$@" >/dev/null 2>"$err"; then
    echo "FAIL ${name} (expected error)"
    fail=1
  elif ! grep -F -q "$needle" "$err"; then
    echo "FAIL ${name} (stderr missing: ${needle})"
    cat "$err"
    fail=1
  else
    echo "ok ${name}"
  fi
  rm -f "$err"
}

assert_release "master patch bump" version_bump v0.1.1 \
  push refs/heads/master 0.1.1 v0.1.0 "" "" "$HEAD_SHA"
assert_release "master minor bump" version_bump v0.2.0 \
  push refs/heads/master 0.2.0 v0.1.0 "" "" "$HEAD_SHA"
assert_release "master ten over nine" version_bump v0.10.0 \
  push refs/heads/master 0.10.0 v0.9.0 "" "" "$HEAD_SHA"
assert_release "master patch ten" version_bump v0.1.10 \
  push refs/heads/master 0.1.10 v0.1.9 "" "" "$HEAD_SHA"
assert_release "master major bump" version_bump v1.0.0 \
  push refs/heads/master 1.0.0 v0.10.10 "" "" "$HEAD_SHA"
assert_release "tag push" tag_push v0.1.0 \
  push refs/tags/v0.1.0 0.1.0 "" "" "$HEAD_SHA" "$HEAD_SHA"
assert_release "manual default tag" manual v0.1.0 \
  workflow_dispatch refs/heads/master 0.1.0 "" "" "" "$HEAD_SHA"
assert_release "manual explicit tag" manual v0.2.0 \
  workflow_dispatch refs/heads/master 0.2.0 v0.1.0 v0.2.0 "" "$HEAD_SHA"
assert_release "rebuild when tag matches head" manual v0.1.0 \
  workflow_dispatch refs/heads/master 0.1.0 v0.1.0 "" "$HEAD_SHA" "$HEAD_SHA"
assert_release "manual moves existing tag" manual v0.1.0 \
  workflow_dispatch refs/heads/master 0.1.0 v0.1.0 "" "$OTHER_SHA" "$HEAD_SHA"

assert_release "first master push" master_push v0.1.0 \
  push refs/heads/master 0.1.0 "" "" "" "$HEAD_SHA"
assert_release "rebuild current tag" rebuild v0.1.0 \
  push refs/heads/master 0.1.0 v0.1.0 "" "$HEAD_SHA" "$HEAD_SHA"
assert_release "same version without recorded sha" master_push v0.1.0 \
  push refs/heads/master 0.1.0 v0.1.0 "" "" "$HEAD_SHA"
assert_release "same version on later commit" retarget v0.1.0 \
  push refs/heads/master 0.1.0 v0.1.0 "" "$OTHER_SHA" "$HEAD_SHA"
assert_skip "feature branch" ignored_ref \
  push refs/heads/feature 0.2.0 v0.1.0 "" "" "$HEAD_SHA"

first_out="$(decide_release push refs/heads/master 0.1.0 "" "" "" "$HEAD_SHA")"
if [[ "$(field release "$first_out")" != true ]] || ! field message "$first_out" | grep -F -q "自动发布"; then
  echo "FAIL first-release message"
  printf '%s\n' "$first_out"
  fail=1
else
  echo "ok first-release message"
fi

move_out="$(decide_release push refs/heads/master 0.1.0 v0.1.0 "" "$OTHER_SHA" "$HEAD_SHA")"
if [[ "$(field retarget "$move_out")" != true ]]; then
  echo "FAIL retarget flag"
  printf '%s\n' "$move_out"
  fail=1
else
  echo "ok retarget flag"
fi

manual_move="$(decide_release workflow_dispatch refs/heads/master 0.1.0 v0.1.0 "" "$OTHER_SHA" "$HEAD_SHA")"
if [[ "$(field release "$manual_move")" != true || "$(field reason "$manual_move")" != manual || "$(field retarget "$manual_move")" != true ]]; then
  echo "FAIL manual retarget flag"
  printf '%s\n' "$manual_move"
  fail=1
else
  echo "ok manual retarget flag"
fi

bump_out="$(decide_release push refs/heads/master 0.1.1 v0.1.0 "" "" "$HEAD_SHA")"
if [[ "$(field retarget "$bump_out")" != false || "$(field reason "$bump_out")" != version_bump ]]; then
  echo "FAIL version bump does not retarget"
  printf '%s\n' "$bump_out"
  fail=1
else
  echo "ok version bump does not retarget"
fi

assert_error "bad cargo version" "MAJOR.MINOR.PATCH" \
  push refs/heads/master 0.1 v0.1.0 "" "" "$HEAD_SHA"
assert_error "leading zero" "MAJOR.MINOR.PATCH" \
  push refs/heads/master 0.1.01 v0.1.0 "" "" "$HEAD_SHA"
assert_error "tag mismatch" "不一致" \
  push refs/tags/v0.1.0 0.2.0 "" "" "$HEAD_SHA" "$HEAD_SHA"
assert_error "non-semver tag" "非稳定版本标签" \
  push refs/tags/v0.1.0-rc.1 0.1.0 "" "" "$HEAD_SHA" "$HEAD_SHA"
assert_error "bad manual tag" "vX.Y.Z" \
  workflow_dispatch refs/heads/master 0.1.0 "" v1 "" "$HEAD_SHA"
assert_error "manual tag mismatch" "不一致" \
  workflow_dispatch refs/heads/master 0.1.0 "" v0.2.0 "" "$HEAD_SHA"
assert_error "manual from feature" "master" \
  workflow_dispatch refs/heads/feature 0.1.0 "" "" "" "$HEAD_SHA"
assert_error "older version" "低于" \
  push refs/heads/master 0.9.0 v1.0.0 "" "" "$HEAD_SHA"
assert_error "manual older version" "低于" \
  workflow_dispatch refs/heads/master 0.9.0 v1.0.0 "" "" "$HEAD_SHA"
assert_error "tag push older version" "低于" \
  push refs/tags/v0.9.0 0.9.0 v1.0.0 "" "$HEAD_SHA" "$HEAD_SHA"
assert_error "tag points elsewhere" "已指向" \
  push refs/tags/v0.1.0 0.1.0 "" "" "$OTHER_SHA" "$HEAD_SHA"
assert_error "empty head" "SHA 为空" \
  workflow_dispatch refs/heads/master 0.1.0 "" "" "" ""
assert_error "bad latest tag" "最新标签" \
  push refs/heads/master 0.2.0 v0.1 "" "" "$HEAD_SHA"

ver="$(read_cargo_version "${ROOT}/Cargo.toml")"
if ! is_semver "$ver"; then
  echo "FAIL repo Cargo.toml version: ${ver}"
  fail=1
else
  echo "ok repo Cargo.toml version ${ver}"
fi

fixture="$(mktemp)"
cleanup_dirs+=("$fixture")
cat >"$fixture" <<'EOF'
[workspace]
members = ["."]

[package]
name = "demo"
version = "1.2.3"
edition = "2024"

[dependencies]
serde = "1.0.210"

[package.metadata.release]
version = "9.9.9"
EOF
if [[ "$(read_cargo_version "$fixture")" != "1.2.3" ]]; then
  echo "FAIL read_cargo_version picked the wrong field"
  fail=1
else
  echo "ok read_cargo_version"
fi

missing="$(mktemp)"
cleanup_dirs+=("$missing")
printf '[package]\nname = "demo"\n' >"$missing"
if read_cargo_version "$missing" >/dev/null 2>&1; then
  echo "FAIL read_cargo_version accepted a missing version"
  fail=1
else
  echo "ok missing version"
fi

new_repo() {
  local dir
  dir="$(mktemp -d)"
  cleanup_dirs+=("$dir")
  git -C "$dir" init -b master >/dev/null
  git -C "$dir" config user.email "ci@example.com"
  git -C "$dir" config user.name "ci"
  git -C "$dir" config commit.gpgsign false
  printf '%s\n' "$dir"
}

write_version() {
  local dir="$1" version="$2"
  cat >"${dir}/Cargo.toml" <<EOF
[package]
name = "demo"
version = "${version}"
edition = "2024"
EOF
}

commit_all() {
  local dir="$1" message="$2"
  git -C "$dir" add Cargo.toml
  git -C "$dir" commit -m "$message" >/dev/null
}

run_plan() {
  local dir="$1" event="$2" ref="$3" out_file="$4"
  local sha
  sha="$(git -C "$dir" rev-parse HEAD)"
  (
    cd "$dir"
    env -u GITHUB_STEP_SUMMARY -u GIT_DIR -u GIT_WORK_TREE \
      RELEASE_EVENT="$event" \
      RELEASE_REF="$ref" \
      RELEASE_INPUT_TAG="" \
      RELEASE_HEAD_SHA="$sha" \
      GITHUB_OUTPUT="$out_file" \
      bash "$SCRIPT"
  )
}

repo="$(new_repo)"
write_version "$repo" "0.1.0"
commit_all "$repo" "init"
out="$(mktemp)"
cleanup_dirs+=("$out")
run_plan "$repo" push refs/heads/master "$out"
if [[ "$(field release "$(cat "$out")")" != true || "$(field reason "$(cat "$out")")" != master_push || "$(field tag "$(cat "$out")")" != v0.1.0 || "$(field retarget "$(cat "$out")")" != false ]]; then
  echo "FAIL git first push"
  cat "$out"
  fail=1
else
  echo "ok git first push"
fi

repo="$(new_repo)"
write_version "$repo" "0.10.0"
commit_all "$repo" "init"
git -C "$repo" tag v0.9.0
git -C "$repo" tag v0.10.0
git -C "$repo" commit --allow-empty -m "later" >/dev/null
: >"$out"
run_plan "$repo" push refs/heads/master "$out"
if [[ "$(field release "$(cat "$out")")" != true || "$(field reason "$(cat "$out")")" != retarget || "$(field tag "$(cat "$out")")" != v0.10.0 || "$(field retarget "$(cat "$out")")" != true ]]; then
  echo "FAIL git latest tag ordering"
  cat "$out"
  fail=1
else
  echo "ok git latest tag ordering"
fi

repo="$(new_repo)"
write_version "$repo" "0.1.0"
commit_all "$repo" "init"
git -C "$repo" tag v0.1.0
: >"$out"
run_plan "$repo" push refs/heads/master "$out"
if [[ "$(field release "$(cat "$out")")" != true || "$(field reason "$(cat "$out")")" != rebuild || "$(field tag "$(cat "$out")")" != v0.1.0 ]]; then
  echo "FAIL git rebuild"
  cat "$out"
  fail=1
else
  echo "ok git rebuild"
fi

repo="$(new_repo)"
write_version "$repo" "0.1.0"
commit_all "$repo" "init"
git -C "$repo" tag v0.1.0
write_version "$repo" "0.2.0"
commit_all "$repo" "bump"
: >"$out"
run_plan "$repo" push refs/heads/master "$out"
if [[ "$(field release "$(cat "$out")")" != true || "$(field tag "$(cat "$out")")" != v0.2.0 ]]; then
  echo "FAIL git version bump"
  cat "$out"
  fail=1
else
  echo "ok git version bump"
fi

repo="$(new_repo)"
write_version "$repo" "0.2.0"
commit_all "$repo" "init"
git -C "$repo" tag v0.1.0
: >"$out"
if (
  cd "$repo"
  env -u GITHUB_STEP_SUMMARY -u GIT_DIR -u GIT_WORK_TREE \
    RELEASE_EVENT=push \
    RELEASE_REF=refs/tags/v0.1.0 \
    RELEASE_INPUT_TAG="" \
    RELEASE_HEAD_SHA="$(git rev-parse HEAD)" \
    GITHUB_OUTPUT="$out" \
    bash "$SCRIPT" >/dev/null 2>"${out}.err"
); then
  echo "FAIL git tag mismatch should fail"
  fail=1
elif ! grep -F -q "不一致" "${out}.err"; then
  echo "FAIL git tag mismatch stderr"
  cat "${out}.err"
  fail=1
else
  echo "ok git tag mismatch"
fi
rm -f "${out}.err"

repo="$(new_repo)"
write_version "$repo" "0.1.0"
commit_all "$repo" "init"
git -C "$repo" tag v0.1.0
git -C "$repo" commit --allow-empty -m "later" >/dev/null
: >"$out"
if (
  cd "$repo"
  env -u GITHUB_STEP_SUMMARY -u GIT_DIR -u GIT_WORK_TREE \
    RELEASE_EVENT=workflow_dispatch \
    RELEASE_REF=refs/heads/master \
    RELEASE_INPUT_TAG="" \
    RELEASE_HEAD_SHA="$(git rev-parse HEAD)" \
    GITHUB_OUTPUT="$out" \
    bash "$SCRIPT" >/dev/null 2>"${out}.err"
); then
  if [[ "$(field release "$(cat "$out")")" != true || "$(field retarget "$(cat "$out")")" != true || "$(field tag "$(cat "$out")")" != v0.1.0 ]]; then
    echo "FAIL git manual retarget"
    cat "$out"
    fail=1
  else
    echo "ok git manual retarget"
  fi
else
  echo "FAIL git manual retarget should publish"
  cat "${out}.err"
  fail=1
fi
rm -f "${out}.err"

repo="$(new_repo)"
write_version "$repo" "0.1.0"
commit_all "$repo" "init"
git -C "$repo" tag v0.2.0
: >"$out"
if (
  cd "$repo"
  env -u GITHUB_STEP_SUMMARY -u GIT_DIR -u GIT_WORK_TREE \
    RELEASE_EVENT=push \
    RELEASE_REF=refs/heads/master \
    RELEASE_INPUT_TAG="" \
    RELEASE_HEAD_SHA="$(git rev-parse HEAD)" \
    GITHUB_OUTPUT="$out" \
    bash "$SCRIPT" >/dev/null 2>"${out}.err"
); then
  echo "FAIL git downgrade should fail"
  fail=1
elif ! grep -F -q "低于" "${out}.err"; then
  echo "FAIL git downgrade stderr"
  cat "${out}.err"
  fail=1
else
  echo "ok git downgrade"
fi
rm -f "${out}.err"

if [[ "$fail" -ne 0 ]]; then
  echo "release decision tests failed"
  exit 1
fi
echo "all release decision tests passed"
