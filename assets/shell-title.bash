# Loaded only by SUPER DESKTOP's plain Bash terminals. Keep the user's normal
# interactive setup and append to its hooks rather than replacing them.
[[ -f ~/.bashrc ]] && source ~/.bashrc

__sd_title_history() {
    local previous_status=$?
    __sd_title_previous=$(HISTTIMEFORMAT= builtin history 1)
    return "$previous_status"
}
__sd_title_command() {
    local line text=''
    line=$(HISTTIMEFORMAT= builtin history 1)
    # A command excluded by HISTCONTROL/HISTIGNORE must not reuse old history.
    if [[ $line != "$__sd_title_previous" && $line =~ ^[[:space:]]*[0-9]+[[:space:]]+(.*)$ ]]; then
        text=${BASH_REMATCH[1]}
    fi
    command tmux set-option -pq -t "$TMUX_PANE" @super_desktop_shell_command "$text" 2>/dev/null
}
command tmux set-option -pq -t "$TMUX_PANE" @super_desktop_shell_tracking 1 2>/dev/null
PROMPT_COMMAND+=(__sd_title_history)
PS0='$(__sd_title_command)'"${PS0-}"
