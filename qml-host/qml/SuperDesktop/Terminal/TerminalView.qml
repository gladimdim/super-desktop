import QtQuick
import "vt.mjs" as VT

// A terminal for plugins: `TerminalView { anchors.fill: parent }` runs the
// user's shell (or `command`) on a pseudo-terminal and draws it with the
// theme's colors. Click it to type. Ctrl+Shift+V or Shift+Insert pastes.
Item {
    id: view

    /// Program and arguments; empty means the user's $SHELL.
    property var command: []
    property string workingDirectory: sd.env("HOME")
    /// Extra environment variables for the program.
    property var environment: ({})
    property bool autoStart: true
    property string fontFamily: "monospace"
    property int fontPixelSize: 13
    property color foreground: sd.theme.foreground || "#e5e5e5"
    property color background: sd.theme.darkBackground || sd.theme.background || "#000000"
    property color cursorColor: sd.theme.accent || foreground
    property int scrollbackLines: 2000

    /// The program's window title (OSC 0/2), e.g. the running command.
    readonly property string title: priv.title
    readonly property int cols: Math.max(2, Math.floor(width / metrics.cellWidth))
    readonly property int rows: Math.max(1, Math.floor(height / metrics.cellHeight))
    readonly property bool running: priv.pty !== null && priv.pty.running

    signal exited(int code)
    signal bell()

    /// Starts `command` (or the shell); stops a running one first.
    function start() {
        stop();
        const argv = command && command.length ? command : [sd.env("SHELL") || "/bin/sh"];
        priv.screen.reset();
        priv.pty = sd.openPty(argv, { cols: cols, rows: rows, cwd: workingDirectory, env: environment });
        if (!priv.pty) {
            feed("\r\n[could not start " + argv[0] + "]\r\n");
            return false;
        }
        priv.pty.output.connect(feed);
        priv.pty.exited.connect(function (code) { priv.pty = null; view.exited(code); });
        priv.repaint(true);
        return true;
    }

    function stop() {
        if (priv.pty) {
            priv.pty.terminate(1);
            priv.pty = null;
        }
    }

    /// Types `text` into the program.
    function send(text) {
        if (priv.pty) priv.pty.write(text);
    }

    /// Pastes `text` (bracketed when the program asked for it).
    function paste(text) {
        if (!text) return;
        send(priv.screen.bracketedPaste ? "\x1b[200~" + text + "\x1b[201~" : text.replace(/\n/g, "\r"));
    }

    /// Draws `text` as if the program printed it.
    function feed(text) {
        priv.screen.write(text);
        priv.schedule();
    }

    /// The text of a visible row, for tests and screen readers.
    function lineText(row) {
        return priv.screen.lineText(row, priv.offset);
    }

    function cursorPosition() {
        return Qt.point(priv.screen.x, priv.screen.y);
    }

    function cellAt(row, col) {
        return priv.screen.cell(row, col);
    }

    focus: true
    activeFocusOnTab: true
    clip: true

    QtObject {
        id: priv
        property var pty: null
        property bool startPending: false
        property string title: ""
        property int offset: 0
        property var palette: VT.themePalette(sd.theme)
        property var screen: new VT.Screen(80, 24, {
            scrollback: view.scrollbackLines,
            respond: function (text) { view.send(text) },
            onTitle: function (text) { priv.title = text },
            onBell: function () { view.bell() },
        })

        function schedule() {
            if (!paint.running) paint.start();
        }

        function repaint(all) {
            const dirty = screen.takeDirty();
            const rowsToPaint = all || offset > 0 || dirty === "all" ? null : dirty;
            const fg = view.foreground.toString();
            const bg = view.background.toString();
            for (let row = 0; row < lines.count; row++) {
                if (rowsToPaint && rowsToPaint.indexOf(row) < 0) continue;
                const item = lines.itemAt(row);
                const line = screen.visibleLine(row, offset);
                if (item) item.text = line ? VT.lineHtml(line, palette, fg, bg) : "";
            }
            cursor.x = screen.x * metrics.cellWidth;
            cursor.y = screen.y * metrics.cellHeight;
            cursor.visible = screen.cursorVisible && offset === 0;
        }
    }

    onColsChanged: resizeTimer.restart()
    onRowsChanged: resizeTimer.restart()
    // The program starts once the view has its size, so it starts with the
    // right number of columns and rows.
    Component.onCompleted: {
        if (width > 0 && height > 0) {
            priv.screen.resize(cols, rows);
            if (autoStart) start();
        } else if (autoStart) {
            priv.startPending = true;
        }
    }
    Component.onDestruction: stop()
    Connections {
        target: sd
        function onThemeChanged() {
            priv.palette = VT.themePalette(sd.theme);
            priv.repaint(true);
        }
    }

    Timer {
        id: paint
        interval: 16
        onTriggered: priv.repaint(false)
    }
    // A resize settles before the program is told: shells redraw on SIGWINCH.
    Timer {
        id: resizeTimer
        interval: 60
        onTriggered: {
            if (view.width <= 0 || view.height <= 0) return;
            priv.screen.resize(view.cols, view.rows);
            if (priv.startPending) {
                priv.startPending = false;
                view.start();
            }
            if (priv.pty) priv.pty.resize(view.cols, view.rows);
            priv.repaint(true);
        }
    }

    FontMetrics {
        id: metrics
        font.family: view.fontFamily
        font.pixelSize: view.fontPixelSize
        readonly property real cellWidth: Math.max(1, advanceWidth("M"))
        readonly property real cellHeight: Math.max(1, Math.ceil(height))
    }

    Rectangle {
        anchors.fill: parent
        color: view.background
    }

    Repeater {
        id: lines
        model: view.rows
        delegate: Text {
            y: index * metrics.cellHeight
            width: view.width
            height: metrics.cellHeight
            textFormat: Text.RichText
            wrapMode: Text.NoWrap
            clip: true
            font.family: view.fontFamily
            font.pixelSize: view.fontPixelSize
            color: view.foreground
        }
        onItemAdded: priv.schedule()
    }

    Rectangle {
        id: cursor
        width: metrics.cellWidth
        height: metrics.cellHeight
        color: view.activeFocus ? view.cursorColor : "transparent"
        border.color: view.cursorColor
        border.width: view.activeFocus ? 0 : 1
        opacity: 0.55
    }

    Keys.onPressed: (event) => {
        const ctrlShift = (event.modifiers & Qt.ControlModifier) && (event.modifiers & Qt.ShiftModifier);
        if ((ctrlShift && event.key === Qt.Key_V) || ((event.modifiers & Qt.ShiftModifier) && event.key === Qt.Key_Insert)) {
            sd.exec(["wl-paste", "--no-newline"], function (r) { if (r.code === 0) view.paste(r.stdout) });
            event.accepted = true;
            return;
        }
        if ((event.modifiers & Qt.ShiftModifier) && (event.key === Qt.Key_PageUp || event.key === Qt.Key_PageDown)) {
            scrollBy(event.key === Qt.Key_PageUp ? view.rows - 1 : -(view.rows - 1));
            event.accepted = true;
            return;
        }
        const bytes = VT.Screen.keyBytes(event.key, event.text, event.modifiers, priv.screen.appCursor);
        // With no program running, keys are left to the plugin (e.g. Enter to restart).
        if (bytes.length > 0 && priv.pty) {
            if (priv.offset !== 0) { priv.offset = 0; priv.repaint(true); }
            view.send(bytes);
            event.accepted = true;
        }
    }

    function scrollBy(lines) {
        const most = priv.screen.alternate ? 0 : priv.screen.scrollback.length;
        const next = Math.max(0, Math.min(most, priv.offset + lines));
        if (next !== priv.offset) {
            priv.offset = next;
            priv.repaint(true);
        }
    }

    MouseArea {
        anchors.fill: parent
        cursorShape: Qt.IBeamCursor
        acceptedButtons: Qt.LeftButton | Qt.MiddleButton
        onPressed: view.forceActiveFocus()
        onClicked: (mouse) => {
            if (mouse.button === Qt.MiddleButton)
                sd.exec(["wl-paste", "--primary", "--no-newline"], function (r) { if (r.code === 0) view.paste(r.stdout) });
        }
        onWheel: (wheel) => {
            const notches = Math.round(wheel.angleDelta.y / 120) || (wheel.angleDelta.y > 0 ? 1 : wheel.angleDelta.y < 0 ? -1 : 0);
            if (priv.screen.alternate) {
                // Full-screen programs scroll themselves.
                const key = notches > 0 ? (priv.screen.appCursor ? "\x1bOA" : "\x1b[A") : (priv.screen.appCursor ? "\x1bOB" : "\x1b[B");
                for (let i = 0; i < Math.abs(notches) * 3; i++) view.send(key);
            } else {
                view.scrollBy(notches * 3);
            }
        }
    }
}
