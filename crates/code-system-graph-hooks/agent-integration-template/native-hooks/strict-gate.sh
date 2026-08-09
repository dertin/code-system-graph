{{BEGIN_MARKER}}
CODE_SYSTEM_GRAPH_RESULT="$({{BINARY}} changes --scope staged --database {{DATABASE}} --workspace {{WORKSPACE}} --repository {{REPOSITORY}})" || {
  echo "Code System Graph staged-change analysis failed; commit blocked by strict mode." >&2
  exit 1
}
CODE_SYSTEM_GRAPH_FINGERPRINT="$(printf '%s' "$CODE_SYSTEM_GRAPH_RESULT" | tr -d '\n' | sed -n 's/.*"exact_diff_fingerprint":"\([^"]*\)".*/\1/p')"
if [ -z "$CODE_SYSTEM_GRAPH_FINGERPRINT" ]; then
  echo "Code System Graph returned no exact staged fingerprint; commit blocked by strict mode." >&2
  exit 1
fi
unset CODE_SYSTEM_GRAPH_RESULT CODE_SYSTEM_GRAPH_FINGERPRINT
{{END_MARKER}}
