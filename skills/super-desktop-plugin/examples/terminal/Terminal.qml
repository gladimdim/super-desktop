import QtQuick
import QtQuick.Layouts
import SuperDesktop.Terminal

// A terminal card: TerminalView does the work (pty, escape sequences,
// colors, keys, scrollback, paste). This adds a title line and a restart
// hint when the shell exits.
Rectangle {
    color: sd.theme.darkBackground

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 6
        spacing: 4

        Text {
            Layout.fillWidth: true
            text: term.title || "Terminal"
            color: sd.theme.muted
            font.family: sd.theme.fontFamily
            font.pixelSize: 11
            elide: Text.ElideMiddle
        }

        TerminalView {
            id: term
            Layout.fillWidth: true
            Layout.fillHeight: true
            fontPixelSize: 13
            onExited: (code) => term.feed("\r\n\x1b[2m[exited with " + code + "; press Enter to start again]\x1b[0m\r\n")
            Keys.onReturnPressed: (event) => { if (!term.running) term.start(); else event.accepted = false }
        }
    }
}
