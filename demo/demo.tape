Output target/demo/raw.webm
Set Shell "bash"
Set FontSize 16
Set Width 1400
Set Height 900
Set Padding 24
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms
Set WindowBar Colorful

Hide
Type "cd target/demo/Taxes && export PS1='\[\e[1;35m\]taxes\[\e[0m\] $ ' && source ../setup.sh && clear"
Enter
Sleep 1s
Show

Type "ls 2025 2025/documents"
Enter
Sleep 3s
Type "claude"
Enter
Wait+Screen@60s /Claude Code/
Sleep 3s
Type "Do my 2025 taxes. My documents and notes are in my tax workspace. Fill out every form I need."
Sleep 1s
Enter
Wait+Screen@25m /(?i)(\$[0-9,.]+ refund|refund of \$)/
Sleep 25s
Ctrl+C
Sleep 500ms
Ctrl+C
Sleep 2s
