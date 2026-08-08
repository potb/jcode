#!/usr/bin/env bash
# Install (or remove) a recurring schedule for the upstream-merge agent.
#
#   Linux : systemd user timer  (~/.config/systemd/user/)
#   macOS : launchd user agent  (~/Library/LaunchAgents/)
#   other : prints a crontab line you can paste
#
# Usage:
#   scripts/install_upstream_merge_schedule.sh [--interval-hours N] [--repo DIR]
#   scripts/install_upstream_merge_schedule.sh --uninstall
#   scripts/install_upstream_merge_schedule.sh --status
#   scripts/install_upstream_merge_schedule.sh --run-now

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AGENT="$SCRIPT_DIR/upstream_merge_agent.sh"
LABEL="jcode-upstream-merge"
INTERVAL_HOURS=6
REPO="$(cd "$SCRIPT_DIR/.." && pwd)"
MODE="install"

while [ $# -gt 0 ]; do
  case "$1" in
    --interval-hours) INTERVAL_HOURS="$2"; shift 2 ;;
    --repo) REPO="$2"; shift 2 ;;
    --uninstall) MODE="uninstall"; shift ;;
    --status) MODE="status"; shift ;;
    --run-now) MODE="run-now"; shift ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1"; exit 2 ;;
  esac
done

chmod +x "$AGENT" 2>/dev/null

JCODE_BIN="${JCODE_BIN:-}"
if [ -z "$JCODE_BIN" ]; then
  if [ -x "$HOME/.local/bin/jcode" ]; then JCODE_BIN="$HOME/.local/bin/jcode";
  else JCODE_BIN="$(command -v jcode 2>/dev/null)"; fi
fi

OS="$(uname -s)"
INTERVAL_SECS=$((INTERVAL_HOURS * 3600))

# --- Linux / systemd ---------------------------------------------------------
install_systemd() {
  local dir="$HOME/.config/systemd/user"
  mkdir -p "$dir"
  cat > "$dir/$LABEL.service" <<EOF
[Unit]
Description=jcode upstream merge agent (keeps this fork mergeable with upstream)

[Service]
Type=oneshot
Environment=JCODE_UPSTREAM_REPO=$REPO
Environment=JCODE_BIN=$JCODE_BIN
Environment=PATH=$HOME/.local/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin
ExecStart=$AGENT
Nice=10
EOF
  cat > "$dir/$LABEL.timer" <<EOF
[Unit]
Description=Run the jcode upstream merge agent every ${INTERVAL_HOURS}h

[Timer]
OnBootSec=15min
OnUnitActiveSec=${INTERVAL_HOURS}h
Persistent=true

[Install]
WantedBy=timers.target
EOF
  systemctl --user daemon-reload
  systemctl --user enable --now "$LABEL.timer"
  echo "installed systemd user timer: $LABEL.timer (every ${INTERVAL_HOURS}h)"
  systemctl --user list-timers "$LABEL.timer" --no-pager
}

uninstall_systemd() {
  systemctl --user disable --now "$LABEL.timer" 2>/dev/null
  rm -f "$HOME/.config/systemd/user/$LABEL.service" "$HOME/.config/systemd/user/$LABEL.timer"
  systemctl --user daemon-reload
  echo "removed systemd user timer: $LABEL"
}

status_systemd() {
  systemctl --user list-timers "$LABEL.timer" --no-pager
  systemctl --user status "$LABEL.service" --no-pager | head -20
}

# --- macOS / launchd ---------------------------------------------------------
PLIST="$HOME/Library/LaunchAgents/com.jcode.upstream-merge.plist"

install_launchd() {
  mkdir -p "$HOME/Library/LaunchAgents" "$HOME/.jcode/upstream-merge/logs"
  cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.jcode.upstream-merge</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>$AGENT</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>JCODE_UPSTREAM_REPO</key><string>$REPO</string>
    <key>JCODE_BIN</key><string>$JCODE_BIN</string>
    <key>PATH</key><string>$HOME/.local/bin:$HOME/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin</string>
  </dict>
  <key>StartInterval</key><integer>$INTERVAL_SECS</integer>
  <key>RunAtLoad</key><false/>
  <key>Nice</key><integer>10</integer>
  <key>StandardOutPath</key><string>$HOME/.jcode/upstream-merge/logs/launchd.out.log</string>
  <key>StandardErrorPath</key><string>$HOME/.jcode/upstream-merge/logs/launchd.err.log</string>
</dict>
</plist>
EOF
  launchctl unload "$PLIST" 2>/dev/null
  launchctl load "$PLIST" || { echo "launchctl load failed"; exit 1; }
  echo "installed launchd agent: com.jcode.upstream-merge (every ${INTERVAL_HOURS}h)"
}

uninstall_launchd() {
  launchctl unload "$PLIST" 2>/dev/null
  rm -f "$PLIST"
  echo "removed launchd agent: com.jcode.upstream-merge"
}

status_launchd() {
  launchctl list | grep -i upstream-merge || echo "not loaded"
  tail -20 "$HOME/.jcode/upstream-merge/logs/launchd.err.log" 2>/dev/null
}

# --- fallback ----------------------------------------------------------------
print_cron() {
  echo "No systemd or launchd detected. Add this crontab line (crontab -e):"
  echo "  0 */$INTERVAL_HOURS * * * JCODE_UPSTREAM_REPO=$REPO JCODE_BIN=$JCODE_BIN $AGENT"
}

case "$MODE" in
  run-now)
    JCODE_UPSTREAM_REPO="$REPO" JCODE_BIN="$JCODE_BIN" "$AGENT"
    exit $?
    ;;
esac

case "$OS" in
  Linux)
    if command -v systemctl >/dev/null 2>&1; then
      case "$MODE" in
        install) install_systemd ;;
        uninstall) uninstall_systemd ;;
        status) status_systemd ;;
      esac
    else
      print_cron
    fi
    ;;
  Darwin)
    case "$MODE" in
      install) install_launchd ;;
      uninstall) uninstall_launchd ;;
      status) status_launchd ;;
    esac
    ;;
  *)
    print_cron
    ;;
esac
