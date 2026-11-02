#!/usr/bin/env bash
# Decide whether this workflow run publishes a GitHub Release.
# Stable versions are MAJOR.MINOR.PATCH without leading zeros.
set -euo pipefail

SEMVER_RE='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
TAG_RE='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'

is_semver() {
  [[ "$1" =~ $SEMVER_RE ]]
}

is_tag() {
  [[ "$1" =~ $TAG_RE ]]
}

ensure_not_downgrade() {
  local cargo="$1"
  local latest="$2"
  if [[ -z "$latest" ]]; then
    return 0
  fi
  if [[ "$(semver_cmp "$cargo" "${latest#v}")" == -1 ]]; then
    echo "版本 ${cargo} 低于最新稳定标签 ${latest}，拒绝发布。" >&2
    return 1
  fi
}

semver_cmp() {
  local a="$1" b="$2"
  local -a aa bb
  local i x y
  IFS=. read -r -a aa <<<"$a"
  IFS=. read -r -a bb <<<"$b"
  for i in 0 1 2; do
    x="${aa[$i]}"
    y="${bb[$i]}"
    x=$((10#$x))
    y=$((10#$y))
    if ((x < y)); then
      printf '%s\n' -1
      return 0
    fi
    if ((x > y)); then
      printf '%s\n' 1
      return 0
    fi
  done
  printf '%s\n' 0
}

read_cargo_version() {
  local file="$1"
  local ver
  ver="$(
    awk '
      /^\[package\][[:space:]]*$/ { in_pkg = 1; next }
      /^\[/ { in_pkg = 0 }
      in_pkg && /^version[[:space:]]*=/ {
        line = $0
        sub(/\r$/, "", line)
        sub(/^version[[:space:]]*=[[:space:]]*"/, "", line)
        sub(/".*$/, "", line)
        print line
        exit
      }
    ' "$file"
  )"
  if [[ -z "$ver" ]]; then
    echo "在 $file 中找不到 [package] version" >&2
    return 1
  fi
  printf '%s\n' "$ver"
}

field() {
  local key="$1"
  local data="$2"
  local line
  line="$(printf '%s\n' "$data" | grep -E "^${key}=")"
  printf '%s' "${line#"${key}="}"
}

decide_release() {
  local event="$1"
  local ref="$2"
  local cargo="$3"
  local latest="$4"
  local input="$5"
  local tag_sha="$6"
  local head="$7"
  local tag="" reason="" message="" release=false version="$cargo" cmp retarget=false

  if ! is_semver "$cargo"; then
    echo "Cargo.toml version 不是不含前导零的 MAJOR.MINOR.PATCH: $cargo" >&2
    return 1
  fi
  if [[ -n "$latest" ]] && ! is_tag "$latest"; then
    echo "最新标签不是稳定版本标签: $latest" >&2
    return 1
  fi
  if [[ -n "$input" ]] && ! is_tag "$input"; then
    echo "手动输入的标签不是 vX.Y.Z: $input" >&2
    return 1
  fi

  case "$event" in
    workflow_dispatch)
      if [[ "$ref" != "refs/heads/master" ]]; then
        echo "手动发布只能从 master 运行，当前引用是 $ref" >&2
        return 1
      fi
      if [[ -n "$input" ]]; then
        tag="$input"
      else
        tag="v${cargo}"
      fi
      if [[ "${tag#v}" != "$cargo" ]]; then
        echo "标签 $tag 与 Cargo.toml 版本 $cargo 不一致" >&2
        return 1
      fi
      ensure_not_downgrade "$cargo" "$latest" || return 1
      reason="manual"
      message="手动发布 ${tag}。"
      release=true
      ;;
    push)
      if [[ "$ref" == refs/tags/* ]]; then
        tag="${ref#refs/tags/}"
        if ! is_tag "$tag"; then
          echo "拒绝非稳定版本标签: $tag" >&2
          return 1
        fi
        if [[ "${tag#v}" != "$cargo" ]]; then
          echo "标签 $tag 与 Cargo.toml 版本 $cargo 不一致" >&2
          return 1
        fi
        ensure_not_downgrade "$cargo" "$latest" || return 1
        reason="tag_push"
        message="推送标签 ${tag}，开始发布。"
        release=true
      elif [[ "$ref" == "refs/heads/master" ]]; then
        tag="v${cargo}"
        ensure_not_downgrade "$cargo" "$latest" || return 1
        if [[ -n "$latest" ]]; then
          cmp="$(semver_cmp "$cargo" "${latest#v}")"
        else
          cmp=1
        fi
        if [[ -n "$tag_sha" && "$tag_sha" == "$head" ]]; then
          reason="rebuild"
          message="版本 ${cargo} 的标签已指向当前提交，重新发布 ${tag}。"
        elif [[ -n "$tag_sha" ]]; then
          reason="retarget"
          message="将标签 ${tag} 移动到当前提交并发布。"
        elif [[ "$cmp" == 1 && -n "$latest" ]]; then
          reason="version_bump"
          message="版本 ${cargo} 高于最新标签 ${latest}，自动发布 ${tag}。"
        else
          reason="master_push"
          message="推送 master，自动发布 ${tag}。"
        fi
        release=true
      else
        reason="ignored_ref"
        message="引用 ${ref} 不触发发布。"
        release=false
      fi
      ;;
    *)
      echo "不支持的事件: $event" >&2
      return 1
      ;;
  esac

  if [[ "$release" == true ]]; then
    if ! is_tag "$tag"; then
      echo "拒绝输出非法标签: $tag" >&2
      return 1
    fi
    if [[ -z "$head" ]]; then
      echo "当前提交 SHA 为空" >&2
      return 1
    fi
    if [[ -n "$tag_sha" && "$tag_sha" != "$head" ]]; then
      if [[ "$event" == "push" && "$ref" == refs/tags/* ]]; then
        echo "标签 $tag 已指向 $tag_sha，而不是 $head" >&2
        return 1
      fi
      retarget=true
    fi
  fi

  printf 'release=%s\n' "$release"
  printf 'tag=%s\n' "$tag"
  printf 'version=%s\n' "$version"
  printf 'reason=%s\n' "$reason"
  printf 'retarget=%s\n' "$retarget"
  printf 'message=%s\n' "$message"
}

decide_from_env() {
  : "${RELEASE_EVENT:?}"
  : "${RELEASE_REF:?}"
  : "${RELEASE_CARGO_VERSION:?}"
  : "${RELEASE_HEAD_SHA:?}"
  decide_release \
    "$RELEASE_EVENT" \
    "$RELEASE_REF" \
    "$RELEASE_CARGO_VERSION" \
    "${RELEASE_LATEST_TAG:-}" \
    "${RELEASE_INPUT_TAG:-}" \
    "${RELEASE_TAG_SHA:-}" \
    "$RELEASE_HEAD_SHA"
}

write_outputs() {
  local out="$1"
  local release tag version message retarget
  printf '%s\n' "$out"
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    printf '%s\n' "$out" >>"$GITHUB_OUTPUT"
  fi
  release="$(field release "$out")"
  tag="$(field tag "$out")"
  version="$(field version "$out")"
  message="$(field message "$out")"
  retarget="$(field retarget "$out")"
  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then
    echo "::notice title=发布计划::${message}"
  fi
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    {
      echo "## 发布计划"
      echo
      if [[ "$release" == true ]]; then
        echo "- 结果: 发布"
      else
        echo "- 结果: 跳过"
      fi
      echo "- 版本: ${version}"
      if [[ -n "$tag" ]]; then
        echo "- 标签: ${tag}"
      else
        echo "- 标签: （无）"
      fi
      if [[ "$retarget" == true ]]; then
        echo "- 移动标签: 是"
      else
        echo "- 移动标签: 否"
      fi
      echo "- 说明: ${message}"
    } >>"$GITHUB_STEP_SUMMARY"
  fi
}

latest_stable_tag() {
  git tag -l | grep -E "$TAG_RE" | sort -V | tail -n 1 || true
}

plan_release() {
  local cargo latest out
  cargo="$(read_cargo_version Cargo.toml)"
  if git remote get-url origin >/dev/null 2>&1; then
    GIT_TERMINAL_PROMPT=0 git fetch --tags --force --quiet origin
  fi
  latest="$(latest_stable_tag)"
  export RELEASE_CARGO_VERSION="$cargo"
  export RELEASE_LATEST_TAG="$latest"
  RELEASE_TAG_SHA=""
  if is_semver "$cargo" && git rev-parse -q --verify "refs/tags/v${cargo}" >/dev/null; then
    RELEASE_TAG_SHA="$(git rev-parse "refs/tags/v${cargo}^{}")"
  fi
  export RELEASE_TAG_SHA
  if ! out="$(decide_from_env)"; then
    return 1
  fi
  write_outputs "$out"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  plan_release
fi
