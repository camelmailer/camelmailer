#!/usr/bin/env sh
# Import the transactional template library into a CamelMailer server.
#
#   ./templates/import.sh https://mail.yourdomain.com $SERVER_API_KEY [name…]
#
# Without names, every template in library/ is imported. Templates that
# already exist (same permalink) are skipped — delete or archive them
# first if you want a fresh copy.
set -eu

BASE_URL="${1:?usage: import.sh <base-url> <server-api-key> [template…]}"
API_KEY="${2:?usage: import.sh <base-url> <server-api-key> [template…]}"
shift 2

DIR="$(dirname "$0")/library"
if [ "$#" -gt 0 ]; then
  FILES=""
  for name in "$@"; do FILES="$FILES $DIR/$name.json"; done
else
  FILES="$DIR"/*.json
fi

imported=0
skipped=0
for file in $FILES; do
  name="$(basename "$file" .json)"
  # send only the fields the API accepts (name/subject/html_body/text_body)
  payload="$(python3 -c '
import json, sys
t = json.load(open(sys.argv[1]))
print(json.dumps({k: t[k] for k in ("name", "subject", "html_body", "text_body") if k in t}))
' "$file")"
  response="$(curl -sS -X POST "$BASE_URL/api/v2/server/templates" \
    -H "X-Server-API-Key: $API_KEY" -H "Content-Type: application/json" \
    -d "$payload")"
  status="$(printf '%s' "$response" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status",""))')"
  if [ "$status" = "success" ]; then
    echo "imported  $name"
    imported=$((imported + 1))
  else
    message="$(printf '%s' "$response" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("error",{}).get("message",""))')"
    echo "skipped   $name ($message)"
    skipped=$((skipped + 1))
  fi
done
echo "done: $imported imported, $skipped skipped"
