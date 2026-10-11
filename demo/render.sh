#!/usr/bin/env bash
# Record Claude Code preparing Jane Doe's return with NotIntoIt, then cut demo.mp4 and demo.gif.
# Needs vhs, ttyd, and ffmpeg with libx264 or libopenh264; output lands in target/demo/.
# --edit-only re-cuts the existing recording.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
out="$repo/target/demo"
nii="$repo/target/release/nii"
model=${DEMO_MODEL:-sonnet}
speedup=${DEMO_SPEEDUP:-4}
hold=${DEMO_HOLD:-15}
prompt_at=${DEMO_PROMPT_AT:-12}

for tool in vhs ttyd ffmpeg ffprobe claude; do
    command -v "$tool" >/dev/null || { echo "missing $tool" >&2; exit 1; }
done
[[ -x $nii ]] || { echo "build first: cargo build --release -p nii" >&2; exit 1; }
encoder=$(ffmpeg -hide_banner -encoders 2>/dev/null | grep -oE 'libx264|libopenh264' | head -1)
[[ -n $encoder ]] || { echo "ffmpeg has neither libx264 nor libopenh264" >&2; exit 1; }

if [[ ${1:-} != --edit-only ]]; then
rm -rf "$out"
mkdir -p "$out/Taxes/2025"
cp -r "$repo/examples/jane-doe/2025/intake.md" "$repo/examples/jane-doe/2025/documents" "$out/Taxes/2025/"
cat > "$out/mcp.json" <<EOF
{"mcpServers": {"notintoit": {"command": "$nii", "args": ["mcp"], "env": {"NII_WORKSPACE": "$out/Taxes"}}}}
EOF
# Variables of an enclosing Claude Code session, which would change the recorded one.
cat > "$out/setup.sh" <<EOF
unset \$(compgen -e | grep -E '^(CLAUDECODE|CLAUDE_CODE_|CLAUDE_PID|CLAUDE_EFFORT|AI_AGENT|ANTHROPIC_BASE_URL)')
claude() { command claude --model $model --strict-mcp-config --mcp-config $out/mcp.json --tools "" --allowedTools 'mcp__notintoit__*' "\$@"; }
EOF

(cd "$repo" && vhs demo/demo.tape)
fi

duration=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$out/raw.webm")
# The tape holds 25s on the summary, then quits for ~2.5s.
summary_at=$(awk "BEGIN { print $duration - 27.5 }")
hold_end=$(awk "BEGIN { e = $summary_at + $hold; print (e < $duration ? e : $duration) }")

forms="$out/Taxes/2025/forms"
"$nii" form view "$forms/f1040.pdf" --page 1 --width 1100 --png "$out/p1.png" >/dev/null
"$nii" form view "$forms/f1040.pdf" --page 2 --width 1100 --png "$out/p2.png" >/dev/null
ffmpeg -v error -y -i "$out/p1.png" -i "$out/p2.png" -filter_complex \
    "[0]scale=-1:860[a];[1]scale=-1:860[b];[a][b]hstack=inputs=2,pad=1400:900:(ow-iw)/2:(oh-ih)/2:color=0x1e1e2e" \
    -frames:v 1 "$out/ending.png"

ffmpeg -v error -y -i "$out/raw.webm" -loop 1 -t 5 -framerate 30 -i "$out/ending.png" -filter_complex "
    [0:v]fps=30,split=3[v1][v2][v3];
    [v1]trim=0:$prompt_at,setpts=PTS-STARTPTS[a];
    [v2]trim=$prompt_at:$summary_at,setpts=(PTS-STARTPTS)/$speedup[b];
    [v3]trim=$summary_at:$hold_end,setpts=PTS-STARTPTS[c];
    [1:v]fps=30,format=yuv420p,setsar=1[d];
    [a][b][c]concat=n=3:v=1:a=0,format=yuv420p,setsar=1[abc];
    [abc][d]concat=n=2:v=1:a=0[outv]" \
    -map "[outv]" -c:v "$encoder" -b:v 2M -movflags +faststart "$out/demo.mp4"
ffmpeg -v error -y -i "$out/demo.mp4" \
    -vf "fps=10,scale=960:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=96[p];[s1][p]paletteuse=dither=bayer:bayer_scale=4" \
    "$out/demo.gif"

ls -la "$out/demo.mp4" "$out/demo.gif"
