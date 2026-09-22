#!/usr/bin/env bash
# What a bridge costs while it is mirroring, against the bridge this replaces,
# on the same pages in the same prefix.
#
# The numbers come from /proc/<pid>/stat — the process's own user and system
# jiffies. wineserver's cost is deliberately not counted on either side: it is
# the same server for both, and attributing it to one would flatter whichever
# ran second.
#
# Two things this gets wrong if you write it casually, both of which produced
# nonsense before they were noticed:
#
#   * Wine names the process `main`, so /proc/<pid>/comm never says
#     "wineshm.exe", and matching the whole command line also matches this
#     script. Both together find the one process that is the bridge.
#   * Killing the `wine` wrapper does not kill the Windows process it started.
#     A leftover from an earlier run holds the section names, and every
#     measurement after it is of something else entirely — which is how this
#     first reported a 94% saving that was not real.
#
# Usage:  bench/measure.sh /path/to/shm-bridge.exe [seconds]
set -uo pipefail

OLD="${1:?give the path to the old shm-bridge.exe}"
WATCH="${2:-20}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NEW="$HERE/target/x86_64-pc-windows-gnu/release/wineshm.exe"

WINE="${WINE:-$HOME/.local/share/Steam/steamapps/common/Proton - Experimental/files/bin/wine}"
export WINEPREFIX="${WINEPREFIX:-$HOME/.wineshm-bench-prefix}"
export WINEDEBUG=-all

PAGES="--page acpmf_physics:2048 --page acpmf_graphics:2048 --page acpmf_static:2048"
PAGES="$PAGES --page acpmf_crewchief:15660 --page AcTools.CSP.Limited.ACPE.v1:2484"
MIRROR_DIR=/dev/shm/wineshm-bench

windows_pids() { # every Windows process wine is running
  local d
  for d in /proc/[0-9]*; do
    [ "$(cat "$d/comm" 2>/dev/null)" = "main" ] || continue
    echo "${d#/proc/}"
  done
}

pid_matching() { # needles... -> the Windows process whose command line has them all
  local d c want ok
  for d in /proc/[0-9]*; do
    [ "$(cat "$d/comm" 2>/dev/null)" = "main" ] || continue
    c="$(tr '\0' ' ' < "$d/cmdline" 2>/dev/null)" || continue
    ok=1
    for want in "$@"; do case "$c" in *"$want"*) ;; *) ok=0 ;; esac; done
    [ "$ok" = 1 ] && { echo "${d#/proc/}"; return; }
  done
}

jiffies_of() { local p="$1"; [ -r "/proc/$p/stat" ] && awk '{print $14 + $15}' "/proc/$p/stat"; }

clean_slate() {
  local p
  for p in $(windows_pids); do kill "$p" 2>/dev/null; done
  sleep 2
  for p in $(windows_pids); do kill -9 "$p" 2>/dev/null; done
  rm -f /dev/shm/acpmf_* "/dev/shm/AcTools.CSP.Limited.ACPE.v1" \
        /dev/shm/wineshm.info /dev/shm/acpe-bridge.info
  rm -rf "$MIRROR_DIR"; mkdir -p "$MIRROR_DIR"
}

watch_it() { # label needles...
  local label="$1"; shift
  local pid before after used
  pid="$(pid_matching "$@")"
  if [ -z "$pid" ]; then printf '  %-18s not found\n' "$label"; return; fi
  before="$(jiffies_of "$pid")"
  sleep "$WATCH"
  after="$(jiffies_of "$pid")"
  [ -z "$after" ] && { printf '  %-18s died\n' "$label"; return; }
  used=$((after - before))
  awk -v l="$label" -v u="$used" -v hz="$(getconf CLK_TCK)" -v s="$WATCH" \
    'BEGIN { printf "  %-18s %5d jiffies  %6.3f s  %6.2f%% of one core\n", l, u, u/hz, 100*(u/hz)/s }'
}

stop_matching() { local p; for p in $(pid_matching "$@"); do kill "$p" 2>/dev/null; done; sleep 3; }

echo "prefix: $WINEPREFIX"
echo "each measured for ${WATCH}s, after 6s of settling"
clean_slate

# Something has to hold the names so both bridges land in their mirroring path,
# which is the path being compared.
"$WINE" "$NEW" $PAGES --quiet >/dev/null 2>&1 &
sleep 7

echo
echo "IDLE — nothing writing (menus, garage, a paused session, the results screen)"
"$WINE" "$OLD" < /dev/null >/dev/null 2>&1 &
sleep 6; watch_it "old shm-bridge" "shm-bridge.exe"
stop_matching "shm-bridge.exe"

"$WINE" "$NEW" $PAGES --dir "$MIRROR_DIR" --quiet >/dev/null 2>&1 &
sleep 6; watch_it "wineshm" "wineshm.exe" "wineshm-bench"
stop_matching "wineshm.exe" "wineshm-bench"

echo
echo "BUSY — two of the five rewritten at ~333 Hz (a car on track)"
python3 - >/dev/null 2>&1 <<'WRITER' &
import os, time
paths = ["/dev/shm/acpmf_physics", "/dev/shm/acpmf_graphics"]
handles = [open(p, "r+b") for p in paths if os.path.exists(p)]
i = 0
while True:
    for handle in handles:
        handle.seek(0)
        handle.write(bytes([i & 0xFF]) * 256)
        handle.flush()
    i += 1
    time.sleep(0.003)
WRITER
WRITER_PID=$!
sleep 1

"$WINE" "$OLD" < /dev/null >/dev/null 2>&1 &
sleep 6; watch_it "old shm-bridge" "shm-bridge.exe"
stop_matching "shm-bridge.exe"

"$WINE" "$NEW" $PAGES --dir "$MIRROR_DIR" --quiet >/dev/null 2>&1 &
sleep 6; watch_it "wineshm" "wineshm.exe" "wineshm-bench"

kill "$WRITER_PID" 2>/dev/null
clean_slate

echo
echo "Binary size"
stat -c '  %-46n %8s bytes' "$OLD" "$NEW"
