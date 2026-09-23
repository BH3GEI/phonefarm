. ../tools/pf_bin.sh
"$PF" report \
  --baseline 'ctrl*' --knob 'knob*' \
  --snap-before snap_before.txt --snap-after snap_final.txt \
  --replay-result replay_exit.txt \
  --primary frame_p95
