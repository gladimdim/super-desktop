import QtQuick

// The top-bar item: a click opens the popup below it, a second click (or a
// click anywhere else, or Esc) closes it.
Rectangle {
    id: root
    radius: height / 2
    color: area.containsMouse ? sd.theme.accent : Qt.alpha(sd.theme.surface, 0.9)

    Text {
        anchors.centerIn: parent
        text: "Agents ▾"
        color: area.containsMouse ? sd.theme.background : sd.theme.foreground
        font.family: sd.theme.fontFamily
        font.pixelSize: Math.max(10, root.height * 0.42)
    }
    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: sd.call("popup.toggle", { surface: "menu" })
    }
}
