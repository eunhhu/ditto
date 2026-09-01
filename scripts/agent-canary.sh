#!/usr/bin/env bash
set -euo pipefail

while IFS= read -r -d '' tracked; do
  case "/${tracked}" in
    */.omo/*|*/.surf/*|*/target/*|*.db|*.db-wal|*.db-shm|*-wal|*-shm|*/.env)
      echo "forbidden generated or sensitive artifact is tracked: ${tracked}" >&2
      exit 1
      ;;
  esac
done < <(git ls-files -z)

if git grep -nE '(/Users/|/home/[^ /]+/|[A-Za-z]:\\Users\\)' -- . \
  ':(exclude)scripts/agent-canary.sh'
then
  echo "tracked source contains a developer-specific absolute path" >&2
  exit 1
fi

credential_pattern='(-----BEGIN ([A-Z ]+ )?PRIVATE KEY-----|AKIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9]{30,}|xox[baprs]-[A-Za-z0-9-]{20,}|sk-[A-Za-z0-9]{20,})'
if git grep -nE "${credential_pattern}" -- . \
  ':(exclude)scripts/agent-canary.sh'
then
  echo "tracked source contains credential-shaped material" >&2
  exit 1
fi

echo "agent canaries passed"
