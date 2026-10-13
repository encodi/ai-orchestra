# Shared by the fake agent CLIs (sourced). Scenarios are chosen by marker words in the question:
#   FAIL_<AGENT>   that agent exits with an error      SLOW_<AGENT>   that agent sleeps 30 s
#   SLEEPALL       every agent sleeps 30 s             DISAGREE       the answers list an unresolved disagreement
# Nothing here contacts a model or the network.
# The caller sets AGENT and PROMPT before sourcing this file (POSIX `.` takes no arguments; dash ignores them).
agent="$AGENT"; prompt="$PROMPT"
[ -n "$FAKE_LOG" ] && echo "$agent" >> "$FAKE_LOG"
[ -n "$FAKE_PROMPTS" ] && { printf '=== %s ===\n%s\n' "$agent" "$prompt" >> "$FAKE_PROMPTS"; }
upper=$(printf '%s' "$agent" | tr 'a-z' 'A-Z')
case "$prompt" in
  *"SLEEPALL"*) sleep 30; exit 0;;
  *"SLOW_$upper"*) sleep 30; exit 0;;
  *"FAIL_$upper"*) echo "simulated failure of $agent" >&2; exit 1;;
esac
sleep "${FAKE_DELAY:-0.6}"
dis="None: the agents agree."
case "$prompt" in *DISAGREE*) dis="- codex wants a professional model; claude prefers the entry model. Unresolved.";; esac
n=$(printf '%s' "$prompt" | wc -c | tr -d ' ')
case "$prompt" in
  *"short summary of the report"*) BODY="Bottom line: choose the entry model.\n\n- Price matters most.\n- Nothing was verified externally.";;
  *) BODY="## Answer\n\n$agent's answer after reading $n characters of prompt.\n\n## Recommendation\n\nChoose the entry model; **$agent** considers it the best value.\n\n| Option | Price |\n|---|---|\n| Entry | 250 |\n| Pro | 700 |\n\n## Areas of agreement\n\nPrice matters most.\n\n## Significant disagreements\n\n$dis\n\n## Assumptions\n\nBudget under 300.\n\n## Evidence limitations\n\nNothing was verified externally.\n"
;;
esac
