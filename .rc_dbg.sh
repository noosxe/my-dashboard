export DASHBOARD_GITHUB_FIXTURE=/work/crates/debug/verify/fixtures/github DASHBOARD_FIXED_NOW=2026-10-23T12:00:00Z DASHBOARD_GITHUB_USAGE_SEED=90 DASHBOARD_GITHUB_FIXTURE_RATE=graphql-exhausted
D="cargo run -q -p dashboard-debug --"
$D session start --name paused --size 1280x800 --binary /tmp/target/debug/my-dashboard >/dev/null 2>&1
$D run crates/debug/verify/.tmp-paused.txt 2>&1 | tail -4
sleep 3; $D --session paused tree | grep -i "Refresh\|requests ·\|Status"
sleep 3; $D --session paused tree | grep -i "Refresh\|Status"; for i in 1 2; do $D --session paused click 87898890429551502 --wait-frame; sleep 2; $D --session paused tree | grep -i "requests ·"; done
$D session stop paused >/dev/null 2>&1
