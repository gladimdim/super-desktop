import QtQuick

Rectangle {
    radius: height / 2
    color: area.containsMouse ? sd.theme.accent : Qt.alpha(sd.theme.surface, 0.9)
    Text {
        anchors.centerIn: parent
        text: ">_ Term"
        color: area.containsMouse ? sd.theme.background : sd.theme.foreground
        font.family: sd.theme.fontFamily
        font.pixelSize: Math.max(10, parent.height * 0.42)
    }
    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        // Another terminal each time; each card closes with its own ✕.
        onClicked: sd.call("card.new", { surface: "term" })
    }
}
