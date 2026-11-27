#!/usr/bin/env bash
# Upload the installer again under a name that never changes, so
# releases/latest/download/<name> links keep working for every version.
set -euxo pipefail

if [ "$RUNNER_OS" = "Windows" ]; then
  FILE=$(find src-tauri/target -path '*bundle/nsis/*-setup.exe' | head -1)
  NAME=Tapline-Windows-Setup.exe
else
  FILE=$(find src-tauri/target -path '*bundle/dmg/*.dmg' | head -1)
  NAME=Tapline-Mac.dmg
fi
if [ -z "$FILE" ]; then
  echo "No installer found"; find src-tauri/target -maxdepth 6 -path '*bundle*' | head -50; exit 1
fi

API="https://api.github.com/repos/$REPO"
AUTH="Authorization: Bearer $GITHUB_TOKEN"
json() { node -e "let d='';process.stdin.on('data',c=>d+=c).on('end',()=>{const j=JSON.parse(d);$1})" "${@:2}"; }

# Find the release if the build step didn't report it
if [ -z "${RELEASE_ID:-}" ]; then
  VERSION=$(node -p "require('./src-tauri/tauri.conf.json').version")
  RELEASE_ID=$(curl -fsS -H "$AUTH" "$API/releases?per_page=30" | json "const r=j.find(x=>x.tag_name==='v'+process.argv[1]);console.log(r?r.id:'')" "$VERSION")
fi
[ -n "$RELEASE_ID" ] || { echo "Release not found"; exit 1; }

OLD=$(curl -fsS -H "$AUTH" "$API/releases/$RELEASE_ID/assets?per_page=100" | json "const a=j.find(x=>x.name===process.argv[1]);console.log(a?a.id:'')" "$NAME")
if [ -n "$OLD" ]; then curl -fsS -X DELETE -H "$AUTH" "$API/releases/assets/$OLD"; fi

curl -fsS -X POST -H "$AUTH" -H "Content-Type: application/octet-stream" \
  --data-binary @"$FILE" "https://uploads.github.com/repos/$REPO/releases/$RELEASE_ID/assets?name=$NAME" -o /dev/null -w "upload HTTP %{http_code}\n"
echo "Uploaded $FILE as $NAME"
