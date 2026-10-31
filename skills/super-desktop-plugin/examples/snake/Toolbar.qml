import QtQuick

// Opens and closes the game card.
Rectangle {
    radius: height / 2
    color: area.containsMouse ? sd.theme.accent : Qt.alpha(sd.theme.surface, 0.9)
    Text {
        anchors.centerIn: parent
        text: "🐍 Snake"
        color: area.containsMouse ? sd.theme.background : sd.theme.foreground
        font.family: sd.theme.fontFamily
        font.pixelSize: Math.max(10, parent.height * 0.42)
    }
    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: sd.call("card.toggle", { surface: "game" })
    }
}
