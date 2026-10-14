import QtQuick

// A square, themed button with an icon (an image path) or a text glyph.
Rectangle {
    id: button
    property string glyph: ""
    property string icon: ""
    signal clicked()

    height: width
    radius: 12
    color: area.pressed ? sd.theme.accent
         : area.containsMouse ? Qt.alpha(sd.theme.accent, 0.35)
         : Qt.alpha(sd.theme.surface, 0.9)
    Behavior on color { ColorAnimation { duration: 120 } }

    Image {
        anchors.centerIn: parent
        visible: button.icon.length > 0
        source: button.icon.length > 0 ? "file://" + button.icon : ""
        sourceSize.width: parent.width * 0.55
        sourceSize.height: parent.width * 0.55
        fillMode: Image.PreserveAspectFit
    }
    Text {
        anchors.centerIn: parent
        visible: button.icon.length === 0
        text: button.glyph
        color: sd.theme.foreground
        font.family: sd.theme.fontFamily
        font.pixelSize: parent.width * 0.4
    }
    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: button.clicked()
    }
}
