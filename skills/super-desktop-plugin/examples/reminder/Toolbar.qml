import QtQuick
import "reminders.js" as R

// The top-bar bell. Left click: add a reminder. Right click: the list.
// It also watches the clock: when a reminder is due it notifies, plays
// ding-dong.wav, and rings (the bell shakes, the pill runs a gradient) until
// the user clicks it.
Rectangle {
    id: root
    radius: height / 2
    color: area.containsMouse ? sd.theme.accent : Qt.alpha(sd.theme.surface, 0.9)
    gradient: ringing ? ringGradient : null

    readonly property var reminders: R.list(sd.values)
    readonly property int pending: reminders.filter(r => !r.fired).length
    readonly property int due: reminders.filter(r => r.fired).length
    // Gone off and not looked at yet (checkDue sets seen: false).
    readonly property bool ringing: reminders.some(r => r.fired && r.seen === false)
    // Whether SUPER DESKTOP is on screen: animations run only then.
    property bool shown: true
    // Times the sound played for the current ringing.
    property int dings: 0

    readonly property int maxDings: 5

    Connections {
        target: sd
        function onVisibleChanged(visible) { root.shown = visible }
    }

    // Plays ding-dong.wav (make it with `python3 make-ding-dong.py
    // ding-dong.wav`; examples come without it), else the desktop's alarm
    // sound. SUPER DESKTOP picks the player; check and snapshot stay quiet.
    function ding() {
        root.dings += 1
        sd.call("sound.play", { file: "ding-dong.wav" }, function(result, err) {
            if (err) sd.call("sound.play", { name: "alarm-clock-elapsed" })
        })
    }

    function checkDue() {
        var all = R.list(sd.values)
        var now = Date.now()
        var fired = []
        for (var i = 0; i < all.length; i++) {
            var r = all[i]
            if (r.fired || r.at > now) continue
            all[i] = Object.assign({}, r, { fired: true, seen: false })
            fired.push(r.text)
        }
        if (fired.length === 0 || !sd.connected) return
        sd.setValue("reminders", all)
        for (var j = 0; j < fired.length; j++) {
            const body = fired[j]
            // Stays until dismissed; a click on it brings up the list.
            sd.call("notify", { title: "⏰ Reminder", body: body, urgency: "critical",
                                actions: [{ id: "default", label: "Show" }] }, function(result, err) {
                if (result && result.action === "default")
                    sd.call("overlay.show", {}, function() { sd.call("popup.open", { surface: "list" }) })
            })
        }
        root.dings = 0
        root.ding()
    }

    // Stops the ringing: every reminder that went off counts as seen.
    function acknowledge() {
        sd.setValue("reminders", R.list(sd.values).map(r => r.fired ? Object.assign({}, r, { seen: true }) : r))
    }

    // Reminders are due while SUPER DESKTOP is hidden too. sd-lint: ignore hidden-timer
    Timer {
        interval: 5000; running: true; repeat: true; triggeredOnStart: true
        onTriggered: root.checkDue()
    }
    // Rings again every minute until clicked, a few times at most.
    Timer {
        interval: 60000; repeat: true
        running: root.ringing && root.dings < root.maxDings
        onTriggered: root.ding()
    }

    // The ringing pill: theme colors that show on its background, sliding
    // across it.
    readonly property var ringColors: {
        var names = ["accent", "magenta", "orange", "yellow", "cyan", "green", "blue"]
        var picked = []
        for (var i = 0; i < names.length; i++) {
            var c = sd.theme[names[i]]
            if (c && R.contrast(c, sd.theme.background) >= 3 && picked.indexOf(c) < 0) picked.push(c)
        }
        if (picked.length < 2) picked = [sd.theme.accent, sd.theme.foreground]
        return picked
    }
    property real phase: 0
    NumberAnimation on phase {
        from: 0; to: 1; duration: 2400; loops: Animation.Infinite
        running: root.ringing && root.shown
    }
    function ringColor(t) {
        var n = root.ringColors.length
        var x = ((t % 1) + 1) % 1 * n
        var i = Math.floor(x)
        return R.mix(root.ringColors[i % n], root.ringColors[(i + 1) % n], x - i)
    }
    Gradient {
        id: ringGradient
        orientation: Gradient.Horizontal
        GradientStop { position: 0.0; color: root.ringColor(root.phase) }
        GradientStop { position: 0.5; color: root.ringColor(root.phase + 0.33) }
        GradientStop { position: 1.0; color: root.ringColor(root.phase + 0.66) }
    }

    Bell {
        id: bell
        anchors.centerIn: parent
        anchors.horizontalCenterOffset: ((root.pending + root.due) > 0 ? -5 : 0) + bell.jitter
        width: root.height * 0.6; height: width
        transformOrigin: Item.Top
        color: area.containsMouse || root.ringing ? sd.theme.background
             : root.due > 0 ? R.readable(sd.theme, ["yellow", "orange", "accent"], 4.5) : sd.theme.foreground

        // The buzz: a burst of quick swings and shivers, then a pause.
        property real jitter: 0
        SequentialAnimation {
            running: root.ringing && root.shown
            loops: Animation.Infinite
            onRunningChanged: if (!running) { bell.rotation = 0; bell.jitter = 0 }
            ParallelAnimation {
                SequentialAnimation {
                    loops: 3
                    NumberAnimation { target: bell; property: "rotation"; to: 18; duration: 45; easing.type: Easing.OutQuad }
                    NumberAnimation { target: bell; property: "rotation"; to: -18; duration: 90; easing.type: Easing.InOutQuad }
                    NumberAnimation { target: bell; property: "rotation"; to: 0; duration: 45; easing.type: Easing.InQuad }
                }
                SequentialAnimation {
                    loops: 9
                    NumberAnimation { target: bell; property: "jitter"; to: 1.5; duration: 30 }
                    NumberAnimation { target: bell; property: "jitter"; to: -1.5; duration: 30 }
                }
            }
            NumberAnimation { target: bell; property: "jitter"; to: 0; duration: 20 }
            PauseAnimation { duration: 900 }
        }
    }

    // How many reminders are waiting (red when one has gone off).
    Rectangle {
        visible: (root.pending + root.due) > 0
        anchors.right: parent.right
        anchors.rightMargin: 6
        anchors.verticalCenter: parent.verticalCenter
        height: Math.max(14, root.height * 0.45)
        width: Math.max(height, countText.implicitWidth + 6)
        radius: height / 2
        color: root.ringing ? sd.theme.background
             : root.due > 0 ? R.readable(sd.theme, ["red", "orange", "magenta"], 4.5) : R.readable(sd.theme, ["accent", "blue", "cyan"], 4.5)
        Text {
            id: countText
            anchors.centerIn: parent
            text: root.pending + root.due
            color: root.ringing ? sd.theme.foreground : sd.theme.background
            font.family: sd.theme.fontFamily
            font.pixelSize: parent.height * 0.7
            font.bold: true
        }
    }

    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        acceptedButtons: Qt.LeftButton | Qt.RightButton
        onClicked: (mouse) => {
            // Ringing: a click stops it and shows what went off.
            if (root.ringing) {
                root.acknowledge()
                sd.call("popup.open", { surface: "list" })
                return
            }
            // Each opens a popup below the bell; opening one closes the other.
            sd.call("popup.toggle", { surface: mouse.button === Qt.RightButton ? "list" : "add" })
        }
    }
}
