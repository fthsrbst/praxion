#!/usr/bin/env bash
# Round 0 capture corpus: drive the proxied client for ~12 minutes with injected input and RCON.
# Requires: server on 25566, proxy on 25565 capturing, client "Praxion" joined through the proxy
# and its window in the foreground. Writes the block ground truth to blocks-truth.txt.
set -u
cd "$(dirname "$0")"
H=../hands/target/release/hands.exe
LOG=../../logs/corpus-play.jsonl
rcon() { python rcon.py "$@"; }
key() { $H key-hold --scancode "$1" --hold-ms "$2" --repeat "${3:-1}" --gap-ms 150 --delay-secs 0 --max-secs 30 --log $LOG >/dev/null || exit $?; }
look() { $H mouse-move --counts "$1" "$2" --repeat "$3" --interval-ms 10 --delay-secs 0 --max-secs 30 --log $LOG >/dev/null || exit $?; }

echo "status pings through proxy and direct"
../proxy/target/release/pingbench.exe --target 127.0.0.1:25565 --n 300 --protocol 777
../proxy/target/release/pingbench.exe --target 127.0.0.1:25566 --n 300 --protocol 777

rcon "gamemode survival Praxion" "tp Praxion 272.5 80 -125.5" "time set noon"
# Ground-truth blocks placed while the area is loaded (arrive as block_update / section updates).
: > blocks-truth.txt
place() { rcon "setblock $1 $2 $3 $4" >/dev/null; echo "$1 $2 $3 $4" >> blocks-truth.txt; }
place 270 120 -120 minecraft:stone
place 271 120 -120 "minecraft:oak_stairs[facing=east,half=top]"
place 272 120 -120 "minecraft:chest[facing=north]"
place 273 120 -120 minecraft:glass
place 274 120 -120 "minecraft:redstone_wall_torch[facing=south,lit=true]"
rcon "fill 260 121 -130 264 125 -126 minecraft:oak_planks" >/dev/null   # many blocks: section_blocks_update
echo "260..264 121..125 -130..-126 minecraft:oak_planks" >> blocks-truth.txt

for i in $(seq 1 10); do
  echo "iteration $i $(date +%T)"
  key 0x11 3000            # W forward
  look 20 0 50             # turn right 150 deg
  key 0x39 120 3           # jump x3
  key 0x1F 1500            # S back
  look -10 5 40            # turn left/down
  key 0x1D 800             # left ctrl (sprint key) tap
  look 0 -5 40
  if (( i % 3 == 0 )); then
    rcon "tp Praxion $((272 + i * 1000)) 120 -125" >/dev/null   # far teleport: chunk burst
    sleep 15
    rcon "tp Praxion 272.5 80 -125.5" >/dev/null
  fi
  sleep 30
done

# Changes made after the chunks were (re)sent, back at spawn.
place 275 120 -120 minecraft:gold_block
place 276 120 -120 "minecraft:oak_log[axis=x]"
sleep 5
rcon "transfer localhost 25565 Praxion"   # handshake intent 3 (transfer) on a new connection
sleep 25
place 277 120 -120 minecraft:diamond_block
sleep 10
echo "done $(date +%T)"
