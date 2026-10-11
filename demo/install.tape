Output target/demo/install.webm
Set Shell "bash"
Set FontSize 18
Set Width 1200
Set Height 640
Set Padding 24
Set Theme "Catppuccin Mocha"
Set TypingSpeed 35ms
Set WindowBar Colorful

Hide
Type "podman run -it --rm nii-install-clip bash"
Enter
Sleep 3s
Type "export PS1='\[\e[1;35m\]fresh ubuntu\[\e[0m\] $ ' && clear"
Enter
Sleep 1s
Show

Type "curl -LsSf https://github.com/DerekParks/NotIntoIt/releases/latest/download/nii-installer.sh | sh"
Sleep 500ms
Enter
Wait+Screen@120s /source/
Sleep 2s
Type "source ~/.cargo/env && nii --version"
Enter
Sleep 2s
Type "nii form fetch 1040 --year 2025"
Enter
Wait+Screen@120s /f1040\.pdf/
Sleep 3s
Type "nii mcp   # point any MCP client here: claude mcp add notintoit -- nii mcp"
Sleep 3s
