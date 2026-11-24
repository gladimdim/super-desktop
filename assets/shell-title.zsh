autoload -Uz add-zsh-hook
__sd_title_preexec() {
    local text="$1"
    if [[ "$text" == [[:space:]]* ]]; then
        text=""
    fi
    command tmux set-option -pq -t "$TMUX_PANE" @super_desktop_shell_command "$text" 2>/dev/null
}
command tmux set-option -pq -t "$TMUX_PANE" @super_desktop_shell_tracking 1 2>/dev/null
add-zsh-hook preexec __sd_title_preexec
