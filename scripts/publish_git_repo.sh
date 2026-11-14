#!/usr/bin/env bash
# Clone, update, commit, and push manifest files to a package manager Git repository.
set -euo pipefail

remote=""
default_branch="main"
push_branch=""
message=""
author_name=""
author_email=""
mappings=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --remote) remote="$2"; shift 2 ;;
    --branch) default_branch="$2"; shift 2 ;;
    --push-branch) push_branch="$2"; shift 2 ;;
    --message) message="$2"; shift 2 ;;
    --author-name) author_name="$2"; shift 2 ;;
    --author-email) author_email="$2"; shift 2 ;;
    --) shift; mappings+=("$@"); break ;;
    -*) echo "未知参数：$1" >&2; exit 2 ;;
    *) mappings+=("$1"); shift ;;
  esac
done

if [[ -z "$remote" || -z "$message" || -z "$author_name" || -z "$author_email" || ${#mappings[@]} -eq 0 ]]; then
  echo "用法：publish_git_repo.sh --remote <url> --message <msg> --author-name <name> --author-email <email> <src:dst>..." >&2
  exit 2
fi

workdir="$(mktemp -d)"
askpass=""
cleanup() {
  rm -rf "$workdir"
  if [[ -n "$askpass" ]]; then
    rm -f "$askpass"
  fi
}
trap cleanup EXIT

if [[ -n "${GIT_HTTP_TOKEN:-}" ]]; then
  askpass="$(mktemp)"
  chmod 700 "$askpass"
  cat > "$askpass" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  *Username*) printf '%s\n' "x-access-token" ;;
  *Password*) printf '%s\n' "$GIT_HTTP_TOKEN" ;;
  *) printf '\n' ;;
esac
EOF
  export GIT_ASKPASS="$askpass"
  export GIT_TERMINAL_PROMPT=0
fi

repo_dir="$workdir/repo"
git clone --depth 1 "$remote" "$repo_dir"
cd "$repo_dir"

git config user.name "$author_name"
git config user.email "$author_email"

if git rev-parse --verify HEAD >/dev/null 2>&1; then
  current_branch="$(git rev-parse --abbrev-ref HEAD)"
  target_branch="${push_branch:-$current_branch}"
else
  target_branch="${push_branch:-$default_branch}"
  git checkout -B "$target_branch"
fi

for mapping in "${mappings[@]}"; do
  src="${mapping%%:*}"
  dst="${mapping#*:}"
  if [[ -z "$src" || -z "$dst" || "$src" == "$mapping" ]]; then
    echo "无效的文件映射：$mapping（需要 <源路径>:<目标相对路径>）" >&2
    exit 2
  fi
  mkdir -p "$(dirname "$dst")"
  cp "$src" "$dst"
done

git add --all
if git diff --cached --quiet; then
  echo "清单内容未变化，跳过提交与推送。"
  exit 0
fi

git commit -m "$message"
git push -u origin "HEAD:${target_branch}"
