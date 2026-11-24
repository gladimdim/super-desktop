function __sd_title_preexec --on-event fish_preexec
    set -l text "$argv[1]"
    if string match -qr '^[[:space:]]' -- "$text"
        set text ""
    end
    command tmux set-option -pq -t "$TMUX_PANE" @super_desktop_shell_command "$text" 2>/dev/null
end
command tmux set-option -pq -t "$TMUX_PANE" @super_desktop_shell_tracking 1 2>/dev/null
