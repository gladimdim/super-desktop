import QtQuick

// A deliberately small terminal: sd.openPty runs the shell, its output is
// shown as plain text (escape sequences are dropped, so full-screen programs
// like vim will not draw), and keys are typed into it. A real terminal plugin
// would parse the escape sequences into a screen grid instead.
Rectangle {
    id: term
    color: sd.theme.darkBackground
    focus: true

    property var pty: null
    property string screen: ""
    readonly property int maxChars: 20000
    readonly property int cols: Math.max(20, Math.floor((width - 16) / metrics.averageCharacterWidth))
    readonly property int rows: Math.max(5, Math.floor((height - 16) / metrics.height))

    FontMetrics { id: metrics; font: text.font }

    function feed(chunk) {
        // Drop CSI / OSC sequences and other escapes; keep text, newlines, tabs.
        chunk = chunk.replace(/\x1b\][^\x07\x1b]*(\x07|\x1b\\)/g, "")
                     .replace(/\x1b\[[0-?]*[ -\/]*[@-~]/g, "")
                     .replace(/\x1b[()][A-Za-z0-9]/g, "")
                     .replace(/\x1b./g, "")
                     .replace(/\r\n/g, "\n")
        let out = screen
        for (const ch of chunk) {
            if (ch === "\b") out = out.slice(0, -1)
            else if (ch === "\r") out = out.slice(0, out.lastIndexOf("\n") + 1)
            else if (ch === "\x07") continue
            else out += ch
        }
        screen = out.length > maxChars ? out.slice(out.length - maxChars) : out
    }

    function start() {
        screen = ""
        pty = sd.openPty([sd.env("SHELL") || "/bin/sh", "-i"], { cols: cols, rows: rows, cwd: sd.env("HOME") })
        if (!pty) { screen = "Could not start a shell."; return }
        pty.output.connect(feed)
        pty.exited.connect(function(code) { feed("\n[exited with " + code + "; press Enter to restart]\n"); pty = null })
    }

    Component.onCompleted: start()
    onColsChanged: if (pty) pty.resize(cols, rows)
    onRowsChanged: if (pty) pty.resize(cols, rows)

    Keys.onPressed: (event) => {
        if (!pty) { if (event.key === Qt.Key_Return) start(); event.accepted = true; return }
        const ctrl = event.modifiers & Qt.ControlModifier
        let send = ""
        switch (event.key) {
        case Qt.Key_Return: case Qt.Key_Enter: send = "\r"; break
        case Qt.Key_Backspace: send = "\x7f"; break
        case Qt.Key_Tab: send = "\t"; break
        case Qt.Key_Escape: send = "\x1b"; break
        case Qt.Key_Up: send = "\x1b[A"; break
        case Qt.Key_Down: send = "\x1b[B"; break
        case Qt.Key_Right: send = "\x1b[C"; break
        case Qt.Key_Left: send = "\x1b[D"; break
        default:
            if (ctrl && event.key >= Qt.Key_A && event.key <= Qt.Key_Z) send = String.fromCharCode(event.key - Qt.Key_A + 1)
            else send = event.text
        }
        if (send.length > 0) { pty.write(send); event.accepted = true }
    }

    MouseArea { anchors.fill: parent; cursorShape: Qt.IBeamCursor; onClicked: term.forceActiveFocus() }

    Flickable {
        id: view
        anchors.fill: parent
        anchors.margins: 8
        contentHeight: text.height
        clip: true
        // Follow the newest output.
        onContentHeightChanged: contentY = Math.max(0, contentHeight - height)

        Text {
            id: text
            width: view.width
            text: term.screen + (term.activeFocus ? "▌" : "")
            color: sd.theme.foreground
            font.family: "monospace"
            font.pixelSize: 13
            wrapMode: Text.WrapAnywhere
            textFormat: Text.PlainText
        }
    }
}
