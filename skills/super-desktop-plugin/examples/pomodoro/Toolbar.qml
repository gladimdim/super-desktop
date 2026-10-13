import QtQuick

// The top-bar item: the time left (shared through sd.values); a click opens
// or closes the panel.
Rectangle {
    id: root
    radius: height / 2
    color: area.containsMouse ? sd.theme.accent : Qt.alpha(sd.theme.surface, 0.9)

    property real endsAt: sd.values.endsAt || 0
    property int remaining: 0

    Timer {
        interval: 1000; running: true; repeat: true; triggeredOnStart: true
        onTriggered: root.remaining = Math.max(0, Math.round((root.endsAt - Date.now()) / 1000))
    }

    Text {
        anchors.centerIn: parent
        text: root.remaining > 0
            ? "🍅 " + Math.floor(root.remaining / 60) + ":" + String(root.remaining % 60).padStart(2, "0")
            : "🍅 Focus"
        color: area.containsMouse ? sd.theme.background : sd.theme.foreground
        font.family: sd.theme.fontFamily
        font.pixelSize: Math.max(10, root.height * 0.42)
    }
    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: sd.call("panel.toggle", { surface: "timer" })
    }
}
